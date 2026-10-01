mod api;
mod config;
mod ingest;
mod library;
mod rank;
mod search;

use std::sync::Arc;

use anyhow::{Context, Result};
use axum::{
    Json,
    http::StatusCode,
    routing::{any, get},
};
use tower_http::{
    compression::CompressionLayer,
    services::{ServeDir, ServeFile},
    trace::TraceLayer,
};
use tracing::info;
use tracing_subscriber::EnvFilter;

use crate::config::Config;

#[tokio::main]
async fn main() -> Result<()> {
    if std::env::args().nth(1).as_deref() == Some("openapi") {
        println!("{}", api::openapi().to_pretty_json()?);
        return Ok(());
    }

    let cfg = Arc::new(Config::load().context("loading configuration")?);
    init_logging(&cfg);
    if std::env::args().nth(1).as_deref() == Some("verify-ingest") {
        let args = std::env::args().skip(2).collect::<Vec<_>>();
        if args.len() != 5 {
            anyhow::bail!(
                "usage: libgendex verify-ingest FIRST_FTP_URL VOLUMES WORK_DIR CACHE_DIR ROWS_PER_TABLE (0 = full snapshot)"
            );
        }
        let cfg = (*cfg).clone();
        let report = tokio::task::spawn_blocking(move || {
            ingest::verify_ftp(
                cfg,
                args[0].clone(),
                args[1].parse()?,
                args[2].clone().into(),
                args[3].clone().into(),
                args[4].parse()?,
            )
        })
        .await??;
        println!("{}", serde_json::to_string_pretty(&report)?);
        return Ok(());
    }
    std::fs::create_dir_all(&cfg.paths.data_dir)?;
    std::fs::create_dir_all(&cfg.paths.library_dir)?;

    let index = Arc::new(search::SearchIndex::open(
        &cfg.paths.data_dir.join("index"),
    )?);
    info!(books = index.num_docs(), "index opened");

    let http = reqwest::Client::builder()
        .user_agent(&cfg.download.user_agent)
        .connect_timeout(cfg.download.connect_timeout)
        .build()?;
    let ranker = rank::Ranker::new(cfg.ranking.clone(), http.clone(), &cfg.paths.data_dir)?;
    let library = library::Library::new(cfg.clone(), http.clone())?;
    let indexer = ingest::Indexer::new(cfg.clone(), index.clone(), http);
    if cfg.indexer.enabled {
        tokio::spawn(indexer.clone().run_forever());
    }

    let (api, spec) = api::router(api::AppState {
        index,
        ranker,
        library,
        indexer,
    });
    let static_dir = &cfg.server.static_dir;
    let app = api
        .route("/api/openapi.json", get(move || async move { Json(spec) }))
        .route(
            "/api/{*rest}",
            any(|| async {
                (
                    StatusCode::NOT_FOUND,
                    Json(serde_json::json!({ "error": "not found" })),
                )
            }),
        )
        .fallback_service(
            ServeDir::new(static_dir).fallback(ServeFile::new(static_dir.join("index.html"))),
        )
        .layer(CompressionLayer::new())
        .layer(TraceLayer::new_for_http());

    let listener = tokio::net::TcpListener::bind(&cfg.server.bind).await?;
    info!(bind = %cfg.server.bind, ranking = %cfg.ranking.provider, "listening");
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown())
        .await?;
    Ok(())
}

fn init_logging(cfg: &Config) {
    let filter = EnvFilter::try_from_env("RUST_LOG").unwrap_or_else(|_| {
        EnvFilter::new(format!("{},tantivy=warn,tower_http=info", cfg.log.level))
    });
    let fmt = tracing_subscriber::fmt().with_env_filter(filter);
    if cfg.log.json {
        fmt.json().init();
    } else {
        fmt.init();
    }
}

async fn shutdown() {
    let ctrl_c = tokio::signal::ctrl_c();
    let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        .expect("signal handler");
    tokio::select! {
        _ = ctrl_c => {}
        _ = term.recv() => {}
    }
    info!("shutting down");
}
