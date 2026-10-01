//! Incremental updates from the libgen.li `json.php` API. Files modified on a day are listed
//! (`mode=modified`), the editions of those files are looked up by id, and
//! each affected file is re-indexed (or removed when hidden, broken or no longer an ebook).

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use anyhow::{Result, bail};
use futures_util::StreamExt;
use serde_json::{Map, Value};
use tracing::{info, warn};

use super::Indexer;
use crate::search::Book;

type Records = Map<String, Value>;

const FILE_FIELDS: &str = "md5,extension,filesize,pages,visible,broken,libgen_topic";
const EDITION_FIELDS: &str = "title,author,year,publisher,series_name,visible";

fn text(v: &Value, key: &str) -> String {
    match v.get(key) {
        Some(Value::String(s)) => s.trim().to_string(),
        Some(Value::Number(n)) => n.to_string(),
        _ => String::new(),
    }
}

fn number(v: &Value, key: &str) -> u64 {
    text(v, key).parse().unwrap_or(0)
}

/// Ids of a related sub-array such as a file's `editions` (`e_id`) or an edition's `files` (`f_id`).
fn related_ids(v: &Value, array: &str, id: &str) -> Vec<String> {
    match v.get(array) {
        Some(Value::Object(m)) => m
            .values()
            .map(|r| text(r, id))
            .filter(|s| !s.is_empty())
            .collect(),
        _ => Vec::new(),
    }
}

/// Values of an additional descriptor key (e.g. 101 language) from an edition's `add` sub-array.
fn descriptor(edition: &Value, key: i64) -> Vec<String> {
    match edition.get("add") {
        Some(Value::Object(m)) => m
            .values()
            .filter(|a| number(a, "key") as i64 == key)
            .map(|a| text(a, "value"))
            .filter(|s| !s.is_empty())
            .collect(),
        _ => Vec::new(),
    }
}

/// Outcome of one sync run.
pub struct SyncResult {
    pub upserts: Vec<Book>,
    pub removals: Vec<String>,
}

impl Indexer {
    /// One GET against the first responding endpoint; retried across mirrors with backoff.
    async fn api_get(&self, query: &str) -> Result<Records> {
        let cfg = &self.cfg.indexer.api;
        let mut last_err = anyhow::anyhow!("no API urls configured");
        for attempt in 0..cfg.urls.len() * 3 {
            let url = format!("{}?{query}", cfg.urls[attempt % cfg.urls.len()]);
            tokio::time::sleep(cfg.request_delay * (1 + attempt as u32 * 5)).await;
            let result = async {
                let body = self
                    .http
                    .get(&url)
                    .timeout(cfg.timeout)
                    .send()
                    .await?
                    .error_for_status()?
                    .text()
                    .await?;
                match serde_json::from_str::<Value>(&body) {
                    Ok(Value::Object(m)) => Ok(m),
                    Ok(Value::Array(a)) if a.is_empty() => Ok(Records::new()),
                    _ => bail!(
                        "unexpected API response: {}",
                        body.chars().take(120).collect::<String>()
                    ),
                }
            }
            .await;
            match result {
                Ok(records) => return Ok(records),
                Err(e) => {
                    warn!(%url, attempt = attempt + 1, error = format!("{e:#}"), "API request failed");
                    last_err = e;
                }
            }
        }
        Err(last_err)
    }

    /// All records of `object` modified in `range` (`timefirst=…[&timelast=…]`), paged.
    async fn api_modified(&self, object: &str, range: &str, fields: &str) -> Result<Records> {
        let size = self.cfg.indexer.api.page_size.clamp(1, 10_000);
        let mut all = Records::new();
        for offset in (0..).step_by(size) {
            // `limit1` alone means "the first N"; `limit1=0&limit2=N` would return everything.
            let limit = match offset {
                0 => format!("limit1={size}"),
                _ => format!("limit1={offset}&limit2={size}"),
            };
            let page = self
                .api_get(&format!(
                    "object={object}&mode=modified&{range}&fields={fields}&{limit}"
                ))
                .await?;
            let n = page.len();
            let before = all.len();
            all.extend(page);
            if n < size {
                break;
            }
            // Offset paging is unreliable in this API (later offsets can repeat a page), so stop as soon
            // as a full page brings nothing new rather than looping forever.
            if all.len() == before {
                warn!(
                    object,
                    range,
                    records = all.len(),
                    "API could not page further; changes of this day may be incomplete"
                );
                break;
            }
        }
        Ok(all)
    }

    /// Looks records up by id in batches, `api.concurrency` requests at a time.
    async fn api_by_ids(&self, object: &str, ids: &[String], fields: &str) -> Result<Records> {
        let cfg = &self.cfg.indexer.api;
        let requests: Vec<_> = ids
            .chunks(cfg.batch_size.max(1))
            .map(|chunk| {
                Box::pin(self.api_get_owned(format!(
                    "object={object}&ids={}&fields={fields}",
                    chunk.join(",")
                )))
            })
            .collect();
        let pages: Vec<Result<Records>> = futures_util::stream::iter(requests)
            .buffer_unordered(cfg.concurrency.max(1))
            .collect()
            .await;
        let mut all = Records::new();
        for page in pages {
            all.extend(page?);
        }
        Ok(all)
    }

    async fn api_get_owned(&self, query: String) -> Result<Records> {
        self.api_get(&query).await
    }

    /// Changes to files on one day (topic letter -> source name): removals and edits of existing files.
    /// The API only answers ranges of about a day, `to` must be omitted for today (a future
    /// `timelast` returns nothing), and deep pages are capped, so new files come from `api_new_files`.
    pub(super) async fn api_changes(
        &self,
        from: &str,
        to: Option<&str>,
        topics: &HashMap<String, String>,
    ) -> Result<SyncResult> {
        let range = match to {
            Some(to) => format!("timefirst={from}&timelast={to}"),
            None => format!("timefirst={from}"),
        };
        let files = self.api_modified("f", &range, FILE_FIELDS).await?;
        info!(day = from, files = files.len(), "API changes listed");
        self.process_files(files, topics, from).await
    }

    /// Files of one topic with per-topic ids (`fiction_id` / `libgen_id`) in `[start, start + page)`,
    /// and the highest such id among them. This is the keyset stream of new files: libgen.li's
    /// per-topic ids are the libgen.rs dump IDs, so it continues exactly where a dump ended.
    pub(super) async fn api_new_files(
        &self,
        topic: &str,
        start: u64,
    ) -> Result<(Records, Option<u64>)> {
        let end = start + self.cfg.indexer.api.page_size.clamp(1, 10_000) as u64 - 1;
        let files = self
            .api_get(&format!("object=f&topic={topic}&id_start={start}&id_end={end}&fields={FILE_FIELDS},libgen_id,fiction_id"))
            .await?;
        let id_field = if topic == "f" {
            "fiction_id"
        } else {
            "libgen_id"
        };
        let max = files.values().map(|f| number(f, id_field)).max();
        Ok((files, max))
    }

    /// Classifies files (removed, already indexed, new), looks up the editions of new ones and builds
    /// their index documents.
    pub(super) async fn process_files(
        &self,
        mut files: Records,
        topics: &HashMap<String, String>,
        label: &str,
    ) -> Result<SyncResult> {
        let extensions: HashSet<String> = self
            .cfg
            .indexer
            .extensions
            .iter()
            .map(|e| e.to_lowercase())
            .collect();
        let lang_key = self.cfg.indexer.language_key.unwrap_or(101);
        let isbn_key = self.cfg.indexer.isbn_key.unwrap_or(505);

        files.retain(|_, f| topics.contains_key(&text(f, "libgen_topic")));

        // Hidden, broken or non-ebook files are removed. Files already in the index keep the metadata
        // they have (most "modified" files are bookkeeping touches), so only new files need their
        // editions looked up, which is what makes a multi-month catch-up feasible.
        let mut result = SyncResult {
            upserts: Vec::new(),
            removals: Vec::new(),
        };
        let mut unchanged = 0usize;
        files.retain(|_, f| {
            let md5 = text(f, "md5").to_lowercase();
            let ext = text(f, "extension").to_lowercase();
            if md5.len() != 32 {
                return false;
            }
            if !text(f, "visible").is_empty()
                || text(f, "broken") == "Y"
                || !extensions.contains(&ext)
            {
                result.removals.push(md5);
                return false;
            }
            if self.index.contains(&md5) {
                unchanged += 1;
                return false;
            }
            true
        });
        info!(
            batch = label,
            new = files.len(),
            unchanged,
            removed = result.removals.len(),
            "API changes classified"
        );

        let edition_ids: BTreeSet<String> = files
            .values()
            .flat_map(|f| related_ids(f, "editions", "e_id"))
            .collect();
        let editions = self
            .api_by_ids(
                "e",
                &edition_ids.into_iter().collect::<Vec<_>>(),
                &format!("{EDITION_FIELDS}&addkeys={lang_key},{isbn_key}"),
            )
            .await?;
        // Numeric order so a file takes its lowest edition id, as in the dump import.
        let editions: BTreeMap<u64, &Value> = editions
            .iter()
            .filter(|(_, e)| text(e, "visible").is_empty() && !text(e, "title").is_empty())
            .filter_map(|(id, e)| Some((id.parse().ok()?, e)))
            .collect();

        for f in files.values() {
            let md5 = text(f, "md5").to_lowercase();
            let ext = text(f, "extension").to_lowercase();
            let mut edition_ids: Vec<u64> = related_ids(f, "editions", "e_id")
                .iter()
                .filter_map(|id| id.parse().ok())
                .collect();
            edition_ids.sort_unstable();
            let edition = edition_ids.iter().find_map(|id| editions.get(id));
            match edition {
                Some(e) => result.upserts.push(Book {
                    md5,
                    title: text(e, "title"),
                    author: text(e, "author"),
                    series: text(e, "series_name"),
                    publisher: text(e, "publisher"),
                    year: text(e, "year"),
                    language: descriptor(e, lang_key)
                        .into_iter()
                        .next()
                        .unwrap_or_default(),
                    extension: ext,
                    filesize: number(f, "filesize"),
                    pages: number(f, "pages"),
                    isbn: descriptor(e, isbn_key)
                        .iter()
                        .map(|i| i.replace(['-', ' '], ""))
                        .collect(),
                    source: topics[&text(f, "libgen_topic")].clone(),
                }),
                None => result.removals.push(md5),
            }
        }
        Ok(result)
    }
}
