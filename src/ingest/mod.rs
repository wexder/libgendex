mod api;
mod ftp;
mod ftp_range;
mod native;
mod progress;
mod sqldump;
mod stage;
#[cfg(test)]
mod test_fixture;
mod verification;

use native::{build_myisam_archive, build_remote};
pub use progress::BootstrapProgress;
use progress::BootstrapReporter;
pub use verification::verify_ftp;

use std::{
    collections::{BTreeMap, HashMap, HashSet},
    io::{BufRead, BufReader},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{Duration, SystemTime},
};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use tokio::sync::{Notify, RwLock};
use tracing::{debug, error, info, warn};
use utoipa::ToSchema;

use crate::{
    config::{Config, SourceConfig},
    search::SearchIndex,
};
use stage::{Stage, StageOptions};

#[derive(Debug, Clone, Default, Serialize, Deserialize, ToSchema)]
pub struct SourceState {
    pub name: String,
    pub dump_date: Option<String>,
    pub indexed_at: Option<u64>,
    pub books: u64,
    /// API changes are applied up to (excluding) this day; set once the source was bootstrapped.
    #[serde(default)]
    pub synced_until: Option<String>,
    /// Next per-topic file id (`fiction_id` / `libgen_id`) to fetch from the API's new-files stream.
    #[serde(default)]
    pub next_file_ids: BTreeMap<String, u64>,
}

#[derive(Debug, Clone, Default, Serialize, ToSchema)]
pub struct IndexStatus {
    pub running: bool,
    /// Human readable description of the current step.
    pub phase: String,
    pub bytes_done: u64,
    pub bytes_total: u64,
    pub last_error: Option<String>,
    pub last_run_finished: Option<u64>,
    pub total_books: u64,
    pub sources: Vec<SourceState>,
    /// Current native archive/table progress; totals describe this step, not the whole import.
    pub bootstrap: Option<BootstrapProgress>,
}

pub struct Indexer {
    cfg: Arc<Config>,
    index: Arc<SearchIndex>,
    http: reqwest::Client,
    status: RwLock<IndexStatus>,
    bootstrap: Mutex<Option<Arc<BootstrapReporter>>>,
    trigger: Notify,
}

/// A dump ready to be indexed.
struct Dump {
    date: String,
    input: DumpInput,
}

enum DumpInput {
    Local(PathBuf),
    Ftp(Vec<String>),
}

/// Empty id windows before the new-file stream is considered caught up.
const NEW_FILE_GAP_WINDOWS: u64 = 3;

fn now() -> u64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// Days since 1970-01-01 (UTC).
fn today() -> i64 {
    (now() / 86_400) as i64
}

/// Civil date for a day number (Howard Hinnant's algorithm), as `YYYY-MM-DD`.
fn days_to_date(days: i64) -> String {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}")
}

/// Day to start API updates from after importing a dump: the day before the dump was made.
fn dump_sync_start(dump_date: &str) -> Option<String> {
    let day = match dump_date.strip_prefix("local-") {
        Some(mtime) => mtime.parse::<i64>().ok()? / 86_400,
        None => date_to_days(dump_date)?,
    };
    Some(days_to_date(day - 1))
}

/// Where the modified-files stream starts after a dump import: the dump date, but no more than
/// `catchup_days` back (new files are covered by the id stream; this one only catches changes).
fn modified_sync_start(dump_date: &str, catchup_days: u64) -> Option<String> {
    let from_dump = date_to_days(&dump_sync_start(dump_date)?)?;
    Some(days_to_date(from_dump.max(today() - catchup_days as i64)))
}

/// Day number for a `YYYY-MM-DD` date.
fn date_to_days(date: &str) -> Option<i64> {
    let mut parts = date.get(..10)?.split('-').map(|p| p.parse::<i64>().ok());
    let (y, m, d) = (parts.next()??, parts.next()??, parts.next()??);
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let mp = if m > 2 { m - 3 } else { m + 9 };
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    Some(era * 146_097 + doe - 719_468)
}

impl Indexer {
    pub fn new(cfg: Arc<Config>, index: Arc<SearchIndex>, http: reqwest::Client) -> Arc<Self> {
        let this = Arc::new(Self {
            cfg,
            index,
            http,
            status: RwLock::default(),
            bootstrap: Mutex::new(None),
            trigger: Notify::new(),
        });
        let state = this.load_state();
        if let Ok(mut s) = this.status.try_write() {
            s.sources = state.into_values().collect();
            s.total_books = this.index.num_docs();
        }
        this
    }

    pub async fn status(&self) -> IndexStatus {
        let mut s = self.status.read().await.clone();
        s.total_books = self.index.num_docs();
        if s.running
            && let Some(reporter) = self.bootstrap.lock().unwrap().as_ref()
        {
            let p = reporter.snapshot();
            s.phase = format!(
                "{}{}",
                p.phase,
                p.table
                    .as_ref()
                    .map(|t| format!(": {t}"))
                    .unwrap_or_default()
            );
            s.bytes_done = p.bytes_done;
            s.bytes_total = p.bytes_total;
            s.bootstrap = Some(p);
        }
        s
    }

    /// Returns false when a run is already in progress.
    pub async fn trigger(&self) -> bool {
        if self.status.read().await.running {
            return false;
        }
        self.trigger.notify_one();
        true
    }

    pub async fn run_forever(self: Arc<Self>) {
        let cfg = &self.cfg.indexer;
        if !cfg.run_on_start {
            tokio::select! {
                _ = tokio::time::sleep(cfg.refresh_interval) => {}
                _ = self.trigger.notified() => {}
            }
        }
        loop {
            {
                let mut s = self.status.write().await;
                s.running = true;
                s.last_error = None;
            }
            if let Err(e) = self.run_once().await {
                error!(error = format!("{e:#}"), "index refresh failed");
                self.status.write().await.last_error = Some(format!("{e:#}"));
            }
            {
                let mut s = self.status.write().await;
                s.running = false;
                s.phase = "idle".into();
                s.last_run_finished = Some(now());
            }
            tokio::select! {
                _ = tokio::time::sleep(cfg.refresh_interval) => {}
                _ = self.trigger.notified() => info!("manual refresh requested"),
            }
        }
    }

    async fn set_phase(&self, phase: impl Into<String>) {
        let phase = phase.into();
        info!(%phase, "indexer");
        let mut s = self.status.write().await;
        s.phase = phase;
        s.bytes_done = 0;
        s.bytes_total = 0;
    }

    fn state_path(&self) -> PathBuf {
        self.cfg.paths.data_dir.join("state.json")
    }

    fn load_state(&self) -> BTreeMap<String, SourceState> {
        std::fs::read(self.state_path())
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default()
    }

    fn save_state(&self, state: &BTreeMap<String, SourceState>) -> Result<()> {
        let tmp = self.state_path().with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(state)?)?;
        std::fs::rename(tmp, self.state_path())?;
        Ok(())
    }

    /// Discovers snapshots, then streams each source into the index and applies API updates.
    async fn run_once(&self) -> Result<()> {
        let sources = &self.cfg.indexer.sources;
        self.set_phase("discovering snapshots").await;
        let preparations: Vec<_> = sources
            .iter()
            .map(|s| Box::pin(self.prepare_source(s)))
            .collect();
        let prepared = futures_util::future::join_all(preparations).await;

        let mut first_err = None;
        for (source, prepared) in sources.iter().zip(prepared) {
            let result = match prepared {
                Ok(Some(dump)) => self.index_source(source, dump).await,
                Ok(None) => Ok(()),
                Err(e) => Err(e),
            };
            if let Err(e) = result {
                error!(source = %source.name, error = format!("{e:#}"), "source refresh failed");
                first_err.get_or_insert(e.context(format!("source {}", source.name)));
            }
        }
        if self.cfg.indexer.api.enabled
            && let Err(e) = self.api_sync().await
        {
            error!(error = format!("{e:#}"), "API sync failed");
            first_err.get_or_insert(e.context("API sync"));
        }
        match first_err {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }

    /// API updates for every bootstrapped source: first new files by id (complete, however old the
    /// dump), then recent file changes by day (removals and edits of indexed files).
    async fn api_sync(&self) -> Result<()> {
        let mut state = self.load_state();
        let topics: HashMap<String, String> = self
            .cfg
            .indexer
            .sources
            .iter()
            .filter(|s| state.get(&s.name).is_some_and(|st| st.dump_date.is_some()))
            .flat_map(|s| s.topics.iter().map(|t| (t.clone(), s.name.clone())))
            .collect();
        if topics.is_empty() {
            return Ok(());
        }
        self.sync_new_files(&mut state, &topics).await?;
        self.sync_modified(&mut state, &topics).await
    }

    /// Walks each topic's file ids upward from where the dump (or the last sync) ended, the way
    /// update_libgen walks `idnewer`. Progress is saved after every window.
    async fn sync_new_files(
        &self,
        state: &mut BTreeMap<String, SourceState>,
        topics: &HashMap<String, String>,
    ) -> Result<()> {
        let page = self.cfg.indexer.api.page_size.clamp(1, 10_000) as u64;
        for (topic, name) in topics {
            let Some(mut next) = state
                .get(name)
                .and_then(|s| s.next_file_ids.get(topic).copied())
            else {
                warn!(source = %name, %topic, "no file id recorded for this topic; re-import the dump to enable new-file updates");
                continue;
            };
            let single = HashMap::from([(topic.clone(), name.clone())]);
            let (mut added, mut gap) = (0usize, 0u64);
            // Ids have gaps; only after several empty windows in a row is the end reached.
            while gap <= NEW_FILE_GAP_WINDOWS {
                let start = next + gap * page;
                self.set_phase(format!("{name}: fetching new files from id {start}"))
                    .await;
                let (files, max) = self.api_new_files(topic, start).await?;
                let Some(max) = max.filter(|_| !files.is_empty()) else {
                    gap += 1;
                    continue;
                };
                gap = 0;
                let changes = self
                    .process_files(files, &single, &format!("{name} ids {start}..={max}"))
                    .await?;
                added += self.apply_changes(changes).await?.0;
                next = max.max(start) + 1;
                if let Some(s) = state.get_mut(name) {
                    s.next_file_ids.insert(topic.clone(), next);
                }
                self.save_state(state)?;
            }
            info!(source = %name, %topic, added, next_file_id = next, "new files synced");
        }
        self.status.write().await.sources = state.values().cloned().collect();
        Ok(())
    }

    /// Replays file changes day by day (the API refuses longer ranges), at most
    /// `modified_catchup_days` back; progress is saved after each day.
    async fn sync_modified(
        &self,
        state: &mut BTreeMap<String, SourceState>,
        topics: &HashMap<String, String>,
    ) -> Result<()> {
        let last = today();
        let oldest = last - self.cfg.indexer.api.modified_catchup_days as i64;
        let first = topics
            .values()
            .filter_map(|name| {
                state
                    .get(name)?
                    .synced_until
                    .as_deref()
                    .and_then(date_to_days)
            })
            .min()
            .unwrap_or(oldest)
            .max(oldest);

        for day in first..=last {
            let date = days_to_date(day);
            let next = (day < last).then(|| days_to_date(day + 1));
            self.set_phase(format!(
                "syncing changes for {date} (day {} of {})",
                day - first + 1,
                last - first + 1
            ))
            .await;
            let changes = self.api_changes(&date, next.as_deref(), topics).await?;
            let (upserts, removals) = self.apply_changes(changes).await?;

            // Today is re-queried next time: its changes may still grow.
            let until = days_to_date((day + 1).min(last));
            for name in topics.values() {
                if let Some(s) = state.get_mut(name) {
                    s.synced_until = Some(until.clone());
                }
            }
            self.save_state(state)?;
            info!(day = %date, upserts, removals, "API changes applied");
        }
        self.status.write().await.sources = state.values().cloned().collect();
        Ok(())
    }

    /// Writes one batch of API changes to the index; returns (upserts, removals).
    async fn apply_changes(&self, changes: api::SyncResult) -> Result<(usize, usize)> {
        let counts = (changes.upserts.len(), changes.removals.len());
        if counts == (0, 0) {
            return Ok(counts);
        }
        let index = self.index.clone();
        let memory = self.cfg.indexer.writer_memory_mb;
        tokio::task::spawn_blocking(move || -> Result<()> {
            let mut writer = index.writer(memory)?;
            for md5 in &changes.removals {
                writer.delete_md5(md5);
            }
            for book in &changes.upserts {
                writer.delete_md5(&book.md5);
                writer.add(book)?;
            }
            writer.commit()
        })
        .await??;
        self.index.reload()?;
        Ok(counts)
    }

    /// Resolves a local fixture or FTP snapshot; `None` if already indexed.
    async fn prepare_source(&self, source: &SourceConfig) -> Result<Option<Dump>> {
        let prev = self
            .load_state()
            .get(&source.name)
            .cloned()
            .unwrap_or_default();
        if self.cfg.indexer.api.enabled && !source.topics.is_empty() && prev.dump_date.is_some() {
            debug!(source = %source.name, "bootstrapped; kept current through the API");
            return Ok(None);
        }
        let dump = if let Some(path) = &source.local_path {
            let modified = std::fs::metadata(path)
                .with_context(|| format!("reading {}", path.display()))?
                .modified()?
                .duration_since(SystemTime::UNIX_EPOCH)?
                .as_secs();
            Dump {
                date: format!("local-{modified}"),
                input: DumpInput::Local(path.clone()),
            }
        } else {
            let (date, urls) = ftp::discover(source).await?;
            if prev.dump_date.as_deref() == Some(date.as_str()) {
                info!(source = %source.name, %date, "dump already indexed");
                return Ok(None);
            }
            info!(source = %source.name, volumes = urls.len(), "using FTP range cache; archive volumes will not be saved in full");
            Dump {
                date,
                input: DumpInput::Ftp(urls),
            }
        };
        if prev.dump_date.as_deref() == Some(dump.date.as_str()) {
            info!(source = %source.name, date = %dump.date, "dump already indexed");
            return Ok(None);
        }
        Ok(Some(dump))
    }

    async fn index_source(&self, source: &SourceConfig, dump: Dump) -> Result<()> {
        self.set_phase(format!("{}: parsing dump {}", source.name, dump.date))
            .await;
        let cfg = self.cfg.clone();
        let index = self.index.clone();
        let name = source.name.clone();
        let topics = source.topics.clone();
        let reporter = BootstrapReporter::new();
        let worker_reporter = reporter.clone();
        if matches!(&dump.input, DumpInput::Ftp(_)) {
            reporter.begin("opening FTP archive", None, None, 0, 0);
            *self.bootstrap.lock().unwrap() = Some(reporter.clone());
        }
        let mut worker = tokio::task::spawn_blocking(move || match dump.input {
            DumpInput::Ftp(urls) => {
                build_remote(&cfg, &index, &name, &topics, urls, worker_reporter)
            }
            DumpInput::Local(path) => build(&cfg, &index, &name, &topics, &path),
        });
        let mut tick = tokio::time::interval(Duration::from_secs(10));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        tick.tick().await;
        let result = loop {
            tokio::select! {
                result = &mut worker => break result,
                _ = tick.tick() => {
                    if self.bootstrap.lock().unwrap().is_some() {
                        reporter.log(&source.name);
                    }
                }
            }
        };
        *self.bootstrap.lock().unwrap() = None;
        let (books, max_ids) = result??;
        self.index.reload()?;

        let mut state = self.load_state();
        state.insert(
            source.name.clone(),
            SourceState {
                name: source.name.clone(),
                synced_until: modified_sync_start(
                    &dump.date,
                    self.cfg.indexer.api.modified_catchup_days,
                ),
                next_file_ids: source
                    .topics
                    .iter()
                    .filter_map(|t| Some((t.clone(), max_ids.get(t).filter(|id| **id > 0)? + 1)))
                    .collect(),
                dump_date: Some(dump.date),
                indexed_at: Some(now()),
                books,
            },
        );
        self.save_state(&state)?;
        self.status.write().await.sources = state.into_values().collect();

        info!(source = %source.name, books, "source indexed");
        Ok(())
    }
}

type DumpReader = (
    Box<dyn BufRead>,
    Option<std::thread::JoinHandle<std::io::Result<()>>>,
);

fn open_dump(path: &Path) -> Result<DumpReader> {
    let name = path.to_string_lossy().to_lowercase();
    if name.ends_with(".rar") {
        let (member, stream, worker) = ftp_range::stream_local_sql_member(path)?;
        let stream: Box<dyn std::io::Read + Send> =
            if member.to_ascii_lowercase().ends_with(".sql.gz") {
                Box::new(flate2::read::MultiGzDecoder::new(stream))
            } else {
                stream
            };
        Ok((
            Box::new(BufReader::with_capacity(1 << 20, stream)),
            Some(worker),
        ))
    } else if name.ends_with(".gz") {
        let f = std::fs::File::open(path)?;
        Ok((
            Box::new(BufReader::with_capacity(
                1 << 20,
                flate2::read::MultiGzDecoder::new(f),
            )),
            None,
        ))
    } else {
        Ok((
            Box::new(BufReader::with_capacity(
                1 << 20,
                std::fs::File::open(path)?,
            )),
            None,
        ))
    }
}

/// Streams the dump through the staging DB and replaces the source's documents in the index.
/// Books indexed from a dump and the highest file id per topic it contained.
type DumpStats = (u64, HashMap<String, u64>);

fn build(
    cfg: &Config,
    index: &SearchIndex,
    source: &str,
    topics: &[String],
    archive: &Path,
) -> Result<DumpStats> {
    if archive
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("rar"))
        && ftp_range::local_rar_has_member(archive, "editions.frm")?
    {
        return build_myisam_archive(cfg, index, source, topics, None, |name| {
            ftp_range::stream_local_named_member(archive, name)
        });
    }
    let (reader, worker) = open_dump(archive)?;
    let result = build_reader(cfg, index, source, topics, reader);
    finish_extract_worker(worker, result)
}

fn finish_extract_worker(
    worker: Option<std::thread::JoinHandle<std::io::Result<()>>>,
    result: Result<DumpStats>,
) -> Result<DumpStats> {
    if let Some(worker) = worker {
        let extraction = worker
            .join()
            .map_err(|_| anyhow::anyhow!("RAR extraction thread panicked"))?;
        if result.is_ok() {
            extraction?;
        }
    }
    result
}

fn build_reader(
    cfg: &Config,
    index: &SearchIndex,
    source: &str,
    topics: &[String],
    reader: Box<dyn BufRead>,
) -> Result<DumpStats> {
    let topics: HashSet<String> = topics.iter().cloned().collect();
    let extensions: HashSet<String> = cfg
        .indexer
        .extensions
        .iter()
        .map(|e| e.to_lowercase())
        .collect();
    let opts = StageOptions {
        extensions: &extensions,
        topics: &topics,
        language_key: cfg.indexer.language_key,
        isbn_key: cfg.indexer.isbn_key,
    };
    let stage_path = cfg.paths.data_dir.join(format!("stage-{source}.sqlite"));
    let mut stage = Stage::create(&stage_path)?;

    stage.load(reader, &opts)?;

    let mut writer = index.writer(cfg.indexer.writer_memory_mb)?;
    writer.delete_source(source);
    let mut count = 0u64;
    let res = stage.for_each_book(source, &opts, |book| {
        writer.add(&book)?;
        count += 1;
        if count.is_multiple_of(500_000) {
            info!(source, count, "indexing");
        }
        Ok(())
    });
    match res {
        Ok(()) => writer.commit()?,
        Err(e) => {
            writer.rollback()?;
            return Err(e);
        }
    }
    let max_ids = std::mem::take(&mut stage.max_ids);
    drop(stage);
    Ok((count, max_ids))
}

#[cfg(test)]
mod tests {
    use super::progress::ProgressReader;
    use super::*;
    use std::io::Read;
    use std::sync::atomic::Ordering;

    #[test]
    fn bootstrap_progress_tracks_reads_and_resets_each_step() {
        let reporter = BootstrapReporter::new();
        reporter.begin(
            "decoding and staging table",
            Some("files"),
            Some("files.MYD"),
            10,
            3,
        );
        let mut reader = ProgressReader {
            inner: Box::new(std::io::Cursor::new(vec![1, 2, 3])),
            reporter: reporter.clone(),
        };
        reader.read_to_end(&mut Vec::new()).unwrap();
        reporter.rows.store(2, Ordering::Relaxed);
        let p = reporter.snapshot();
        assert_eq!(
            (p.rows_done, p.rows_total, p.bytes_done, p.bytes_total),
            (2, 10, 3, 3)
        );
        assert_eq!(p.member.as_deref(), Some("files.MYD"));
        assert!(p.rows_per_second.is_finite());
        assert!(p.bytes_per_second.is_finite());
        serde_json::to_value(p).unwrap();
        reporter.begin("committing search index", None, None, 0, 0);
        let p = reporter.snapshot();
        assert_eq!(
            (p.rows_done, p.bytes_done, p.rows_total, p.bytes_total),
            (0, 0, 0, 0)
        );
        assert!(p.table.is_none());
    }

    #[tokio::test]
    async fn index_status_exposes_live_bootstrap_progress() {
        let dir = test_fixture::TempDir::new("progress-status");
        let mut cfg = Config::default();
        cfg.paths.data_dir = dir.0.clone();
        let index = Arc::new(SearchIndex::open(&dir.0.join("index")).unwrap());
        let indexer = Indexer::new(Arc::new(cfg), index, reqwest::Client::new());
        indexer.status.write().await.running = true;
        let reporter = BootstrapReporter::new();
        reporter.begin(
            "decoding and staging table",
            Some("editions"),
            Some("editions.MYD"),
            100,
            1000,
        );
        reporter.rows.store(25, Ordering::Relaxed);
        reporter.bytes.store(250, Ordering::Relaxed);
        *indexer.bootstrap.lock().unwrap() = Some(reporter);
        let status = indexer.status().await;
        assert_eq!(status.phase, "decoding and staging table: editions");
        assert_eq!((status.bytes_done, status.bytes_total), (250, 1000));
        assert_eq!(status.bootstrap.unwrap().rows_done, 25);
        *indexer.bootstrap.lock().unwrap() = None;
        assert!(indexer.status().await.bootstrap.is_none());
    }

    #[test]
    fn dates_round_trip() {
        assert_eq!(days_to_date(0), "1970-01-01");
        assert_eq!(date_to_days("2026-09-27"), Some(20_723));
        assert_eq!(days_to_date(20_723), "2026-09-27");
        assert_eq!(
            days_to_date(date_to_days("2024-02-29").unwrap() + 1),
            "2024-03-01"
        );
    }

    #[test]
    fn local_native_rar_bootstrap_indexes_and_cleans_stage() {
        let dir = test_fixture::TempDir::new("local-ingest");
        let archive = dir.0.join("fixture.rar");
        std::fs::write(&archive, test_fixture::rar(&test_fixture::books(false))).unwrap();
        let mut cfg = Config::default();
        cfg.paths.data_dir = dir.0.clone();
        let index = SearchIndex::open(&dir.0.join("index")).unwrap();
        let (count, max_ids) =
            build_myisam_archive(&cfg, &index, "fixture", &["f".into()], None, |name| {
                ftp_range::stream_local_named_member(&archive, name)
            })
            .unwrap();
        assert_eq!(count, 1);
        assert_eq!(max_ids["f"], 77);
        index.reload().unwrap();
        let found = index.search("Native Rust", None, None, 10).unwrap();
        assert_eq!(found.len(), 1);
        let book = index
            .get("0123456789abcdef0123456789abcdef")
            .unwrap()
            .unwrap();
        assert_eq!(book.title, "The Native Rust Archive");
        assert_eq!(book.language.to_lowercase(), "english");
        assert_eq!(book.filesize, 12345);
        assert_eq!(book.pages, 321);
        assert!(!dir.0.join("stage-fixture.sqlite").exists());
        // A corrupt count must fail before replacing the valid committed index.
        std::fs::write(&archive, test_fixture::rar(&test_fixture::books(true))).unwrap();
        let err = build_myisam_archive(&cfg, &index, "fixture", &["f".into()], None, |name| {
            ftp_range::stream_local_named_member(&archive, name)
        })
        .unwrap_err();
        assert!(err.to_string().contains("declares 2 rows"), "{err:#}");
        assert!(!dir.0.join("stage-fixture.sqlite").exists());
        index.reload().unwrap();
        assert_eq!(
            index.search("Native Rust", None, None, 10).unwrap().len(),
            1
        );
    }

    #[test]
    fn ftp_native_rar_to_search_and_warm_cache() {
        let dir = test_fixture::TempDir::new("ftp-ingest");
        let server =
            test_fixture::FtpServer::new(test_fixture::rar(&test_fixture::books(false)), false);
        let mut cfg = Config::default();
        cfg.indexer.ftp_cache_mb = 1;
        let first = verify_ftp(
            cfg.clone(),
            server.url.clone(),
            1,
            dir.0.join("work"),
            dir.0.join("cache"),
            0,
        )
        .unwrap();
        assert_eq!(first["books"], 1);
        assert_eq!(first["stage_removed"], true);
        assert_eq!(first["complete_snapshot"], true);
        assert_eq!(first["search_hits"], 1);
        assert!(first["ftp"]["ftp_bytes"].as_u64().unwrap() > 0);
        let second = verify_ftp(
            cfg,
            server.url.clone(),
            1,
            dir.0.join("work"),
            dir.0.join("cache"),
            0,
        )
        .unwrap();
        assert_eq!(second["books"], 1);
        assert_eq!(second["ftp"]["ftp_ranges"], 0);
        assert_eq!(second["ftp"]["ftp_size_requests"], 0);
    }

    #[tokio::test]
    async fn bootstrapped_source_uses_api_refresh_without_discovery() {
        let dir = test_fixture::TempDir::new("api-refresh");
        let mut cfg = Config::default();
        cfg.paths.data_dir = dir.0.clone();
        // An invalid listing would fail discovery if the daily refresh tried to read a dump.
        cfg.indexer.sources[0].listing_urls = vec!["invalid://unused".into()];
        let source = cfg.indexer.sources[0].clone();
        let index = Arc::new(SearchIndex::open(&dir.0.join("index")).unwrap());
        let indexer = Indexer::new(Arc::new(cfg), index, reqwest::Client::new());
        indexer
            .save_state(&BTreeMap::from([(
                source.name.clone(),
                SourceState {
                    name: source.name.clone(),
                    dump_date: Some("2026-09-06".into()),
                    ..SourceState::default()
                },
            )]))
            .unwrap();
        assert!(indexer.prepare_source(&source).await.unwrap().is_none());
        assert!(!dir.0.join("dumps").exists());
        assert!(!dir.0.join("ftp-range-cache").exists());
    }

    #[tokio::test]
    async fn service_bootstrap_saves_state_for_daily_api_continuation() {
        let dir = test_fixture::TempDir::new("service-bootstrap");
        let server =
            test_fixture::FtpServer::new(test_fixture::rar(&test_fixture::books(false)), false);
        let mut cfg = Config::default();
        cfg.paths.data_dir = dir.0.clone();
        cfg.indexer.sources[0].listing_urls =
            vec![format!("{}/", server.url.rsplit_once('/').unwrap().0)];
        let source = cfg.indexer.sources[0].clone();
        let index = Arc::new(SearchIndex::open(&dir.0.join("index")).unwrap());
        let indexer = Indexer::new(Arc::new(cfg), index, reqwest::Client::new());
        let dump = indexer.prepare_source(&source).await.unwrap().unwrap();
        assert!(matches!(&dump.input, DumpInput::Ftp(_)));
        indexer.index_source(&source, dump).await.unwrap();
        let state = indexer.load_state();
        assert_eq!(state[&source.name].books, 1);
        assert_eq!(state[&source.name].dump_date.as_deref(), Some("2026-09-06"));
        assert_eq!(state[&source.name].next_file_ids["f"], 78);
        assert!(state[&source.name].synced_until.is_some());
        assert!(indexer.prepare_source(&source).await.unwrap().is_none());
        assert!(indexer.bootstrap.lock().unwrap().is_none());
        assert!(!dir.0.join("dumps").exists());
        assert!(!dir.0.join("stage-libgen.sqlite").exists());
        assert_eq!(indexer.status().await.total_books, 1);
    }
}
