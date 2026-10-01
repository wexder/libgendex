//! In-process OpenJev-style scorer (cargo feature `local`). For the top distinct works (title +
//! author, so all formats of one book share an answer) a small local model answers two yes/no
//! questions; answers are log-odds calibrated within the search and cached per (query, work).

mod engine;

use std::{collections::HashMap, path::Path, sync::Arc, time::Instant};

use quick_cache::sync::Cache;
use tracing::debug;

use super::{Scores, cut, ereader_fit};
use crate::{config::RankingConfig, search::Book};
use engine::{Engine, EngineState, Job};

/// Distinct works scored per search.
const MAX_WORKS: usize = 8;
/// Calibration tuned on Qwen3-1.7B: relevance falls off by e every 4 logits below the best result;
/// companions (study guides, summaries) score well above 6.5.
const RELEVANCE_TEMPERATURE: f32 = 4.0;
const COMPANION_THRESHOLD: f32 = 6.5;

pub struct LocalScorer {
    engine: Arc<Engine>,
    /// (relevance, genuine) per (query, work).
    cache: Cache<(String, String), (f32, f32)>,
}

fn sigmoid(x: f32) -> f32 {
    1.0 / (1.0 + (-x).exp())
}

fn work_key(book: &Book) -> String {
    format!(
        "{}\u{1f}{}",
        book.title.trim().to_lowercase(),
        book.author.trim().to_lowercase()
    )
}

fn describe_work(book: &Book) -> String {
    let mut s = format!("{} — {}", cut(&book.title), cut(&book.author));
    if !book.year.is_empty() {
        s.push_str(&format!(" ({})", book.year));
    }
    if !book.series.is_empty() {
        s.push_str(&format!(" [series: {}]", cut(&book.series)));
    }
    s
}

impl LocalScorer {
    pub fn new(cfg: &RankingConfig, http: reqwest::Client, data_dir: &Path) -> Self {
        Self {
            engine: Engine::start(cfg.local.clone(), data_dir.to_path_buf(), http),
            cache: Cache::new(cfg.cache_size.max(16)),
        }
    }

    pub fn describe(&self) -> String {
        match self.engine.state() {
            EngineState::Ready => "local".into(),
            EngineState::Loading => "local (loading model)".into(),
            EngineState::Failed(err) => format!("local (failed: {err})"),
        }
    }

    pub async fn score(
        &self,
        query: &str,
        candidates: &[(f32, Book)],
        deadline: Instant,
    ) -> Vec<Option<Scores>> {
        if self.engine.state() != EngineState::Ready {
            return vec![None; candidates.len()];
        }
        let mut works: Vec<(String, &Book)> = Vec::new();
        for (_, book) in candidates {
            let key = work_key(book);
            if works.len() < MAX_WORKS && !works.iter().any(|(k, _)| *k == key) {
                works.push((key, book));
            }
        }
        let cached: Option<HashMap<String, (f32, f32)>> = works
            .iter()
            .map(|(k, _)| Some((k.clone(), self.cache.get(&(query.to_string(), k.clone()))?)))
            .collect();
        let answers = match cached {
            Some(answers) => answers,
            None => self.ask(query, &works, deadline).await,
        };

        candidates
            .iter()
            .map(|(_, book)| {
                let &(relevance, genuine) = answers.get(&work_key(book))?;
                Some(Scores {
                    relevance,
                    genuine,
                    ereader: ereader_fit(book),
                    ranked_by: "local",
                })
            })
            .collect()
    }

    /// Asks two yes/no questions per work and calibrates the answers within this search:
    /// relevance relative to the best-matching work, companion-ness against a fixed threshold.
    async fn ask(
        &self,
        query: &str,
        works: &[(String, &Book)],
        deadline: Instant,
    ) -> HashMap<String, (f32, f32)> {
        let job = Job {
            context: format!("A user searched an ebook library for: \"{query}\""),
            items: works.iter().map(|(_, b)| format!("Search result: {}", describe_work(b))).collect(),
            questions: vec![
                "Is this exactly the book the user is searching for, rather than a different book that only shares some words with the search?".into(),
                "Is this a study guide, summary, analysis or other companion to another book, rather than a book in its own right?".into(),
            ],
            deadline,
        };
        let answers = match self.engine.ask(job).await {
            Ok(a) => a,
            Err(e) => {
                debug!(error = format!("{e:#}"), "local scoring skipped");
                return HashMap::new();
            }
        };
        let best = answers
            .iter()
            .flatten()
            .map(|a| a[0])
            .fold(f32::MIN, f32::max);
        let mut out = HashMap::new();
        for ((key, _), answer) in works.iter().zip(answers) {
            let Some(a) = answer else { continue };
            let genuine = 1.0 - sigmoid(a[1] - COMPANION_THRESHOLD);
            debug!(work = %key, relevance = a[0], companion = a[1], "local log-odds");
            let relevance = ((a[0] - best) / RELEVANCE_TEMPERATURE).exp();
            self.cache
                .insert((query.to_string(), key.clone()), (relevance, genuine));
            out.insert(key.clone(), (relevance, genuine));
        }
        out
    }
}
