//! Isolated production-path verification used by the verify-ingest command.

use super::{build_myisam_archive, ftp_range};
use crate::{config::Config, search::SearchIndex};
use anyhow::{Result, bail};
use std::{path::PathBuf, sync::Arc, time::Instant};

/// Runs a bounded live bootstrap through the same decoder, staging and index writer as the
/// service, then verifies document lookup and full-text search. Uses a dedicated work directory.
pub fn verify_ftp(
    mut cfg: Config,
    first: String,
    volumes: usize,
    work: PathBuf,
    cache_path: PathBuf,
    rows: u64,
) -> Result<serde_json::Value> {
    if volumes == 0 {
        bail!("at least one volume is required");
    }
    let started = Instant::now();
    cfg.paths.data_dir = work;
    std::fs::create_dir_all(&cfg.paths.data_dir)?;
    let cache = Arc::new(ftp_range::DiskRangeCache::open(
        cache_path,
        cfg.indexer.ftp_cache_mb.saturating_mul(1024 * 1024),
    )?);
    let urls = (1..=volumes)
        .map(|n| {
            if n == 1 {
                first.clone()
            } else {
                first.replace("part001.rar", &format!("part{n:03}.rar"))
            }
        })
        .collect::<Vec<_>>();
    if volumes > 1 && !first.contains("part001.rar") {
        bail!("multi-volume verification requires a part001.rar URL");
    }
    let index = SearchIndex::open(&cfg.paths.data_dir.join("index"))?;
    let topics = cfg
        .indexer
        .sources
        .first()
        .map(|s| s.topics.clone())
        .unwrap_or_default();
    let limit = (rows > 0).then_some(rows);
    let (books, max_ids) =
        build_myisam_archive(&cfg, &index, "verification", &topics, limit, |name| {
            ftp_range::stream_named_member(urls.clone(), cache.clone(), name)
        })?;
    index.reload()?;
    if books == 0 {
        bail!("sample produced no joined books; increase the per-table row sample");
    }
    let sample = index
        .search("*", None, None, 1)?
        .into_iter()
        .next()
        .ok_or_else(|| anyhow::anyhow!("committed index returned no documents"))?
        .1;
    let lookup = index
        .get(&sample.md5)?
        .ok_or_else(|| anyhow::anyhow!("indexed document could not be retrieved by MD5"))?;
    if lookup.title != sample.title {
        bail!("document lookup returned inconsistent metadata");
    }
    let query = sample
        .title
        .split_whitespace()
        .take(4)
        .collect::<Vec<_>>()
        .join(" ");
    let hits = index.search(&query, None, None, 20)?;
    if hits.is_empty() {
        bail!("full-text search did not find the indexed title");
    }
    let stage_removed = !cfg
        .paths
        .data_dir
        .join("stage-verification.sqlite")
        .exists();
    if !stage_removed {
        bail!("temporary staging database was not removed");
    }
    Ok(
        serde_json::json!({ "complete_snapshot": limit.is_none(), "rows_per_table_limit": limit,
        "books": books, "topics": topics, "max_file_ids": max_ids,
        "query": query, "search_hits": hits.len(), "sample": sample,
        "stage_removed": stage_removed, "ftp": cache.stats(), "elapsed_seconds": started.elapsed().as_secs_f64() }),
    )
}
