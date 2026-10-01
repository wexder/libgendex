//! Result ordering. Every search is ordered by BM25 plus file heuristics (format, size, study-guide
//! keywords). AI re-ranking is an optional component, selected by `ranking.provider`:
//! `api` asks a remote OpenJev server, `local` runs an OpenJev-style model in-process (cargo
//! feature `local`). Any scorer failure or timeout falls back to the heuristic.

mod api;
#[cfg(feature = "local")]
mod local;

use std::{path::Path, sync::Arc, time::Instant};

use anyhow::{Result, bail};
use serde::Serialize;
use utoipa::ToSchema;

use crate::{config::RankingConfig, search::Book};

#[derive(Debug, Clone, Copy, Serialize, ToSchema)]
pub struct Scores {
    /// 0..1, how well the book matches the query.
    pub relevance: f32,
    /// 0..1, how suitable the file is for an e-reader.
    pub ereader: f32,
    /// 0..1, likelihood the file is the book itself rather than a study guide, summary or sample.
    pub genuine: f32,
    /// `api`, `local` or `heuristic`.
    pub ranked_by: &'static str,
}

enum Provider {
    None,
    Api(api::ApiScorer),
    #[cfg(feature = "local")]
    Local(local::LocalScorer),
}

pub struct Ranker {
    rerank_top: usize,
    timeout: std::time::Duration,
    provider: Provider,
}

impl Ranker {
    #[cfg_attr(not(feature = "local"), allow(unused_variables))]
    pub fn new(cfg: RankingConfig, http: reqwest::Client, data_dir: &Path) -> Result<Arc<Self>> {
        let provider = match cfg.provider.to_lowercase().as_str() {
            "none" | "" => Provider::None,
            "api" => Provider::Api(api::ApiScorer::new(&cfg, http)),
            #[cfg(feature = "local")]
            "local" => Provider::Local(local::LocalScorer::new(&cfg, http, data_dir)),
            #[cfg(not(feature = "local"))]
            "local" => bail!("ranking.provider = \"local\" needs a build with `--features local`"),
            other => bail!("unknown ranking.provider `{other}` (expected none, api or local)"),
        };
        Ok(Arc::new(Self {
            rerank_top: cfg.rerank_top,
            timeout: cfg.timeout,
            provider,
        }))
    }

    pub fn window(&self) -> usize {
        self.rerank_top
    }

    /// Provider state for the health endpoint: `none`, `api` or `local` (with a loading note).
    pub fn describe_provider(&self) -> String {
        match &self.provider {
            Provider::None => "none".into(),
            Provider::Api(_) => "api".into(),
            #[cfg(feature = "local")]
            Provider::Local(l) => l.describe(),
        }
    }

    /// Takes BM25-scored candidates; the first `rerank_top` are re-ordered by the combined score,
    /// the rest keep their BM25 order below them. With `use_ai = false` only the heuristic is used.
    pub async fn rerank(
        &self,
        query: &str,
        mut candidates: Vec<(f32, Book)>,
        use_ai: bool,
    ) -> Vec<(f32, Scores, Book)> {
        let max_bm25 = candidates
            .iter()
            .map(|c| c.0)
            .fold(f32::MIN_POSITIVE, f32::max);
        let tail = candidates.split_off(self.rerank_top.min(candidates.len()));
        let query = query.trim().to_lowercase();
        let deadline = Instant::now() + self.timeout;

        let ai: Vec<Option<Scores>> = match &self.provider {
            _ if !use_ai => vec![None; candidates.len()],
            Provider::None => vec![None; candidates.len()],
            Provider::Api(s) => s.score(&query, &candidates, deadline).await,
            #[cfg(feature = "local")]
            Provider::Local(s) => s.score(&query, &candidates, deadline).await,
        };

        let mut head: Vec<_> = candidates
            .into_iter()
            .zip(ai)
            .map(|((bm25, book), ai)| {
                let bm25n = bm25 / max_bm25;
                let scores = ai.unwrap_or_else(|| heuristic(bm25n, &book));
                (combine(bm25n, &scores), scores, book)
            })
            .collect();
        head.sort_by(|a, b| b.0.total_cmp(&a.0));

        head.extend(tail.into_iter().map(|(bm25, book)| {
            let scores = heuristic(bm25 / max_bm25, &book);
            (combine(bm25 / max_bm25, &scores), scores, book)
        }));
        head
    }
}

fn cut(s: &str) -> String {
    s.chars().take(160).collect()
}

pub fn format_score(ext: &str) -> f32 {
    match ext {
        "epub" => 1.0,
        "azw3" | "kepub" => 0.95,
        "mobi" | "azw" => 0.85,
        "fb2" => 0.75,
        "txt" | "rtf" => 0.5,
        "docx" | "doc" | "lit" => 0.4,
        "pdf" => 0.35,
        "djvu" => 0.15,
        _ => 0.2,
    }
}

/// A likely study guide or summary loses up to 40% of its score.
fn combine(bm25n: f32, s: &Scores) -> f32 {
    let base = match s.ranked_by {
        "heuristic" => 0.65 * bm25n + 0.35 * s.ereader,
        _ => 0.3 * bm25n + 0.35 * s.relevance + 0.35 * s.ereader,
    };
    base * (0.6 + 0.4 * s.genuine)
}

fn ereader_fit(book: &Book) -> f32 {
    let mb = book.filesize as f32 / 1_048_576.0;
    let size = match book.extension.as_str() {
        _ if book.filesize == 0 => 0.6,
        "epub" | "azw3" | "mobi" | "azw" | "fb2" if mb < 0.05 => 0.2,
        "epub" | "azw3" | "mobi" | "azw" | "fb2" if mb > 60.0 => 0.6,
        "pdf" if mb > 80.0 => 0.5,
        _ => 1.0,
    };
    format_score(&book.extension) * 0.8 + size * 0.2
}

fn heuristic(bm25n: f32, book: &Book) -> Scores {
    let metadata = [
        !book.author.is_empty(),
        !book.year.is_empty(),
        !book.language.is_empty(),
        !book.publisher.is_empty(),
    ]
    .iter()
    .filter(|b| **b)
    .count() as f32
        / 4.0;
    let t = book.title.to_lowercase();
    let suspicious = [
        "summary",
        "study guide",
        "sparknotes",
        "cliffsnotes",
        "sample",
        "excerpt",
    ]
    .iter()
    .any(|w| t.contains(w));
    Scores {
        relevance: bm25n,
        ereader: ereader_fit(book),
        genuine: if suspicious {
            0.2
        } else {
            0.8 + 0.2 * metadata
        },
        ranked_by: "heuristic",
    }
}
