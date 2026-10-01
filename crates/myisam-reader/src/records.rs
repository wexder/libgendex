use crate::{
    MyisamInfo,
    bytes::{be_u16, be_u24, be_u32, be_u64, invalid, invalid_at},
};
use std::{collections::BTreeMap, io, path::Path};

#[derive(Debug, Default)]
pub struct WalkStats {
    pub records: u64,
    pub deleted_blocks: u64,
    pub continuation_blocks: u64,
    pub fragmented_records: u64,
    pub bytes_consumed: u64,
    pub spill_bytes_written: u64,
    pub spill_disk_bytes: u64,
}

/// Select the record layout declared by `.MYI`. Some LibGen tables (notably the file/edition
/// links) use fixed records, while the text tables use dynamic records.
pub fn walk_records<R: io::Read>(
    reader: R,
    length: u64,
    info: &MyisamInfo,
    limit: Option<u64>,
    on_record: impl FnMut(&[u8]) -> io::Result<()>,
) -> io::Result<WalkStats> {
    walk_records_in(
        reader,
        length,
        info,
        limit,
        &std::env::temp_dir(),
        on_record,
    )
}

/// Configurable fragment memory budgets and scratch storage.
#[derive(Debug, Clone)]
pub struct WalkOptions {
    pub row_limit: Option<u64>,
    pub scratch_dir: std::path::PathBuf,
    pub max_record_bytes: usize,
    pub pending_bytes: usize,
    pub earlier_bytes: usize,
    pub lookback_bytes: u64,
}
impl Default for WalkOptions {
    fn default() -> Self {
        Self {
            row_limit: None,
            scratch_dir: std::env::temp_dir(),
            max_record_bytes: 128 * 1024 * 1024,
            pending_bytes: 128 * 1024 * 1024,
            earlier_bytes: 32 * 1024 * 1024,
            lookback_bytes: 16 * 1024 * 1024,
        }
    }
}

pub fn walk_records_in<R: io::Read>(
    reader: R,
    length: u64,
    info: &MyisamInfo,
    limit: Option<u64>,
    scratch_dir: &Path,
    on_record: impl FnMut(&[u8]) -> io::Result<()>,
) -> io::Result<WalkStats> {
    let options = WalkOptions {
        row_limit: limit,
        scratch_dir: scratch_dir.into(),
        ..WalkOptions::default()
    };
    walk_records_with_options(reader, length, info, &options, on_record)
}

/// Zero pending/earlier budgets force unresolved fragments to temporary disk immediately.
pub fn walk_records_with_options<R: io::Read>(
    mut reader: R,
    length: u64,
    info: &MyisamInfo,
    options: &WalkOptions,
    mut on_record: impl FnMut(&[u8]) -> io::Result<()>,
) -> io::Result<WalkStats> {
    if options.max_record_bytes == 0 {
        return Err(invalid("record size limit must be positive"));
    }
    if info.options & 4 != 0 {
        return Err(invalid("myisampack compressed tables are not supported"));
    }
    if info.options & 1 != 0 {
        return walk_dynamic_with_options(reader, length, options, on_record);
    }
    let width = info.record_len as usize;
    if width > options.max_record_bytes {
        return Err(invalid("fixed record exceeds record safety limit"));
    }
    if width == 0 || !length.is_multiple_of(width as u64) {
        return Err(invalid(
            "fixed MyISAM data length is not a multiple of record width",
        ));
    }
    let mut stats = WalkStats::default();
    let mut row = vec![0; width];
    while stats.bytes_consumed < length && !options.row_limit.is_some_and(|n| stats.records >= n) {
        reader.read_exact(&mut row)?;
        stats.bytes_consumed += width as u64;
        if row[0] == 0 {
            stats.deleted_blocks += 1;
            continue;
        }
        on_record(&row)?;
        stats.records += 1;
    }
    Ok(stats)
}

pub(crate) struct PendingRow {
    pub(crate) expected: usize,
    pub(crate) bytes: Vec<u8>,
}

pub(crate) struct Continuation {
    pub(crate) bytes: Vec<u8>,
    pub(crate) next: Option<u64>,
    pub(crate) physical_next: u64,
}

/// Only unresolved fragments are spooled. This supports pointers anywhere earlier in `.MYD`
/// while keeping the stream's RAM bounded; the complete extracted table is never saved.
pub(crate) struct FragmentSpill {
    dir: std::path::PathBuf,
    path: Option<std::path::PathBuf>,
    db: Option<rusqlite::Connection>,
    written: u64,
}
impl FragmentSpill {
    pub(crate) fn new(dir: &Path) -> Self {
        Self {
            dir: dir.into(),
            path: None,
            db: None,
            written: 0,
        }
    }
    fn open(&mut self) -> io::Result<&rusqlite::Connection> {
        if self.db.is_none() {
            static SEQUENCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            let seq = SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let stamp = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_err(io::Error::other)?
                .as_nanos();
            let path = self.dir.join(format!(
                "myisam-fragments-{}-{stamp}-{seq}.sqlite",
                std::process::id()
            ));
            std::fs::OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(&path)?;
            self.path = Some(path.clone());
            let db = rusqlite::Connection::open(path).map_err(io::Error::other)?;
            db.execute_batch("PRAGMA journal_mode=OFF; PRAGMA synchronous=OFF; PRAGMA cache_size=-2048;
                CREATE TABLE pending(pos INTEGER PRIMARY KEY, expected INTEGER, bytes BLOB);
                CREATE TABLE earlier(pos INTEGER PRIMARY KEY, bytes BLOB, next INTEGER, physical INTEGER);
                BEGIN;").map_err(io::Error::other)?;
            self.db = Some(db);
        }
        Ok(self.db.as_ref().unwrap())
    }
    pub(crate) fn put_pending(&mut self, pos: u64, row: PendingRow) -> io::Result<()> {
        self.written += row.bytes.len() as u64;
        self.open()?
            .prepare_cached("INSERT INTO pending VALUES (?,?,?)")
            .map_err(io::Error::other)?
            .execute(rusqlite::params![
                pos as i64,
                row.expected as i64,
                row.bytes
            ])
            .map_err(io::Error::other)?;
        Ok(())
    }
    pub(crate) fn take_pending(&self, pos: u64) -> io::Result<Option<PendingRow>> {
        use rusqlite::OptionalExtension;
        let Some(db) = &self.db else {
            return Ok(None);
        };
        let row = db
            .prepare_cached("SELECT expected, bytes FROM pending WHERE pos=?")
            .map_err(io::Error::other)?
            .query_row([pos as i64], |r| {
                Ok(PendingRow {
                    expected: r.get::<_, i64>(0)? as usize,
                    bytes: r.get(1)?,
                })
            })
            .optional()
            .map_err(io::Error::other)?;
        if row.is_some() {
            db.prepare_cached("DELETE FROM pending WHERE pos=?")
                .map_err(io::Error::other)?
                .execute([pos as i64])
                .map_err(io::Error::other)?;
        }
        Ok(row)
    }
    pub(crate) fn has_pending(&self, pos: Option<u64>) -> io::Result<bool> {
        let Some(db) = &self.db else {
            return Ok(false);
        };
        let count: i64 = if let Some(pos) = pos {
            db.query_row(
                "SELECT count(*) FROM pending WHERE pos=?",
                [pos as i64],
                |r| r.get(0),
            )
        } else {
            db.query_row("SELECT count(*) FROM pending", [], |r| r.get(0))
        }
        .map_err(io::Error::other)?;
        Ok(count != 0)
    }
    pub(crate) fn put_earlier(&mut self, pos: u64, fragment: Continuation) -> io::Result<()> {
        self.written += fragment.bytes.len() as u64;
        self.open()?
            .prepare_cached("INSERT INTO earlier VALUES (?,?,?,?)")
            .map_err(io::Error::other)?
            .execute(rusqlite::params![
                pos as i64,
                fragment.bytes,
                fragment.next.map(|n| n as i64),
                fragment.physical_next as i64
            ])
            .map_err(io::Error::other)?;
        Ok(())
    }
    fn take_earlier(&self, pos: u64) -> io::Result<Option<Continuation>> {
        use rusqlite::OptionalExtension;
        let Some(db) = &self.db else {
            return Ok(None);
        };
        let row = db
            .prepare_cached("SELECT bytes, next, physical FROM earlier WHERE pos=?")
            .map_err(io::Error::other)?
            .query_row([pos as i64], |r| {
                Ok(Continuation {
                    bytes: r.get(0)?,
                    next: r.get::<_, Option<i64>>(1)?.map(|n| n as u64),
                    physical_next: r.get::<_, i64>(2)? as u64,
                })
            })
            .optional()
            .map_err(io::Error::other)?;
        if row.is_some() {
            db.prepare_cached("DELETE FROM earlier WHERE pos=?")
                .map_err(io::Error::other)?
                .execute([pos as i64])
                .map_err(io::Error::other)?;
        }
        Ok(row)
    }
    fn disk_bytes(&self) -> io::Result<u64> {
        let Some(db) = &self.db else {
            return Ok(0);
        };
        let pages: i64 = db
            .query_row("PRAGMA page_count", [], |r| r.get(0))
            .map_err(io::Error::other)?;
        let size: i64 = db
            .query_row("PRAGMA page_size", [], |r| r.get(0))
            .map_err(io::Error::other)?;
        Ok((pages * size) as u64)
    }
}
impl Drop for FragmentSpill {
    fn drop(&mut self) {
        self.db.take();
        if let Some(path) = &self.path {
            let _ = std::fs::remove_file(path);
        }
    }
}

pub(crate) struct FragmentStore {
    pending: BTreeMap<u64, PendingRow>,
    pending_bytes: usize,
    earlier: std::collections::BTreeMap<u64, Continuation>,
    earlier_bytes: usize,
    pub(crate) spill: FragmentSpill,
}
impl FragmentStore {
    pub(crate) fn new(dir: &Path) -> Self {
        Self {
            pending: BTreeMap::new(),
            pending_bytes: 0,
            earlier: std::collections::BTreeMap::new(),
            earlier_bytes: 0,
            spill: FragmentSpill::new(dir),
        }
    }
    pub(crate) fn resolve(
        &mut self,
        mut target: u64,
        current_pos: u64,
        mut row: PendingRow,
    ) -> io::Result<Option<PendingRow>> {
        loop {
            if row.bytes.len() == row.expected {
                return Ok(Some(row));
            }
            if target >= current_pos {
                if self.spill.has_pending(Some(target))? {
                    return Err(invalid_at(
                        target,
                        "two rows point at the same continuation",
                    ));
                }
                self.pending_bytes += row.bytes.len();
                if self.pending.insert(target, row).is_some() {
                    return Err(invalid_at(
                        target,
                        "two fragmented rows point at the same block",
                    ));
                }
                return Ok(None);
            }
            let fragment = if let Some(fragment) = self.earlier.remove(&target) {
                self.earlier_bytes = self.earlier_bytes.saturating_sub(fragment.bytes.len());
                fragment
            } else {
                self.spill
                    .take_earlier(target)?
                    .ok_or_else(|| invalid_at(target, "missing backward continuation"))?
            };
            let left = row.expected.saturating_sub(row.bytes.len());
            if fragment.bytes.len() > left {
                return Err(invalid_at(
                    target,
                    "continuation exceeds the remaining row length",
                ));
            }
            row.bytes.extend_from_slice(&fragment.bytes);
            if row.bytes.len() == row.expected {
                return Ok(Some(row));
            }
            target = fragment.next.unwrap_or(fragment.physical_next);
        }
    }
}

/// Walk a dynamic `.MYD` as a forward stream, retaining only fragmented rows that have not yet
/// reached their next block. This avoids seeking or spooling the whole data member to disk.
#[cfg(test)]
pub fn walk_dynamic_forward<R: io::Read>(
    reader: R,
    length: u64,
    on_record: impl FnMut(&[u8]) -> io::Result<()>,
) -> io::Result<WalkStats> {
    walk_dynamic_forward_limited(reader, length, None, &std::env::temp_dir(), on_record)
}

#[cfg(test)]
pub(crate) fn walk_dynamic_forward_limited<R: io::Read>(
    reader: R,
    length: u64,
    limit: Option<u64>,
    scratch_dir: &Path,
    on_record: impl FnMut(&[u8]) -> io::Result<()>,
) -> io::Result<WalkStats> {
    let options = WalkOptions {
        row_limit: limit,
        scratch_dir: scratch_dir.into(),
        ..WalkOptions::default()
    };
    walk_dynamic_with_options(reader, length, &options, on_record)
}

fn walk_dynamic_with_options<R: io::Read>(
    mut reader: R,
    length: u64,
    options: &WalkOptions,
    mut on_record: impl FnMut(&[u8]) -> io::Result<()>,
) -> io::Result<WalkStats> {
    let mut stats = WalkStats::default();
    let mut fragments = FragmentStore::new(&options.scratch_dir);
    let mut pos = 0u64;

    while pos < length && !options.row_limit.is_some_and(|n| stats.records >= n) {
        let mut header = [0u8; 20];
        reader.read_exact(&mut header[..1])?;
        let header_len = match header[0] {
            0 => 20,
            1 | 7 => 3,
            2 | 3 | 8 | 9 => 4,
            4 | 10 => 5,
            5 => 13,
            6 => 15,
            11 => 11,
            12 => 12,
            13 => 16,
            kind => {
                return Err(invalid_at(
                    pos,
                    format!("unknown dynamic block type {kind}"),
                ));
            }
        };
        if pos + header_len > length {
            return Err(invalid_at(pos, "truncated dynamic block header"));
        }
        reader.read_exact(&mut header[1..header_len as usize])?;
        let block = parse_block_header(header, pos, length)?;
        if block.data_len > options.max_record_bytes as u64 {
            return Err(invalid_at(
                pos,
                "dynamic block exceeds the record safety limit",
            ));
        }
        let mut payload = vec![0; block.data_len as usize];
        reader.read_exact(&mut payload)?;

        match block.kind {
            BlockKind::Deleted => stats.deleted_blocks += 1,
            BlockKind::Record => {
                let record_len = block.record_len as usize;
                if record_len > options.max_record_bytes || payload.len() > record_len {
                    return Err(invalid_at(pos, "invalid or oversized MyISAM record length"));
                }
                if payload.len() == record_len {
                    on_record(&payload)?;
                    stats.records += 1;
                } else {
                    let next = block.next.ok_or_else(|| {
                        invalid_at(pos, "fragmented row has no next-block pointer")
                    })?;
                    let row = PendingRow {
                        expected: record_len,
                        bytes: payload,
                    };
                    if let Some(row) = fragments.resolve(next, block.next_pos, row)? {
                        on_record(&row.bytes)?;
                        stats.records += 1;
                        stats.fragmented_records += 1;
                    }
                }
            }
            BlockKind::Continuation => {
                stats.continuation_blocks += 1;
                let record = if let Some(record) = fragments.pending.remove(&pos) {
                    fragments.pending_bytes -= record.bytes.len();
                    Some(record)
                } else {
                    fragments.spill.take_pending(pos)?
                };
                if let Some(mut record) = record {
                    if payload.len() > record.expected.saturating_sub(record.bytes.len()) {
                        return Err(invalid_at(pos, "continuation exceeds the record length"));
                    }
                    record.bytes.extend_from_slice(&payload);
                    if let Some(record) = fragments.resolve(
                        block.next.unwrap_or(block.next_pos),
                        block.next_pos,
                        record,
                    )? {
                        on_record(&record.bytes)?;
                        stats.records += 1;
                        stats.fragmented_records += 1;
                    }
                } else {
                    fragments.earlier_bytes += payload.len();
                    fragments.earlier.insert(
                        pos,
                        Continuation {
                            bytes: payload,
                            next: block.next,
                            physical_next: block.next_pos,
                        },
                    );
                }
            }
        }

        let used = header_len + block.data_len;
        let physical = block.next_pos - pos;
        if physical < used {
            return Err(invalid_at(
                pos,
                "dynamic block payload exceeds physical block",
            ));
        }
        let mut skip = physical - used;
        let mut discard = [0u8; 8192];
        while skip > 0 {
            let n = skip.min(discard.len() as u64) as usize;
            reader.read_exact(&mut discard[..n])?;
            skip -= n as u64;
        }
        pos = block.next_pos;
        stats.bytes_consumed = pos;
        while let Some((&oldest, _)) = fragments.earlier.first_key_value() {
            if pos.saturating_sub(oldest) <= options.lookback_bytes
                && fragments.earlier_bytes <= options.earlier_bytes
            {
                break;
            }
            let (offset, removed) = fragments.earlier.pop_first().unwrap();
            fragments.earlier_bytes = fragments.earlier_bytes.saturating_sub(removed.bytes.len());
            fragments.spill.put_earlier(offset, removed)?;
        }
        while fragments.pending_bytes > options.pending_bytes {
            // Spill the most distant future fragment in O(log n). Retain rows that will
            // complete soon; scanning every pending row here makes large imports quadratic.
            let (target, row) = fragments.pending.pop_last().unwrap();
            fragments.pending_bytes -= row.bytes.len();
            fragments.spill.put_pending(target, row)?;
        }
    }
    if pos == length && (!fragments.pending.is_empty() || fragments.spill.has_pending(None)?) {
        return Err(invalid(
            "end of .MYD reached with incomplete fragmented rows",
        ));
    }
    stats.spill_bytes_written = fragments.spill.written;
    stats.spill_disk_bytes = fragments.spill.disk_bytes()?;
    Ok(stats)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BlockKind {
    Deleted,
    Record,
    Continuation,
}

#[derive(Debug)]
struct Block {
    kind: BlockKind,
    next_pos: u64,
    record_len: u32,
    data_len: u64,
    next: Option<u64>,
}

fn parse_block_header(h: [u8; 20], pos: u64, file_len: u64) -> io::Result<Block> {
    let typ = h[0];
    let (kind, block_len, record_len, data_len, payload_offset, next) = match typ {
        0 => (BlockKind::Deleted, be_u24(&h, 1)? as u64, 0, 0, 0, None),
        1 => {
            let n = be_u16(&h, 1)? as u64;
            (BlockKind::Record, n, n as u32, n, 3, None)
        }
        2 => {
            let n = be_u24(&h, 1)? as u64;
            (BlockKind::Record, n, n as u32, n, 4, None)
        }
        3 => {
            let n = be_u16(&h, 1)? as u64;
            (BlockKind::Record, n + h[3] as u64, n as u32, n, 4, None)
        }
        4 => {
            let n = be_u24(&h, 1)? as u64;
            (BlockKind::Record, n + h[4] as u64, n as u32, n, 5, None)
        }
        5 => {
            let n = be_u16(&h, 1)? as u64;
            let chunk = be_u16(&h, 3)? as u64;
            (
                BlockKind::Record,
                chunk,
                n as u32,
                chunk,
                13,
                Some(be_u64(&h, 5)?),
            )
        }
        6 => {
            let n = be_u24(&h, 1)? as u64;
            let chunk = be_u24(&h, 4)? as u64;
            (
                BlockKind::Record,
                chunk,
                n as u32,
                chunk,
                15,
                Some(be_u64(&h, 7)?),
            )
        }
        7 => {
            let n = be_u16(&h, 1)? as u64;
            (BlockKind::Continuation, n, 0, n, 3, None)
        }
        8 => {
            let n = be_u24(&h, 1)? as u64;
            (BlockKind::Continuation, n, 0, n, 4, None)
        }
        9 => {
            let n = be_u16(&h, 1)? as u64;
            (BlockKind::Continuation, n + h[3] as u64, 0, n, 4, None)
        }
        10 => {
            let n = be_u24(&h, 1)? as u64;
            (BlockKind::Continuation, n + h[4] as u64, 0, n, 5, None)
        }
        11 => {
            let n = be_u16(&h, 1)? as u64;
            (BlockKind::Continuation, n, 0, n, 11, Some(be_u64(&h, 3)?))
        }
        12 => {
            let n = be_u24(&h, 1)? as u64;
            (BlockKind::Continuation, n, 0, n, 12, Some(be_u64(&h, 4)?))
        }
        13 => {
            let rec_len = be_u32(&h, 1)?;
            let chunk = be_u24(&h, 5)? as u64;
            (
                BlockKind::Record,
                chunk,
                rec_len,
                chunk,
                16,
                Some(be_u64(&h, 8)?),
            )
        }
        _ => return Err(invalid_at(pos, format!("unknown dynamic block type {typ}"))),
    };
    if data_len > block_len && !matches!(typ, 5..=6 | 13) {
        return Err(invalid_at(pos, "payload size exceeds dynamic block length"));
    }
    let payload_pos = pos + payload_offset;
    let next_pos = if typ == 0 {
        pos + block_len
    } else {
        payload_pos + block_len
    };
    if block_len == 0 || next_pos > file_len {
        return Err(invalid_at(pos, "invalid dynamic block length"));
    }
    Ok(Block {
        kind,
        next_pos,
        record_len,
        data_len,
        next: next.filter(|p| *p != u64::MAX),
    })
}
