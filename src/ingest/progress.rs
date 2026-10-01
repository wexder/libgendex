//! Live native-bootstrap counters shared by the worker, status API, and log heartbeat.

use super::ftp_range;
use serde::Serialize;
use std::{
    io::Read,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::Instant,
};
use tracing::info;
use utoipa::ToSchema;

#[derive(Debug, Clone, Default, Serialize, ToSchema)]
pub struct BootstrapProgress {
    pub phase: String,
    pub table: Option<String>,
    pub member: Option<String>,
    pub rows_done: u64,
    pub rows_total: u64,
    pub bytes_done: u64,
    pub bytes_total: u64,
    pub elapsed_seconds: u64,
    pub step_elapsed_seconds: u64,
    pub rows_per_second: f64,
    pub bytes_per_second: f64,
    pub ftp_bytes: u64,
    pub cache_hits: u64,
    pub cached_bytes: u64,
    pub cache_evictions: u64,
}

pub(super) struct BootstrapReporter {
    step: Mutex<(BootstrapProgress, Instant)>,
    started: Instant,
    pub(super) rows: AtomicU64,
    pub(super) bytes: AtomicU64,
    pub(super) cache: Mutex<Option<Arc<ftp_range::DiskRangeCache>>>,
}

impl BootstrapReporter {
    pub(super) fn new() -> Arc<Self> {
        Arc::new(Self {
            step: Mutex::new((BootstrapProgress::default(), Instant::now())),
            started: Instant::now(),
            rows: AtomicU64::new(0),
            bytes: AtomicU64::new(0),
            cache: Mutex::new(None),
        })
    }

    pub(super) fn begin(
        &self,
        phase: &str,
        table: Option<&str>,
        member: Option<&str>,
        rows: u64,
        bytes: u64,
    ) {
        let mut step = self.step.lock().unwrap();
        self.rows.store(0, Ordering::Relaxed);
        self.bytes.store(0, Ordering::Relaxed);
        *step = (
            BootstrapProgress {
                phase: phase.into(),
                table: table.map(str::to_owned),
                member: member.map(str::to_owned),
                rows_total: rows,
                bytes_total: bytes,
                ..Default::default()
            },
            Instant::now(),
        );
        info!(
            phase,
            table,
            member,
            rows_total = rows,
            bytes_total = bytes,
            "bootstrap step started"
        );
    }

    pub(super) fn snapshot(&self) -> BootstrapProgress {
        let (mut result, seconds) = {
            let step = self.step.lock().unwrap();
            let mut result = step.0.clone();
            result.rows_done = self.rows.load(Ordering::Relaxed);
            result.bytes_done = self.bytes.load(Ordering::Relaxed);
            (result, step.1.elapsed().as_secs_f64())
        };
        result.elapsed_seconds = self.started.elapsed().as_secs();
        result.step_elapsed_seconds = seconds as u64;
        if seconds > 0.0 {
            result.rows_per_second = result.rows_done as f64 / seconds;
            result.bytes_per_second = result.bytes_done as f64 / seconds;
        }
        if let Some(cache) = self.cache.lock().unwrap().as_ref() {
            let stats = cache.stats();
            result.ftp_bytes = stats.ftp_bytes;
            result.cache_hits = stats.cache_hits;
            result.cached_bytes = stats.cached_bytes;
            result.cache_evictions = stats.evictions;
        }
        result
    }

    pub(super) fn log(&self, source: &str) {
        let p = self.snapshot();
        info!(source, phase = %p.phase, table = ?p.table, member = ?p.member,
            rows = p.rows_done, rows_total = p.rows_total,
            bytes = p.bytes_done, bytes_total = p.bytes_total,
            elapsed_seconds = p.elapsed_seconds, step_elapsed_seconds = p.step_elapsed_seconds,
            rows_per_second = p.rows_per_second.round(),
            mib_per_second = p.bytes_per_second / 1_048_576.0,
            ftp_bytes = p.ftp_bytes, cache_hits = p.cache_hits,
            cached_bytes = p.cached_bytes, cache_evictions = p.cache_evictions,
            "bootstrap progress");
    }
}

pub(super) struct ProgressReader {
    pub(super) inner: Box<dyn Read + Send>,
    pub(super) reporter: Arc<BootstrapReporter>,
}

impl Read for ProgressReader {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self.inner.read(buf)?;
        self.reporter.bytes.fetch_add(n as u64, Ordering::Relaxed);
        Ok(n)
    }
}
