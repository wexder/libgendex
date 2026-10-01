//! Bounded persistent random access to split RAR volumes on an FTP mirror.
//!
//! RAR readers seek while parsing headers and extracting members. This adapter fetches aligned
//! source blocks on demand and keeps only a configured number of bytes in an LRU disk cache.

use std::{
    collections::{BTreeSet, HashMap},
    fs,
    io::{self, Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, OnceLock,
        mpsc::{self, Receiver, SyncSender},
    },
    thread::JoinHandle,
};

use reqwest::Url;
use sha2::{Digest, Sha256};
use suppaftp::{
    tokio::{AsyncFtpStream, AsyncNoTlsStream, TransferStream},
    types::FileType,
};
use tokio::io::AsyncReadExt;
use unrar_rs::{RarArchive, VolumeProvider, VolumeProviderError, archive::ReadSeek};

const BLOCK_SIZE: u64 = 65_536;

#[derive(Default)]
struct Entry {
    path: PathBuf,
    bytes: u64,
    used: u64,
}

/// Persistent LRU of source ranges, with an aggregate byte cap.
pub struct DiskRangeCache {
    root: PathBuf,
    cap: u64,
    index: Mutex<CacheIndex>,
    counters: Mutex<CacheCounters>,
    metadata_write: Mutex<()>,
    // One process owns this directory. OS releases the lock after a crash.
    _lease: fs::File,
}

#[derive(Default)]
struct CacheIndex {
    entries: HashMap<String, Entry>,
    lru: BTreeSet<(u64, String)>,
    clock: u64,
    bytes: u64,
}

#[derive(Debug, Clone, Copy, Default, serde::Serialize)]
pub struct CacheStats {
    pub ftp_size_requests: u64,
    pub size_cache_hits: u64,
    pub ftp_ranges: u64,
    pub ftp_transfers: u64,
    pub ftp_bytes: u64,
    pub cache_hits: u64,
    pub evictions: u64,
    pub cached_bytes: u64,
}

#[derive(Default)]
struct CacheCounters {
    ftp_size_requests: u64,
    size_cache_hits: u64,
    ftp_ranges: u64,
    ftp_transfers: u64,
    ftp_bytes: u64,
    cache_hits: u64,
    evictions: u64,
}

impl DiskRangeCache {
    pub fn open(root: impl Into<PathBuf>, cap: u64) -> io::Result<Self> {
        let root = root.into();
        fs::create_dir_all(&root)?;
        let lease = fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(root.join(".lock"))?;
        lease.try_lock().map_err(|e| {
            io::Error::other(format!(
                "FTP cache {} is already in use: {e}",
                root.display()
            ))
        })?;
        let mut files = Vec::new();
        for item in fs::read_dir(&root)? {
            let item = item?;
            let path = item.path();
            if path.extension().and_then(|s| s.to_str()) == Some("tmp") {
                remove_if_present(&path)?;
                continue;
            }
            if path.extension().and_then(|s| s.to_str()) != Some("blk") {
                continue;
            }
            let Some(key) = path.file_stem().and_then(|s| s.to_str()).map(str::to_owned) else {
                continue;
            };
            let meta = item.metadata()?;
            files.push((meta.modified().ok(), key, path, meta.len()));
        }
        files.sort_by_key(|item| item.0);
        let mut index = CacheIndex::default();
        for (_, key, path, bytes) in files {
            index.clock += 1;
            let used = index.clock;
            index.bytes += bytes;
            index.lru.insert((used, key.clone()));
            index.entries.insert(key, Entry { path, bytes, used });
        }
        let cache = Self {
            root,
            cap,
            index: Mutex::new(index),
            counters: Mutex::default(),
            metadata_write: Mutex::new(()),
            _lease: lease,
        };
        cache.trim()?;
        Ok(cache)
    }

    fn key(url: &str, offset: u64) -> String {
        let mut hash = Sha256::new();
        hash.update(url.as_bytes());
        hash.update(offset.to_le_bytes());
        hex(&hash.finalize())
    }

    pub fn stats(&self) -> CacheStats {
        let cached_bytes = self.index.lock().unwrap().bytes;
        let counters = self.counters.lock().unwrap();
        CacheStats {
            ftp_size_requests: counters.ftp_size_requests,
            size_cache_hits: counters.size_cache_hits,
            ftp_ranges: counters.ftp_ranges,
            ftp_transfers: counters.ftp_transfers,
            ftp_bytes: counters.ftp_bytes,
            cache_hits: counters.cache_hits,
            evictions: counters.evictions,
            cached_bytes,
        }
    }

    fn size(&self, url: &str) -> io::Result<u64> {
        let path = self.root.join(format!("{}.size", Self::key(url, u64::MAX)));
        if let Ok(s) = fs::read_to_string(&path)
            && let Ok(size) = s.trim().parse()
        {
            self.counters.lock().unwrap().size_cache_hits += 1;
            return Ok(size);
        }
        let _metadata_write = self.metadata_write.lock().unwrap();
        if let Ok(s) = fs::read_to_string(&path)
            && let Ok(size) = s.trim().parse()
        {
            self.counters.lock().unwrap().size_cache_hits += 1;
            return Ok(size);
        }
        let size =
            ftp_size(url).map_err(|e| io::Error::new(e.kind(), format!("FTP SIZE {url}: {e}")))?;
        let tmp = path.with_extension("size.tmp");
        fs::write(&tmp, size.to_string())?;
        fs::rename(tmp, path)?;
        self.counters.lock().unwrap().ftp_size_requests += 1;
        Ok(size)
    }

    fn get(&self, url: &str, offset: u64, expected: usize) -> io::Result<Option<Vec<u8>>> {
        let key = Self::key(url, offset);
        let mut index = self.index.lock().unwrap();
        let Some(entry) = index.entries.get(&key) else {
            return Ok(None);
        };
        let used = entry.used;
        let data = match fs::read(&entry.path) {
            Ok(data) => data,
            Err(e) if e.kind() == io::ErrorKind::NotFound => Vec::new(),
            Err(e) => return Err(e),
        };
        index.lru.remove(&(used, key.clone()));
        if data.len() != expected {
            let old = index.entries.remove(&key).unwrap();
            let _ = fs::remove_file(old.path);
            index.bytes = index.bytes.saturating_sub(old.bytes);
            return Ok(None);
        }
        index.clock += 1;
        let used = index.clock;
        index.entries.get_mut(&key).unwrap().used = used;
        index.lru.insert((used, key));
        self.counters.lock().unwrap().cache_hits += 1;
        Ok(Some(data))
    }

    fn put(&self, url: &str, offset: u64, data: &[u8]) -> io::Result<()> {
        // A zero cap disables disk caching; small caps also bypass blocks they cannot hold.
        if data.len() as u64 > self.cap {
            return Ok(());
        }
        let key = Self::key(url, offset);
        // Hold one lock through file replacement and bookkeeping. Concurrent readers must not
        // observe a block written by another writer with stale length or eviction metadata.
        let mut index = self.index.lock().unwrap();
        if let Some(old) = index.entries.remove(&key) {
            index.lru.remove(&(old.used, key.clone()));
            index.bytes = index.bytes.saturating_sub(old.bytes);
            remove_if_present(&old.path)?;
        }
        while index.bytes + data.len() as u64 > self.cap {
            self.evict(&mut index)?;
        }
        let final_path = self.root.join(format!("{key}.blk"));
        let temp_path = self.root.join(format!("{key}.tmp"));
        fs::write(&temp_path, data)?;
        fs::rename(&temp_path, &final_path)?;
        index.clock += 1;
        let used = index.clock;
        index.lru.insert((used, key.clone()));
        index.entries.insert(
            key,
            Entry {
                path: final_path,
                bytes: data.len() as u64,
                used,
            },
        );
        index.bytes += data.len() as u64;
        Ok(())
    }

    fn evict(&self, index: &mut CacheIndex) -> io::Result<()> {
        let (_, key) = index
            .lru
            .pop_first()
            .ok_or_else(|| io::Error::other("cache has no eviction candidate"))?;
        let victim = index.entries.remove(&key).unwrap();
        remove_if_present(&victim.path)?;
        index.bytes = index.bytes.saturating_sub(victim.bytes);
        self.counters.lock().unwrap().evictions += 1;
        Ok(())
    }

    fn trim(&self) -> io::Result<()> {
        let mut index = self.index.lock().unwrap();
        while index.bytes > self.cap {
            self.evict(&mut index)?;
        }
        Ok(())
    }
}

fn remove_if_present(path: &Path) -> io::Result<()> {
    match fs::remove_file(path) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        result => result,
    }
}

pub struct FtpRangeReader {
    url: String,
    size: u64,
    pos: u64,
    start: u64,
    block: Vec<u8>,
    cache: Arc<DiskRangeCache>,
    ftp: Option<FtpSession>,
}

impl FtpRangeReader {
    fn close_transfer(&mut self) {
        if let Some(mut ftp) = self.ftp.take() {
            runtime().block_on(ftp.abort_transfer());
        }
    }

    pub fn open(url: String, cache: Arc<DiskRangeCache>) -> io::Result<Self> {
        let size = cache.size(&url)?;
        Ok(Self {
            url,
            size,
            pos: 0,
            start: 0,
            block: Vec::new(),
            cache,
            ftp: None,
        })
    }

    fn load_block(&mut self) -> io::Result<()> {
        self.start = self.pos / BLOCK_SIZE * BLOCK_SIZE;
        let len = (self.size - self.start).min(BLOCK_SIZE) as usize;
        self.block = match self.cache.get(&self.url, self.start, len)? {
            Some(block) => block,
            None => {
                let (block, opened) = ftp_range(&self.url, self.start, len, &mut self.ftp)
                    .map_err(|e| {
                        io::Error::new(
                            e.kind(),
                            format!("FTP range {}+{}: {e}", self.url, self.start),
                        )
                    })?;
                if self.start + len as u64 == self.size {
                    // The final range reaches EOF: verify the server's completion reply.
                    if let Some(mut ftp) = self.ftp.take() {
                        runtime().block_on(async {
                            let stream = ftp.transfer.take().unwrap();
                            tokio::time::timeout(std::time::Duration::from_secs(5), stream.finish())
                                .await
                                .map_err(|_| {
                                    io::Error::new(
                                        io::ErrorKind::TimedOut,
                                        "FTP completion timed out",
                                    )
                                })?
                                .map_err(io::Error::other)
                        })?;
                    }
                }
                {
                    let mut counters = self.cache.counters.lock().unwrap();
                    counters.ftp_ranges += 1;
                    counters.ftp_transfers += u64::from(opened);
                    counters.ftp_bytes += block.len() as u64;
                }
                self.cache.put(&self.url, self.start, &block)?;
                block
            }
        };
        Ok(())
    }
}

impl Read for FtpRangeReader {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        if out.is_empty() || self.pos >= self.size {
            return Ok(0);
        }
        if self.pos < self.start || self.pos >= self.start + self.block.len() as u64 {
            self.load_block()?;
        }
        let offset = (self.pos - self.start) as usize;
        let n = out
            .len()
            .min(self.block.len() - offset)
            .min((self.size - self.pos) as usize);
        out[..n].copy_from_slice(&self.block[offset..offset + n]);
        self.pos += n as u64;
        Ok(n)
    }
}

impl Seek for FtpRangeReader {
    fn seek(&mut self, from: SeekFrom) -> io::Result<u64> {
        let pos = match from {
            SeekFrom::Start(n) => n,
            SeekFrom::Current(n) => self
                .pos
                .checked_add_signed(n)
                .ok_or(io::ErrorKind::InvalidInput)?,
            SeekFrom::End(n) => self
                .size
                .checked_add_signed(n)
                .ok_or(io::ErrorKind::InvalidInput)?,
        };
        if pos > self.size {
            return Err(io::ErrorKind::InvalidInput.into());
        }
        if pos != self.pos {
            // Header discovery can touch dozens of volumes. Release the control connection
            // as well as the data stream so idle catalog readers cannot exhaust the mirror's
            // per-address connection limit. Sequential extraction still keeps RETR open.
            self.close_transfer();
        }
        self.pos = pos;
        Ok(pos)
    }
}

impl Drop for FtpRangeReader {
    fn drop(&mut self) {
        self.close_transfer();
    }
}

pub struct FtpVolumeProvider {
    urls: Vec<String>,
    cache: Arc<DiskRangeCache>,
}

impl FtpVolumeProvider {
    pub fn new(urls: Vec<String>, cache: Arc<DiskRangeCache>) -> Self {
        Self { urls, cache }
    }
}

impl VolumeProvider for FtpVolumeProvider {
    fn get_volume(&self, volume: usize) -> Result<Box<dyn ReadSeek>, VolumeProviderError> {
        let url = self
            .urls
            .get(volume)
            .ok_or_else(|| VolumeProviderError::Unavailable {
                volume,
                reason: "volume is outside the discovered set".into(),
            })?;
        FtpRangeReader::open(url.clone(), self.cache.clone())
            .map(|reader| Box::new(reader) as Box<dyn ReadSeek>)
            .map_err(VolumeProviderError::Io)
    }
}

pub fn open_rar(urls: Vec<String>, cache: Arc<DiskRangeCache>) -> io::Result<RarArchive> {
    let started = std::time::Instant::now();
    let mut last_log = started;
    tracing::info!(
        volumes = urls.len(),
        "discovering RAR volume headers through FTP cache"
    );
    let first = urls
        .first()
        .ok_or_else(|| io::Error::other("RAR volume list is empty"))?;
    let reader = FtpRangeReader::open(first.clone(), cache.clone())?;
    let mut archive = RarArchive::open(reader).map_err(io::Error::other)?;
    let provider = FtpVolumeProvider::new(urls, cache);
    for i in 1..provider.urls.len() {
        let volume = provider.get_volume(i).map_err(io::Error::other)?;
        archive.add_volume(i, volume).map_err(io::Error::other)?;
        if last_log.elapsed().as_secs() >= 10 || i + 1 == provider.urls.len() {
            let stats = provider.cache.stats();
            tracing::info!(
                volumes_done = i + 1,
                volumes_total = provider.urls.len(),
                elapsed_seconds = started.elapsed().as_secs(),
                ftp_bytes = stats.ftp_bytes,
                cache_hits = stats.cache_hits,
                cached_bytes = stats.cached_bytes,
                "RAR volume discovery progress"
            );
            last_log = std::time::Instant::now();
        }
    }
    tracing::info!(
        members = archive.entries().count(),
        elapsed_seconds = started.elapsed().as_secs(),
        "RAR volume discovery complete"
    );
    Ok(archive)
}

pub fn stream_named_member(
    urls: Vec<String>,
    cache: Arc<DiskRangeCache>,
    wanted: &str,
) -> io::Result<StreamedMember> {
    let mut archive = open_rar(urls.clone(), cache.clone())?;
    let members = archive.entries().collect::<Vec<_>>();
    let (index, member) = members
        .iter()
        .enumerate()
        .find(|(_, m)| m.name.eq_ignore_ascii_case(wanted))
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                format!("RAR member {wanted} not found"),
            )
        })?;
    let name = member.name.clone();
    let size = member.unpacked_size.unwrap_or(0);
    let provider = FtpVolumeProvider::new(urls, cache);
    let (tx, rx) = mpsc::sync_channel(4);
    let worker = std::thread::spawn(move || {
        let entry = archive
            .by_index_via(index, &provider)
            .map_err(io::Error::other)?;
        let mut sink = ChannelWriter { tx };
        entry.copy_to(&mut sink).map_err(io::Error::other)?;
        Ok(())
    });
    Ok((name, size, Box::new(ChannelReader::new(rx)), worker))
}

pub type StreamedSql = (String, Box<dyn Read + Send>, JoinHandle<io::Result<()>>);

pub type StreamedMember = (
    String,
    u64,
    Box<dyn Read + Send>,
    JoinHandle<io::Result<()>>,
);

fn open_local_rar(first_volume: &Path) -> io::Result<(RarArchive, LocalVolumeProvider)> {
    let paths = local_volume_paths(first_volume)?;
    let mut archive = RarArchive::open(fs::File::open(&paths[0])?).map_err(io::Error::other)?;
    let provider = LocalVolumeProvider { paths };
    for (index, path) in provider.paths.iter().enumerate().skip(1) {
        archive
            .add_volume(index, Box::new(fs::File::open(path)?))
            .map_err(io::Error::other)?;
    }
    Ok((archive, provider))
}

pub fn local_rar_has_member(first_volume: &Path, wanted: &str) -> io::Result<bool> {
    let (archive, _) = open_local_rar(first_volume)?;
    Ok(archive
        .entries()
        .any(|m| m.name.eq_ignore_ascii_case(wanted)))
}

pub fn stream_local_named_member(first_volume: &Path, wanted: &str) -> io::Result<StreamedMember> {
    let (mut archive, provider) = open_local_rar(first_volume)?;
    let members = archive.entries().collect::<Vec<_>>();
    let (index, member) = members
        .iter()
        .enumerate()
        .find(|(_, m)| m.name.eq_ignore_ascii_case(wanted))
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                format!("RAR member {wanted} not found"),
            )
        })?;
    let name = member.name.clone();
    let size = member
        .unpacked_size
        .ok_or_else(|| io::Error::other("RAR member length unavailable"))?;
    let (tx, rx) = mpsc::sync_channel(4);
    let worker = std::thread::spawn(move || {
        let entry = archive
            .by_index_via(index, &provider)
            .map_err(io::Error::other)?;
        entry
            .copy_to(&mut ChannelWriter { tx })
            .map_err(io::Error::other)?;
        Ok(())
    });
    Ok((name, size, Box::new(ChannelReader::new(rx)), worker))
}

pub fn stream_local_sql_member(first_volume: &Path) -> io::Result<StreamedSql> {
    let paths = local_volume_paths(first_volume)?;
    let first = paths
        .first()
        .ok_or_else(|| io::Error::other("no local RAR volume"))?;
    let mut archive = RarArchive::open(fs::File::open(first)?).map_err(io::Error::other)?;
    let provider = LocalVolumeProvider { paths };
    for (index, path) in provider.paths.iter().enumerate().skip(1) {
        let file = fs::File::open(path)?;
        archive
            .add_volume(index, Box::new(file))
            .map_err(io::Error::other)?;
    }
    let members = archive.entries().collect::<Vec<_>>();
    let Some((index, member)) = members.iter().enumerate().find(|(_, m)| {
        let name = m.name.to_ascii_lowercase();
        name.ends_with(".sql") || name.ends_with(".sql.gz")
    }) else {
        let names = members
            .iter()
            .map(|m| m.name.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        return Err(io::Error::other(format!(
            "RAR has no SQL member; members are: {names}"
        )));
    };
    let name = member.name.clone();
    let (tx, rx) = mpsc::sync_channel(4);
    let worker = std::thread::spawn(move || {
        let entry = archive
            .by_index_via(index, &provider)
            .map_err(io::Error::other)?;
        let mut sink = ChannelWriter { tx };
        entry.copy_to(&mut sink).map_err(io::Error::other)?;
        Ok(())
    });
    Ok((name, Box::new(ChannelReader::new(rx)), worker))
}

fn local_volume_paths(first: &Path) -> io::Result<Vec<PathBuf>> {
    let name = first
        .file_name()
        .and_then(|s| s.to_str())
        .ok_or_else(|| io::Error::other("invalid local RAR filename"))?;
    let lower = name.to_ascii_lowercase();
    let Some(marker) = lower.rfind("part") else {
        return Ok(vec![first.to_path_buf()]);
    };
    let digits_start = marker + 4;
    let digits_end = lower[digits_start..]
        .find(".rar")
        .map(|n| digits_start + n)
        .ok_or_else(|| io::Error::other("split RAR filename must end in .rar"))?;
    let width = digits_end - digits_start;
    let first_index = name[digits_start..digits_end]
        .parse::<usize>()
        .map_err(io::Error::other)?;
    let prefix = &name[..digits_start];
    let suffix = &name[digits_end..];
    let mut paths = Vec::new();
    for index in first_index.. {
        let path = first.with_file_name(format!("{prefix}{index:0width$}{suffix}"));
        if !path.exists() {
            break;
        }
        paths.push(path);
    }
    Ok(paths)
}

struct LocalVolumeProvider {
    paths: Vec<PathBuf>,
}
impl VolumeProvider for LocalVolumeProvider {
    fn get_volume(&self, volume: usize) -> Result<Box<dyn ReadSeek>, VolumeProviderError> {
        let path = self
            .paths
            .get(volume)
            .ok_or_else(|| VolumeProviderError::Unavailable {
                volume,
                reason: "local volume not found".into(),
            })?;
        fs::File::open(path)
            .map(|file| Box::new(file) as Box<dyn ReadSeek>)
            .map_err(VolumeProviderError::Io)
    }
}

struct ChannelWriter {
    tx: SyncSender<Vec<u8>>,
}

impl io::Write for ChannelWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.is_empty() {
            return Ok(0);
        }
        self.tx
            .send(bytes.to_vec())
            .map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "SQL consumer stopped"))?;
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

struct ChannelReader {
    rx: Receiver<Vec<u8>>,
    current: Vec<u8>,
    offset: usize,
}

impl ChannelReader {
    fn new(rx: Receiver<Vec<u8>>) -> Self {
        Self {
            rx,
            current: Vec::new(),
            offset: 0,
        }
    }
}

impl Read for ChannelReader {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        if out.is_empty() {
            return Ok(0);
        }
        while self.offset == self.current.len() {
            match self.rx.recv() {
                Ok(bytes) => {
                    self.current = bytes;
                    self.offset = 0;
                }
                Err(_) => return Ok(0),
            }
        }
        let n = out.len().min(self.current.len() - self.offset);
        out[..n].copy_from_slice(&self.current[self.offset..self.offset + n]);
        self.offset += n;
        Ok(n)
    }
}

static RUNTIME: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
fn runtime() -> &'static tokio::runtime::Runtime {
    RUNTIME.get_or_init(|| {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
    })
}

async fn ftp_connect(url: &Url) -> io::Result<AsyncFtpStream> {
    let host = url
        .host_str()
        .ok_or_else(|| io::Error::other("FTP URL missing host"))?;
    let mut ftp = AsyncFtpStream::connect(format!("{host}:{}", url.port().unwrap_or(21)))
        .await
        .map_err(io::Error::other)?;
    let (user, pass) = if url.username().is_empty() {
        ("anonymous", "anonymous@")
    } else {
        (url.username(), url.password().unwrap_or_default())
    };
    ftp.login(user, pass).await.map_err(io::Error::other)?;
    ftp.transfer_type(FileType::Binary)
        .await
        .map_err(io::Error::other)?;
    Ok(ftp)
}

fn ftp_size(raw: &str) -> io::Result<u64> {
    let url = Url::parse(raw).map_err(io::Error::other)?;
    runtime().block_on(async {
        let mut last_error = None;
        for attempt in 0..8 {
            let result = tokio::time::timeout(std::time::Duration::from_secs(45), async {
                let mut ftp = ftp_connect(&url).await?;
                let size = ftp.size(url.path()).await.map_err(io::Error::other)? as u64;
                let _ = tokio::time::timeout(std::time::Duration::from_secs(2), ftp.quit()).await;
                Ok(size)
            })
            .await
            .unwrap_or_else(|_| {
                Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "FTP SIZE timed out",
                ))
            });
            match result {
                Ok(size) => return Ok(size),
                Err(error) => {
                    last_error = Some(error);
                    if attempt < 7 {
                        tokio::time::sleep(std::time::Duration::from_millis(
                            (250u64 << attempt.min(4)).min(4_000),
                        ))
                        .await;
                    }
                }
            }
        }
        Err(last_error.unwrap_or_else(|| io::Error::other("FTP SIZE failed")))
    })
}

struct FtpSession {
    control: AsyncFtpStream,
    transfer: Option<TransferStream<AsyncNoTlsStream>>,
    next_offset: u64,
}

impl FtpSession {
    async fn abort_transfer(&mut self) {
        if let Some(stream) = self.transfer.take() {
            // abort() detaches the data socket before awaiting the server, so timeout or
            // connection failure still closes it without leaving an unfinished transfer.
            // This control connection is always discarded after cleanup.
            let result = tokio::time::timeout(
                std::time::Duration::from_secs(2),
                self.control.abort(stream),
            )
            .await;
            if !matches!(result, Ok(Ok(()))) {
                tracing::debug!(
                    ?result,
                    "FTP partial transfer cleanup failed; discarding connection"
                );
            }
        }
    }
}

fn ftp_range(
    raw: &str,
    offset: u64,
    len: usize,
    session: &mut Option<FtpSession>,
) -> io::Result<(Vec<u8>, bool)> {
    let url = Url::parse(raw).map_err(io::Error::other)?;
    runtime().block_on(async {
        let mut last_error = None;
        for attempt in 0..8 {
            let result = tokio::time::timeout(std::time::Duration::from_secs(45), async {
                if session
                    .as_ref()
                    .is_some_and(|ftp| ftp.next_offset != offset)
                    && let Some(mut ftp) = session.take()
                {
                    // Cache hits can skip ranges. Discard this connection rather than
                    // reuse it with mirror-specific trailing abort replies.
                    ftp.abort_transfer().await;
                }
                if session.is_none() {
                    *session = Some(FtpSession {
                        control: ftp_connect(&url).await?,
                        transfer: None,
                        next_offset: offset,
                    });
                }
                let ftp = session.as_mut().unwrap();
                let opened = ftp.transfer.is_none();
                if opened {
                    let rest = usize::try_from(offset)
                        .map_err(|_| io::Error::other("FTP offset exceeds platform limit"))?;
                    ftp.control
                        .resume_transfer(rest)
                        .await
                        .map_err(io::Error::other)?;
                    ftp.transfer = Some(
                        ftp.control
                            .retr_as_stream(url.path())
                            .await
                            .map_err(io::Error::other)?,
                    );
                }
                let mut bytes = vec![0; len];
                ftp.transfer
                    .as_mut()
                    .unwrap()
                    .read_exact(&mut bytes)
                    .await?;
                ftp.next_offset = offset + len as u64;
                // Keep RETR open across consecutive 64 KiB cache blocks. This removes one
                // passive connection and round trip per block during large member extraction.
                Ok((bytes, opened))
            })
            .await
            .unwrap_or_else(|_| {
                Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "FTP range timed out",
                ))
            });
            match result {
                Ok(bytes) => return Ok(bytes),
                Err(error) => {
                    last_error = Some(error);
                    if let Some(mut ftp) = session.take() {
                        ftp.abort_transfer().await;
                    }
                    if attempt < 7 {
                        tokio::time::sleep(std::time::Duration::from_millis(
                            (250u64 << attempt.min(4)).min(4_000),
                        ))
                        .await;
                    }
                }
            }
        }
        Err(last_error.unwrap_or_else(|| io::Error::other("FTP range failed")))
    })
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

pub fn cache_dir(data_dir: &Path) -> PathBuf {
    data_dir.join("ftp-range-cache")
}

#[cfg(test)]
mod tests {
    use super::super::test_fixture::{FtpServer, TempDir};
    use super::*;

    #[test]
    fn disk_cache_enforces_lru_cap_persists_and_recovers_missing_blocks() {
        let dir = TempDir::new("cache-lru");
        let cache = DiskRangeCache::open(&dir.0, 8).unwrap();
        assert!(DiskRangeCache::open(&dir.0, 8).is_err());
        cache.put("a", 0, b"aaaa").unwrap();
        cache.put("b", 0, b"bbbb").unwrap();
        assert_eq!(cache.get("a", 0, 4).unwrap().unwrap(), b"aaaa");
        cache.put("c", 0, b"cccc").unwrap();
        assert!(cache.get("b", 0, 4).unwrap().is_none());
        assert_eq!(cache.stats().cached_bytes, 8);
        fs::remove_file(dir.0.join(format!("{}.blk", DiskRangeCache::key("a", 0)))).unwrap();
        assert!(cache.get("a", 0, 4).unwrap().is_none());
        cache.put("d", 0, b"dddd").unwrap();
        drop(cache);
        fs::write(dir.0.join("interrupted.tmp"), b"partial").unwrap();
        let cache = DiskRangeCache::open(&dir.0, 4).unwrap();
        assert!(!dir.0.join("interrupted.tmp").exists());
        assert_eq!(cache.stats().cached_bytes, 4);
        assert_eq!(cache.get("d", 0, 4).unwrap().unwrap(), b"dddd");
        fs::write(
            dir.0.join(format!("{}.blk", DiskRangeCache::key("d", 0))),
            b"bad",
        )
        .unwrap();
        assert!(cache.get("d", 0, 4).unwrap().is_none());
        assert_eq!(cache.stats().cached_bytes, 0);
        let disabled = DiskRangeCache::open(dir.0.join("disabled"), 0).unwrap();
        disabled.put("a", 0, b"aaaa").unwrap();
        assert!(disabled.get("a", 0, 4).unwrap().is_none());
    }

    #[test]
    fn concurrent_cache_writes_stay_consistent() {
        let dir = TempDir::new("cache-concurrent");
        let cache = Arc::new(DiskRangeCache::open(&dir.0, 64).unwrap());
        let workers = (0..4)
            .map(|worker| {
                let cache = cache.clone();
                std::thread::spawn(move || {
                    for i in 0..100 {
                        cache.put("same", i % 4, &[worker; 16]).unwrap();
                    }
                })
            })
            .collect::<Vec<_>>();
        for worker in workers {
            worker.join().unwrap();
        }
        assert_eq!(cache.stats().cached_bytes, 64);
        for i in 0..4 {
            assert_eq!(cache.get("same", i, 16).unwrap().unwrap().len(), 16);
        }
        let disk_bytes: u64 = fs::read_dir(&dir.0)
            .unwrap()
            .map(|p| p.unwrap().metadata().unwrap().len())
            .sum();
        assert_eq!(disk_bytes, 64);
    }

    #[test]
    fn ftp_read_retries_short_transfer_reuses_stream_and_serves_warm_cache() {
        let dir = TempDir::new("ftp-reader");
        let bytes = (0..(BLOCK_SIZE * 3 + 17))
            .map(|i| (i % 251) as u8)
            .collect::<Vec<_>>();
        let server = FtpServer::new(bytes.clone(), true);
        let cache = Arc::new(DiskRangeCache::open(&dir.0, BLOCK_SIZE * 4).unwrap());
        let mut reader = FtpRangeReader::open(server.url.clone(), cache.clone()).unwrap();
        let mut actual = vec![];
        reader.read_to_end(&mut actual).unwrap();
        assert_eq!(actual, bytes);
        assert_eq!(
            server.transfers.load(std::sync::atomic::Ordering::Relaxed),
            2
        );
        assert_eq!(cache.stats().ftp_transfers, 1); // Successful sequential reads use one RETR.
        assert!(reader.ftp.is_none(), "EOF must finish the FTP transfer");
        reader.seek(SeekFrom::Start(100)).unwrap();
        assert!(
            reader.ftp.is_none(),
            "seeking must release idle FTP control connections"
        );
        drop(reader);
        let mut reader = FtpRangeReader::open(server.url.clone(), cache.clone()).unwrap();
        reader.seek(SeekFrom::Start(BLOCK_SIZE + 100)).unwrap();
        let mut part = [0; 32];
        reader.read_exact(&mut part).unwrap();
        assert_eq!(
            &part,
            &bytes[BLOCK_SIZE as usize + 100..BLOCK_SIZE as usize + 132]
        );
        reader.seek(SeekFrom::End(-17)).unwrap();
        let mut tail = vec![];
        reader.read_to_end(&mut tail).unwrap();
        assert_eq!(tail, bytes[bytes.len() - 17..]);
        assert_eq!(cache.stats().ftp_ranges, 4);
        assert_eq!(cache.stats().ftp_size_requests, 1);
        assert_eq!(cache.stats().size_cache_hits, 1);
        assert!(reader.seek(SeekFrom::End(1)).is_err());
    }

    #[test]
    fn partial_ftp_transfers_are_aborted_on_seek_and_drop() {
        let dir = TempDir::new("ftp-abort");
        let server = FtpServer::new(vec![7; BLOCK_SIZE as usize * 4], false);
        let cache = Arc::new(DiskRangeCache::open(&dir.0, 0).unwrap());
        let mut reader = FtpRangeReader::open(server.url.clone(), cache).unwrap();
        let mut byte = [0];
        reader.read_exact(&mut byte).unwrap();
        assert!(reader.ftp.is_some());
        reader.seek(SeekFrom::Start(BLOCK_SIZE * 2)).unwrap();
        assert!(reader.ftp.is_none());
        assert_eq!(server.aborts.load(std::sync::atomic::Ordering::Relaxed), 1);
        reader.read_exact(&mut byte).unwrap();
        assert_eq!(byte, [7]);
        drop(reader);
        assert_eq!(server.aborts.load(std::sync::atomic::Ordering::Relaxed), 2);
    }
}
