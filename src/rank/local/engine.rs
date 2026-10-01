//! OpenJev-style "System One" decision engine: a small local LLM answers typed questions
//! by reading next-token probabilities over the allowed answers, never generating text.
//!
//! A job is a shared `context`, a list of `items` and yes/no `questions` asked about each item.
//! The KV cache holds the context, is extended by one item at a time and truncated back after every
//! question, so shared text is evaluated only once.

use std::{
    num::NonZeroU32,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        mpsc::{Receiver, SyncSender, sync_channel},
    },
    time::Instant,
};

use anyhow::{Context, Result, bail};
use futures_util::StreamExt;
use llama_cpp_2::{
    context::{LlamaContext, params::LlamaContextParams},
    llama_backend::LlamaBackend,
    llama_batch::LlamaBatch,
    model::{AddBos, LlamaModel, params::LlamaModelParams},
    token::LlamaToken,
};
use tokio::{io::AsyncWriteExt, sync::oneshot};
use tracing::{debug, error, info};

use crate::config::LocalRankingConfig;

const N_CTX: u32 = 1024;
const N_BATCH: usize = 128;

pub struct Job {
    /// Text shared by all items (e.g. the search query); evaluated once.
    pub context: String,
    /// Each item is evaluated once on top of the context and shared by all questions about it.
    pub items: Vec<String>,
    /// Yes/no questions asked about every item.
    pub questions: Vec<String>,
    /// Items not reached by then are answered with `None`.
    pub deadline: Instant,
}

struct Queued {
    job: Job,
    reply: oneshot::Sender<Result<Vec<Option<Vec<f32>>>>>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum EngineState {
    Loading,
    Ready,
    Failed(String),
}

pub struct Engine {
    tx: SyncSender<Queued>,
    state: Arc<Mutex<EngineState>>,
}

impl Engine {
    /// Starts loading (downloading first if needed) in the background; `ask` fails until ready.
    pub fn start(cfg: LocalRankingConfig, data_dir: PathBuf, http: reqwest::Client) -> Arc<Self> {
        let (tx, rx) = sync_channel::<Queued>(32);
        let state = Arc::new(Mutex::new(EngineState::Loading));
        let engine = Arc::new(Self {
            tx,
            state: state.clone(),
        });

        tokio::spawn(async move {
            let path = match resolve_model(&cfg, &data_dir, &http).await {
                Ok(p) => p,
                Err(e) => {
                    error!(error = format!("{e:#}"), "embedded model unavailable");
                    *state.lock().unwrap() = EngineState::Failed(format!("{e:#}"));
                    return;
                }
            };
            std::thread::Builder::new()
                .name("systemone".into())
                .spawn(move || {
                    if let Err(e) = worker(&cfg, &path, rx, &state) {
                        error!(error = format!("{e:#}"), "embedded engine stopped");
                        *state.lock().unwrap() = EngineState::Failed(format!("{e:#}"));
                    }
                })
                .expect("spawning systemone thread");
        });
        engine
    }

    pub fn state(&self) -> EngineState {
        self.state.lock().unwrap().clone()
    }

    /// Returns per item the log-odds of yes over no for each question, or `None` when cut off by the deadline.
    pub async fn ask(&self, job: Job) -> Result<Vec<Option<Vec<f32>>>> {
        if self.state() != EngineState::Ready {
            bail!("embedded engine not ready");
        }
        let (reply, rx) = oneshot::channel();
        self.tx
            .try_send(Queued { job, reply })
            .map_err(|_| anyhow::anyhow!("embedded engine busy"))?;
        rx.await.context("embedded engine dropped the request")?
    }
}

async fn resolve_model(
    cfg: &LocalRankingConfig,
    data_dir: &Path,
    http: &reqwest::Client,
) -> Result<PathBuf> {
    if let Some(p) = &cfg.model_path {
        return Ok(p.clone());
    }
    let name = cfg
        .model_url
        .rsplit('/')
        .next()
        .filter(|n| !n.is_empty())
        .unwrap_or("model.gguf");
    let path = data_dir.join("models").join(name);
    if path.exists() {
        return Ok(path);
    }
    tokio::fs::create_dir_all(path.parent().unwrap()).await?;
    info!(url = %cfg.model_url, "downloading embedded model");
    let part = path.with_extension("gguf.part");
    let resp = http.get(&cfg.model_url).send().await?.error_for_status()?;
    let total = resp.content_length().unwrap_or(0);
    let mut file = tokio::fs::File::create(&part).await?;
    let mut stream = resp.bytes_stream();
    let (mut done, mut logged) = (0u64, 0u64);
    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        file.write_all(&chunk).await?;
        done += chunk.len() as u64;
        if done - logged > 50 << 20 {
            logged = done;
            info!(
                mb = done >> 20,
                total_mb = total >> 20,
                "downloading embedded model"
            );
        }
    }
    file.flush().await?;
    tokio::fs::rename(&part, &path).await?;
    info!(path = %path.display(), "embedded model downloaded");
    Ok(path)
}

/// Strips chat-template control sequences so metadata can't break out of the prompt.
fn clean(s: &str) -> String {
    s.replace("<|", "< |").replace("|>", "| >")
}

struct Runner<'a> {
    model: &'a LlamaModel,
    ctx: LlamaContext<'a>,
    batch: LlamaBatch<'a>,
    yes: Vec<LlamaToken>,
    no: Vec<LlamaToken>,
}

impl Runner<'_> {
    fn tokens(&self, text: &str, bos: bool) -> Result<Vec<LlamaToken>> {
        Ok(self
            .model
            .str_to_token(text, if bos { AddBos::Always } else { AddBos::Never })?)
    }

    /// Appends tokens at `start`; returns the new length.
    fn feed(&mut self, tokens: &[LlamaToken], start: usize, want_logits: bool) -> Result<usize> {
        if start + tokens.len() > N_CTX as usize {
            bail!("prompt exceeds context window");
        }
        for (c, chunk) in tokens.chunks(N_BATCH).enumerate() {
            self.batch.clear();
            let last_chunk = (c + 1) * N_BATCH >= tokens.len();
            for (i, t) in chunk.iter().enumerate() {
                let logits = want_logits && last_chunk && i == chunk.len() - 1;
                self.batch
                    .add(*t, (start + c * N_BATCH + i) as i32, &[0], logits)?;
            }
            self.ctx.decode(&mut self.batch)?;
        }
        Ok(start + tokens.len())
    }

    fn truncate(&mut self, len: usize) -> Result<()> {
        self.ctx
            .clear_kv_cache_seq(Some(0), Some(len as u32), None)?;
        Ok(())
    }

    /// log P(yes) - log P(no) at the last decoded position, summed over the token variants.
    fn log_odds(&self) -> f32 {
        let logits = self.ctx.get_logits_ith(self.batch.n_tokens() - 1);
        let lse = |ts: &[LlamaToken]| {
            let max = ts
                .iter()
                .map(|t| logits[t.0 as usize])
                .fold(f32::MIN, f32::max);
            max + ts
                .iter()
                .map(|t| (logits[t.0 as usize] - max).exp())
                .sum::<f32>()
                .ln()
        };
        lse(&self.yes) - lse(&self.no)
    }

    /// Returns, per item, the log-odds of "yes" over "no" for each question.
    fn run(&mut self, job: &Job) -> Result<Vec<Option<Vec<f32>>>> {
        self.ctx.clear_kv_cache();
        let context = format!(
            "<|im_start|>system\nYou judge search results in an ebook library. Answer only yes or no.<|im_end|>\n<|im_start|>user\n{}\n",
            clean(&job.context)
        );
        let context = self.tokens(&context, true)?;
        let base = self.feed(&context, 0, false)?;
        let questions = job
            .questions
            .iter()
            .map(|q| {
                let q = format!("{} Answer yes or no.<|im_end|>\n<|im_start|>assistant\n<think>\n\n</think>\n\n", clean(q));
                self.tokens(&q, false)
            })
            .collect::<Result<Vec<_>>>()?;

        let mut out = Vec::with_capacity(job.items.len());
        for item in &job.items {
            if Instant::now() >= job.deadline {
                out.push(None);
                continue;
            }
            let item = self.tokens(&format!("\n{}\n", clean(item)), false)?;
            let with_item = self.feed(&item, base, false)?;
            let mut answers = Vec::with_capacity(questions.len());
            for q in &questions {
                self.feed(q, with_item, true)?;
                answers.push(self.log_odds());
                self.truncate(with_item)?;
            }
            self.truncate(base)?;
            out.push(Some(answers));
        }
        Ok(out)
    }
}

fn worker(
    cfg: &LocalRankingConfig,
    path: &Path,
    rx: Receiver<Queued>,
    state: &Mutex<EngineState>,
) -> Result<()> {
    llama_cpp_2::send_logs_to_tracing(llama_cpp_2::LogOptions::default().with_logs_enabled(false));
    let backend = LlamaBackend::init()?;
    let model = LlamaModel::load_from_file(&backend, path, &LlamaModelParams::default())
        .with_context(|| format!("loading {}", path.display()))?;
    let threads = match cfg.threads {
        0 => std::thread::available_parallelism()
            .map_or(4, |n| n.get() / 2)
            .clamp(1, 6),
        n => n,
    } as i32;
    let params = LlamaContextParams::default()
        .with_n_ctx(NonZeroU32::new(N_CTX))
        .with_n_batch(N_BATCH as u32)
        .with_n_ubatch(N_BATCH as u32)
        .with_n_threads(threads)
        .with_n_threads_batch(threads);
    let ctx = model.new_context(&backend, params)?;

    let single = |words: &[&str]| -> Result<Vec<LlamaToken>> {
        let mut out = Vec::new();
        for w in words {
            let t = model.str_to_token(w, AddBos::Never)?;
            if t.len() == 1 {
                out.push(t[0]);
            }
        }
        if out.is_empty() {
            bail!("model has no single-token {words:?}");
        }
        Ok(out)
    };
    let yes = single(&["yes", "Yes"])?;
    let no = single(&["no", "No"])?;
    let mut runner = Runner {
        model: &model,
        ctx,
        batch: LlamaBatch::new(N_BATCH, 1),
        yes,
        no,
    };
    info!(model = %path.display(), threads, "embedded engine ready");
    *state.lock().unwrap() = EngineState::Ready;

    while let Ok(Queued { job, reply }) = rx.recv() {
        if reply.is_closed() {
            continue;
        }
        let started = Instant::now();
        let result = runner.run(&job);
        let answered = result
            .as_ref()
            .map(|r| r.iter().filter(|x| x.is_some()).count())
            .unwrap_or(0);
        debug!(
            ms = started.elapsed().as_millis() as u64,
            items = job.items.len(),
            answered,
            "systemone job"
        );
        let _ = reply.send(result);
    }
    Ok(())
}
