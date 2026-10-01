//! Remote scorer: each candidate is sent to an OpenJev / Jev compatible `/v1/systemone` endpoint.

use std::{
    sync::Mutex,
    time::{Duration, Instant},
};

use futures_util::{StreamExt, stream};
use quick_cache::sync::Cache;
use serde_json::{Value, json};
use tracing::{debug, warn};

use super::{Scores, cut};
use crate::{
    config::{ApiRankingConfig, RankingConfig},
    search::Book,
};

const RELEVANCE_LEVELS: [&str; 4] = [
    "unrelated",
    "loosely related",
    "good match",
    "exactly what was searched for",
];
const EREADER_LEVELS: [&str; 5] = [
    "unusable on an e-reader",
    "poor (fixed layout scan or odd format)",
    "acceptable",
    "good",
    "excellent (clean reflowable ebook)",
];
const EREADER_Q: &str = "How well suited is this file for reading on an e-ink e-reader such as a Kindle or Kobo? Reflowable formats (EPUB, AZW3, MOBI, FB2) are best, scanned PDFs and DJVU are poor, and a sensible file size matters.";
const GENUINE_Q: &str = "Is this likely a genuine, complete edition of the book rather than a summary, study guide, sample or corrupted file?";

pub struct ApiScorer {
    cfg: ApiRankingConfig,
    timeout: Duration,
    http: reqwest::Client,
    /// Answers per (query, md5).
    cache: Cache<(String, String), Scores>,
    /// Set after a failed call so a down or slow server is skipped for a while.
    backoff_until: Mutex<Option<Instant>>,
}

fn describe_file(book: &Book) -> String {
    format!(
        "Book file:\ntitle: {}\nauthor: {}\nseries: {}\npublisher: {}\nyear: {}\nlanguage: {}\nformat: {}\nfile size: {:.1} MB\npages: {}",
        cut(&book.title),
        cut(&book.author),
        cut(&book.series),
        cut(&book.publisher),
        book.year,
        book.language,
        book.extension.to_uppercase(),
        book.filesize as f64 / 1_048_576.0,
        if book.pages > 0 {
            book.pages.to_string()
        } else {
            "unknown".into()
        },
    )
}

impl ApiScorer {
    pub fn new(cfg: &RankingConfig, http: reqwest::Client) -> Self {
        Self {
            cfg: cfg.api.clone(),
            timeout: cfg.timeout,
            http,
            cache: Cache::new(cfg.cache_size.max(16)),
            backoff_until: Mutex::default(),
        }
    }

    pub async fn score(
        &self,
        query: &str,
        candidates: &[(f32, Book)],
        deadline: Instant,
    ) -> Vec<Option<Scores>> {
        if self.backing_off() {
            return vec![None; candidates.len()];
        }
        let deadline = tokio::time::Instant::from_std(deadline);
        let books: Vec<Book> = candidates.iter().map(|(_, b)| b.clone()).collect();
        stream::iter(books)
            .map(|book| async move {
                match tokio::time::timeout_at(deadline, self.ask(query, &book)).await {
                    Ok(s) => s,
                    Err(_) => {
                        self.fail(&"deadline exceeded");
                        None
                    }
                }
            })
            .buffered(self.cfg.concurrency.max(1))
            .collect()
            .await
    }

    fn backing_off(&self) -> bool {
        let until = *self.backoff_until.lock().unwrap();
        until.is_some_and(|t| Instant::now() < t)
    }

    fn fail(&self, e: &dyn std::fmt::Display) {
        let mut until = self.backoff_until.lock().unwrap();
        if !until.is_some_and(|t| Instant::now() < t) {
            warn!(error = %e, "ranking api unavailable, using heuristic ranking for 30s");
            *until = Some(Instant::now() + Duration::from_secs(30));
        }
    }

    async fn ask(&self, query: &str, book: &Book) -> Option<Scores> {
        let key = (query.to_string(), book.md5.clone());
        if let Some(s) = self.cache.get(&key) {
            return Some(s);
        }
        let body = json!({
            "model": self.cfg.model,
            "state": describe_file(book),
            "questions": {
                "relevance": {
                    "type": "score",
                    "instructions": format!("The user searched for \"{query}\". How well does this file match that search?"),
                    "criteria": RELEVANCE_LEVELS,
                },
                "ereader": { "type": "score", "instructions": EREADER_Q, "criteria": EREADER_LEVELS },
                "genuine": { "type": "noul", "instructions": GENUINE_Q },
            }
        });
        let url = format!("{}/v1/systemone", self.cfg.url.trim_end_matches('/'));
        let mut req = self.http.post(&url).json(&body).timeout(self.timeout);
        if let Some(k) = &self.cfg.api_key {
            req = req.bearer_auth(k);
        }
        let resp: Value = match req.send().await.and_then(|r| r.error_for_status()) {
            Ok(r) => match r.json().await {
                Ok(v) => v,
                Err(e) => {
                    warn!(error = %e, "ranking api returned invalid json");
                    return None;
                }
            },
            Err(e) => {
                self.fail(&e);
                return None;
            }
        };
        let answers = resp.get("answers")?;
        let scores = Scores {
            relevance: score_answer(answers.get("relevance")?, RELEVANCE_LEVELS.len())?,
            ereader: score_answer(answers.get("ereader")?, EREADER_LEVELS.len())?,
            genuine: answers.get("genuine")?.get("noul")?.as_f64()? as f32,
            ranked_by: "api",
        };
        debug!(md5 = %book.md5, ?scores, "api scores");
        self.cache.insert(key, scores);
        Some(scores)
    }
}

/// Normalizes a `score` answer to 0..1, preferring the probability distribution when present.
fn score_answer(a: &Value, levels: usize) -> Option<f32> {
    let max = (levels - 1).max(1) as f64;
    if let Some(probs) = a.get("probabilities") {
        let probs: Vec<f64> = match probs {
            Value::Array(v) => v.iter().filter_map(Value::as_f64).collect(),
            Value::Object(m) => m.values().filter_map(Value::as_f64).collect(),
            _ => Vec::new(),
        };
        let total: f64 = probs.iter().sum();
        if probs.len() == levels && total > 0.0 {
            let expected: f64 = probs.iter().enumerate().map(|(i, p)| i as f64 * p).sum();
            return Some((expected / total / max) as f32);
        }
    }
    let s = a.get("score")?.as_f64()?;
    // The expected level may be 0- or 1-based depending on the server.
    let s = if s > max { s - 1.0 } else { s };
    Some((s / max).clamp(0.0, 1.0) as f32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn score_normalization() {
        let a = json!({"score": 2.0, "probabilities": [0.0, 0.0, 1.0]});
        assert_eq!(score_answer(&a, 3), Some(1.0));
        let a = json!({"score": 1.0});
        assert_eq!(score_answer(&a, 3), Some(0.5));
    }
}
