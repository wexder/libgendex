use std::{collections::HashSet, sync::Arc, time::Instant};

use axum::{
    Json,
    body::Body,
    extract::{Path, Query, State},
    http::{HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
};
use serde::{Deserialize, Serialize};
use tracing::{error, info};
use utoipa::{IntoParams, OpenApi, ToSchema};
use utoipa_axum::{router::OpenApiRouter, routes};

use crate::{
    ingest::{IndexStatus, Indexer},
    library::{self, DownloadJob, Library},
    rank::{Ranker, Scores},
    search::{Book, SearchIndex},
};

#[derive(Clone)]
pub struct AppState {
    pub index: Arc<SearchIndex>,
    pub ranker: Arc<Ranker>,
    pub library: Arc<Library>,
    pub indexer: Arc<Indexer>,
}

#[derive(OpenApi)]
#[openapi(info(title = "bookjev", description = "Library Genesis index search API"))]
pub struct ApiDoc;

fn api_router() -> OpenApiRouter<AppState> {
    OpenApiRouter::with_openapi(ApiDoc::openapi())
        .routes(routes!(search))
        .routes(routes!(get_book))
        .routes(routes!(download_book))
        .routes(routes!(save_book))
        .routes(routes!(list_downloads))
        .routes(routes!(index_status))
        .routes(routes!(refresh_index))
        .routes(routes!(health))
}

pub fn router(state: AppState) -> (axum::Router, utoipa::openapi::OpenApi) {
    api_router().with_state(state).split_for_parts()
}

pub fn openapi() -> utoipa::openapi::OpenApi {
    api_router().into_openapi()
}

#[derive(Serialize, ToSchema)]
pub struct ErrorBody {
    pub error: String,
}

pub struct ApiError(StatusCode, String);

impl ApiError {
    fn not_found(what: &str) -> Self {
        Self(StatusCode::NOT_FOUND, format!("{what} not found"))
    }
}

impl From<anyhow::Error> for ApiError {
    fn from(e: anyhow::Error) -> Self {
        error!(error = format!("{e:#}"), "request failed");
        Self(StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}"))
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.0, Json(ErrorBody { error: self.1 })).into_response()
    }
}

type ApiResult<T> = Result<T, ApiError>;

#[derive(Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct SearchParams {
    /// Free text: title, author, series, publisher or ISBN.
    pub q: String,
    /// Restrict to a file extension, e.g. `epub`.
    pub ext: Option<String>,
    /// Restrict to languages (ISO 639-2 codes as stored by LibGen), comma-separated, e.g. `ger,deu`.
    pub lang: Option<String>,
    /// Maximum results (default 40, max 100).
    pub limit: Option<usize>,
    /// Re-rank with the AI scorer (default true); `false` returns instantly with heuristic ranking.
    pub ai: Option<bool>,
}

#[derive(Serialize, ToSchema)]
pub struct SearchResult {
    pub book: Book,
    pub scores: Scores,
    /// Final combined score used for ordering.
    pub score: f32,
    pub in_library: bool,
}

#[derive(Serialize, ToSchema)]
pub struct SearchResponse {
    pub query: String,
    pub took_ms: u64,
    pub results: Vec<SearchResult>,
}

#[utoipa::path(get, path = "/api/search", params(SearchParams), tag = "search",
    responses((status = 200, body = SearchResponse), (status = 400, body = ErrorBody)))]
async fn search(
    State(s): State<AppState>,
    Query(p): Query<SearchParams>,
) -> ApiResult<Json<SearchResponse>> {
    let started = Instant::now();
    let q = p.q.trim().to_string();
    if q.is_empty() {
        return Err(ApiError(
            StatusCode::BAD_REQUEST,
            "query must not be empty".into(),
        ));
    }
    let limit = p.limit.unwrap_or(40).clamp(1, 100);
    let fetch = limit.max(s.ranker.window());
    let index = s.index.clone();
    let (q2, ext, lang) = (q.clone(), p.ext.clone(), p.lang.clone());
    let hits = tokio::task::spawn_blocking(move || {
        index.search(&q2, ext.as_deref(), lang.as_deref(), fetch)
    })
    .await
    .map_err(anyhow::Error::from)??;

    let mut seen = HashSet::new();
    let hits: Vec<_> = hits
        .into_iter()
        .filter(|(_, b)| seen.insert(b.md5.clone()))
        .collect();
    let ranked = s.ranker.rerank(&q, hits, p.ai.unwrap_or(true)).await;
    let results: Vec<_> = ranked
        .into_iter()
        .take(limit)
        .map(|(score, scores, book)| SearchResult {
            in_library: s.library.exists(&book),
            book,
            scores,
            score,
        })
        .collect();
    let took_ms = started.elapsed().as_millis() as u64;
    info!(query = %q, results = results.len(), took_ms, "search");
    Ok(Json(SearchResponse {
        query: q,
        took_ms,
        results,
    }))
}

async fn find_book(s: &AppState, md5: &str) -> ApiResult<Book> {
    let index = s.index.clone();
    let md5 = md5.to_string();
    tokio::task::spawn_blocking(move || index.get(&md5))
        .await
        .map_err(anyhow::Error::from)??
        .ok_or_else(|| ApiError::not_found("book"))
}

#[utoipa::path(get, path = "/api/books/{md5}", tag = "books",
    params(("md5" = String, Path, description = "File MD5")),
    responses((status = 200, body = Book), (status = 404, body = ErrorBody)))]
async fn get_book(State(s): State<AppState>, Path(md5): Path<String>) -> ApiResult<Json<Book>> {
    Ok(Json(find_book(&s, &md5).await?))
}

/// Streams the file from a mirror to the browser.
#[utoipa::path(get, path = "/api/books/{md5}/file", tag = "books",
    params(("md5" = String, Path, description = "File MD5")),
    responses((status = 200, description = "Book file", content_type = "application/octet-stream"),
              (status = 404, body = ErrorBody), (status = 502, body = ErrorBody)))]
async fn download_book(State(s): State<AppState>, Path(md5): Path<String>) -> ApiResult<Response> {
    let book = find_book(&s, &md5).await?;
    let remote = s.library.open_remote(&book.md5).await.map_err(|e| {
        ApiError(
            StatusCode::BAD_GATEWAY,
            format!("no mirror could serve the file: {e:#}"),
        )
    })?;
    info!(md5 = %book.md5, title = %book.title, "streaming download to browser");
    let name = library::download_name(&book);
    let ascii: String = name
        .chars()
        .map(|c| if c.is_ascii() && c != '"' { c } else { '_' })
        .collect();
    let disposition = format!(
        "attachment; filename=\"{ascii}\"; filename*=UTF-8''{}",
        utf8_percent(&name)
    );
    let len = remote.content_length();
    let mut resp = Response::new(Body::from_stream(remote.bytes_stream()));
    let h = resp.headers_mut();
    if let Some(len) = len {
        h.insert(header::CONTENT_LENGTH, HeaderValue::from(len));
    }
    h.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/octet-stream"),
    );
    if let Ok(v) = HeaderValue::from_str(&disposition) {
        h.insert(header::CONTENT_DISPOSITION, v);
    }
    Ok(resp)
}

fn utf8_percent(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

/// Downloads the file into the server library under `<author>/`.
#[utoipa::path(post, path = "/api/books/{md5}/save", tag = "books",
    params(("md5" = String, Path, description = "File MD5")),
    responses((status = 202, body = DownloadJob), (status = 404, body = ErrorBody)))]
async fn save_book(
    State(s): State<AppState>,
    Path(md5): Path<String>,
) -> ApiResult<(StatusCode, Json<DownloadJob>)> {
    let book = find_book(&s, &md5).await?;
    info!(md5 = %book.md5, title = %book.title, "queued library download");
    Ok((StatusCode::ACCEPTED, Json(s.library.enqueue(book).await)))
}

#[utoipa::path(get, path = "/api/downloads", tag = "books", responses((status = 200, body = Vec<DownloadJob>)))]
async fn list_downloads(State(s): State<AppState>) -> Json<Vec<DownloadJob>> {
    Json(s.library.jobs().await)
}

#[utoipa::path(get, path = "/api/index/status", tag = "index", responses((status = 200, body = IndexStatus)))]
async fn index_status(State(s): State<AppState>) -> Json<IndexStatus> {
    Json(s.indexer.status().await)
}

#[derive(Serialize, ToSchema)]
pub struct RefreshResponse {
    /// False when a refresh is already running.
    pub started: bool,
}

#[utoipa::path(post, path = "/api/index/refresh", tag = "index", responses((status = 202, body = RefreshResponse)))]
async fn refresh_index(State(s): State<AppState>) -> (StatusCode, Json<RefreshResponse>) {
    let started = s.indexer.trigger().await;
    (StatusCode::ACCEPTED, Json(RefreshResponse { started }))
}

#[derive(Serialize, ToSchema)]
pub struct Health {
    pub ok: bool,
    pub books: u64,
    pub ranking: String,
}

#[utoipa::path(get, path = "/api/health", tag = "meta", responses((status = 200, body = Health)))]
async fn health(State(s): State<AppState>) -> Json<Health> {
    Json(Health {
        ok: true,
        books: s.index.num_docs(),
        ranking: s.ranker.describe_provider(),
    })
}
