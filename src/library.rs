use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::Arc,
};

use anyhow::{Context, Result, bail};
use futures_util::StreamExt;
use regex::Regex;
use serde::Serialize;
use tokio::{
    io::AsyncWriteExt,
    sync::{Mutex, Semaphore},
};
use tracing::{info, warn};
use utoipa::ToSchema;

use crate::{config::Config, search::Book};

#[derive(Debug, Clone, Copy, PartialEq, Serialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum JobState {
    Queued,
    Downloading,
    Done,
    Failed,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct DownloadJob {
    pub md5: String,
    pub title: String,
    pub author: String,
    pub state: JobState,
    pub bytes: u64,
    pub total: u64,
    /// Path relative to the library directory.
    pub path: String,
    pub error: Option<String>,
    pub started_at: u64,
}

pub struct Library {
    cfg: Arc<Config>,
    http: reqwest::Client,
    resolvers: Vec<(String, Option<Regex>)>,
    jobs: Mutex<HashMap<String, DownloadJob>>,
    slots: Semaphore,
}

fn sanitize(s: &str, max: usize) -> String {
    let cleaned: String = s
        .chars()
        .map(|c| {
            if c.is_control() || r#"/\:*?"<>|"#.contains(c) {
                ' '
            } else {
                c
            }
        })
        .collect();
    let cleaned = cleaned.split_whitespace().collect::<Vec<_>>().join(" ");
    let cleaned = cleaned.trim_matches(|c: char| c == '.' || c.is_whitespace());
    let mut out: String = cleaned.chars().take(max).collect();
    if out.is_empty() {
        out.push_str("Unknown");
    }
    out.trim_end().to_string()
}

pub fn primary_author(author: &str) -> &str {
    author.split([';', '&']).next().unwrap_or("").trim()
}

/// `<Author>/<Title> (<year>).<ext>`, relative to the library root.
pub fn relative_path(book: &Book) -> PathBuf {
    let author = primary_author(&book.author);
    let author = if author.is_empty() {
        "Unknown Author"
    } else {
        author
    };
    let year: String = book
        .year
        .chars()
        .filter(char::is_ascii_digit)
        .take(4)
        .collect();
    let name = if year.len() == 4 {
        format!("{} ({year})", sanitize(&book.title, 150))
    } else {
        sanitize(&book.title, 150)
    };
    PathBuf::from(sanitize(author, 100)).join(format!("{name}.{}", sanitize(&book.extension, 10)))
}

pub fn download_name(book: &Book) -> String {
    let author = primary_author(&book.author);
    let base = if author.is_empty() {
        book.title.clone()
    } else {
        format!("{author} - {}", book.title)
    };
    format!("{}.{}", sanitize(&base, 180), book.extension)
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

impl Library {
    pub fn new(cfg: Arc<Config>, http: reqwest::Client) -> Result<Arc<Self>> {
        let resolvers = cfg
            .download
            .resolvers
            .iter()
            .map(|r| {
                Ok((
                    r.url.clone(),
                    r.link_pattern.as_deref().map(Regex::new).transpose()?,
                ))
            })
            .collect::<Result<_>>()?;
        let slots = Semaphore::new(cfg.download.max_concurrent.max(1));
        Ok(Arc::new(Self {
            cfg,
            http,
            resolvers,
            jobs: Mutex::default(),
            slots,
        }))
    }

    pub fn exists(&self, book: &Book) -> bool {
        self.cfg
            .paths
            .library_dir
            .join(relative_path(book))
            .exists()
    }

    /// Tries each resolver until one yields a response that is actually a file.
    pub async fn open_remote(&self, md5: &str) -> Result<reqwest::Response> {
        let mut last_err = anyhow::anyhow!("no download resolvers configured");
        for (template, pattern) in &self.resolvers {
            let url = template.replace("{md5}", md5);
            match self.try_resolver(&url, pattern.as_ref()).await {
                Ok(resp) => return Ok(resp),
                Err(e) => {
                    warn!(%url, error = format!("{e:#}"), "resolver failed");
                    last_err = e;
                }
            }
        }
        Err(last_err)
    }

    async fn try_resolver(&self, url: &str, pattern: Option<&Regex>) -> Result<reqwest::Response> {
        let file_url = match pattern {
            None => reqwest::Url::parse(url)?,
            Some(re) => {
                let page = self
                    .http
                    .get(url)
                    .send()
                    .await?
                    .error_for_status()?
                    .text()
                    .await?;
                let link = re
                    .captures(&page)
                    .and_then(|c| c.get(1))
                    .context("download link not found on page")?
                    .as_str()
                    .replace("&amp;", "&");
                reqwest::Url::parse(url)?.join(&link)?
            }
        };
        let resp = self
            .http
            .get(file_url.clone())
            .send()
            .await?
            .error_for_status()?;
        let is_html = resp
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v.starts_with("text/html"));
        if is_html {
            bail!("{file_url} returned an HTML page instead of a file");
        }
        Ok(resp)
    }

    pub async fn jobs(&self) -> Vec<DownloadJob> {
        let mut jobs: Vec<_> = self.jobs.lock().await.values().cloned().collect();
        jobs.sort_by_key(|j| std::cmp::Reverse(j.started_at));
        jobs
    }

    /// Queues a download into the library; returns the existing job if one is active.
    pub async fn enqueue(self: &Arc<Self>, book: Book) -> DownloadJob {
        let rel = relative_path(&book);
        let mut jobs = self.jobs.lock().await;
        if let Some(j) = jobs.get(&book.md5)
            && j.state != JobState::Failed
        {
            return j.clone();
        }
        let done = self.cfg.paths.library_dir.join(&rel).exists();
        let job = DownloadJob {
            md5: book.md5.clone(),
            title: book.title.clone(),
            author: book.author.clone(),
            state: if done {
                JobState::Done
            } else {
                JobState::Queued
            },
            bytes: 0,
            total: book.filesize,
            path: rel.to_string_lossy().into_owned(),
            error: None,
            started_at: now(),
        };
        jobs.insert(book.md5.clone(), job.clone());
        drop(jobs);
        if !done {
            let this = self.clone();
            tokio::spawn(async move {
                let md5 = book.md5.clone();
                let res = this.save(&book, &rel).await;
                let mut jobs = this.jobs.lock().await;
                if let Some(j) = jobs.get_mut(&md5) {
                    match res {
                        Ok(()) => {
                            j.state = JobState::Done;
                            info!(%md5, path = %j.path, "saved to library");
                        }
                        Err(e) => {
                            warn!(%md5, error = format!("{e:#}"), "library download failed");
                            j.state = JobState::Failed;
                            j.error = Some(format!("{e:#}"));
                        }
                    }
                }
            });
        }
        job
    }

    async fn update(&self, md5: &str, f: impl FnOnce(&mut DownloadJob)) {
        if let Some(j) = self.jobs.lock().await.get_mut(md5) {
            f(j);
        }
    }

    async fn save(&self, book: &Book, rel: &Path) -> Result<()> {
        let _slot = self.slots.acquire().await?;
        self.update(&book.md5, |j| j.state = JobState::Downloading)
            .await;
        let dest = self.cfg.paths.library_dir.join(rel);
        tokio::fs::create_dir_all(dest.parent().context("bad path")?).await?;
        let resp = self.open_remote(&book.md5).await?;
        if let Some(len) = resp.content_length() {
            self.update(&book.md5, |j| j.total = len).await;
        }
        let part = dest.with_extension(format!("{}.part", book.extension));
        let mut file = tokio::fs::File::create(&part).await?;
        let mut stream = resp.bytes_stream();
        let mut written = 0u64;
        let mut reported = 0u64;
        while let Some(chunk) = stream.next().await {
            let chunk = match chunk {
                Ok(c) => c,
                Err(e) => {
                    let _ = tokio::fs::remove_file(&part).await;
                    return Err(e.into());
                }
            };
            file.write_all(&chunk).await?;
            written += chunk.len() as u64;
            if written - reported > 256 * 1024 {
                reported = written;
                self.update(&book.md5, |j| j.bytes = written).await;
            }
        }
        file.flush().await?;
        drop(file);
        tokio::fs::rename(&part, &dest).await?;
        self.update(&book.md5, |j| j.bytes = written).await;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_paths() {
        let b = Book {
            title: "The Hobbit: or There/Back?".into(),
            author: "Tolkien, J.R.R.; Anderson".into(),
            year: "1937".into(),
            extension: "epub".into(),
            ..Default::default()
        };
        assert_eq!(
            relative_path(&b),
            PathBuf::from("Tolkien, J.R.R/The Hobbit or There Back (1937).epub")
        );
    }
}
