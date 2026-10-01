//! Explicit, optional fixture refresh. Normal tests are offline and read checked-in bytes.
#[allow(dead_code)] // Shared production module exposes more than this tool uses.
#[path = "../src/ingest/ftp_range.rs"]
mod ftp_range;
#[cfg(test)]
#[path = "../src/ingest/test_fixture.rs"]
mod test_fixture;

use myisam_reader::{FrmSchema, MyisamInfo, read_myi_header, value, walk_records};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::{self, Read},
    path::PathBuf,
    sync::Arc,
};

struct Capture<R> {
    inner: R,
    bytes: Vec<u8>,
}
impl<R: Read> Read for Capture<R> {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        let n = self.inner.read(out)?;
        self.bytes.extend_from_slice(&out[..n]);
        Ok(n)
    }
}
fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
fn hash(bytes: &[u8]) -> String {
    hex(&Sha256::digest(bytes))
}
fn selected(table: &str) -> &'static [&'static str] {
    match table {
        "editions" => &["e_id", "title", "author", "libgen_topic", "year", "visible"],
        "editions_add_descr" => &["e_id", "key", "value"],
        "editions_to_files" => &["e_id", "f_id"],
        "files" => &[
            "f_id",
            "md5",
            "extension",
            "filesize",
            "pages",
            "libgen_id",
            "fiction_id",
            "libgen_topic",
            "broken",
            "visible",
        ],
        "elem_descr" => &["key", "name_en"],
        _ => unreachable!(),
    }
}
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    if args.len() != 5 {
        return Err("usage: capture-myisam-fixtures FIRST_FTP_URL VOLUMES CACHE_DIR OUTPUT_DIR ROWS (positive)".into());
    }
    let first = &args[0];
    let volumes: usize = args[1].parse()?;
    let output = PathBuf::from(&args[3]);
    let rows: u64 = args[4].parse()?;
    if rows == 0 || volumes == 0 {
        return Err("rows and volumes must be positive".into());
    }
    if volumes > 1 && !first.contains("part001.rar") {
        return Err("split archive URL must contain part001.rar".into());
    }
    fs::create_dir_all(&output)?;
    let cache = Arc::new(ftp_range::DiskRangeCache::open(&args[2], 64 * 1024 * 1024)?);
    let urls = (1..=volumes)
        .map(|n| first.replace("part001.rar", &format!("part{n:03}.rar")))
        .collect::<Vec<_>>();
    let member = |name: &str| ftp_range::stream_named_member(urls.clone(), cache.clone(), name);
    let mut cases = Vec::new();
    for table in [
        "editions",
        "editions_add_descr",
        "editions_to_files",
        "files",
        "elem_descr",
    ] {
        let (_, _, mut stream, worker) = member(&format!("{table}.frm"))?;
        let mut frm = Vec::new();
        stream
            .by_ref()
            .take(1024 * 1024 + 1)
            .read_to_end(&mut frm)?;
        drop(stream);
        worker.join().map_err(|_| "schema extractor panicked")??;
        if frm.len() > 1024 * 1024 {
            return Err("schema exceeds size limit".into());
        }
        let (_, _, stream, worker) = member(&format!("{table}.MYI"))?;
        let myi = read_myi_header(stream)?;
        let _ = worker.join().map_err(|_| "index extractor panicked")?; // Intentionally read header only.
        let schema = FrmSchema::parse(&frm)?;
        let info = MyisamInfo::parse_myi(&myi)?;
        let decoder = schema.decoder(&info)?;
        let (_, length, stream, worker) = member(&format!("{table}.MYD"))?;
        let mut captured = Capture {
            inner: stream,
            bytes: Vec::new(),
        };
        let mut digest = Sha256::new();
        let mut samples = Vec::new();
        let stats = walk_records(&mut captured, length, &info, Some(rows), |packed| {
            let decoded = decoder.unpack(packed)?;
            for field in &decoded {
                if let Some(bytes) = field {
                    digest.update([1]);
                    digest.update((bytes.len() as u64).to_le_bytes());
                    digest.update(bytes);
                } else {
                    digest.update([0]);
                }
            }
            if samples.len() < 8 {
                let mut sample = serde_json::Map::new();
                for column in selected(table) {
                    sample.insert(
                        (*column).into(),
                        serde_json::json!(value(&schema, &decoded, column)?),
                    );
                }
                samples.push(sample);
            }
            Ok(())
        })?;
        let Capture { inner, bytes: myd } = captured;
        drop(inner);
        let extraction = worker.join().map_err(|_| "data extractor panicked")?;
        let complete = stats.bytes_consumed == length;
        if complete {
            extraction?;
        }
        fs::write(output.join(format!("{table}.frm")), &frm)?;
        fs::write(output.join(format!("{table}.MYI.header")), &myi)?;
        fs::write(output.join(format!("{table}.MYD")), &myd)?;
        let columns = schema
            .columns
            .iter()
            .map(|c| {
                serde_json::json!({"name": c.name, "offset": c.offset, "length": c.length,
            "type_code": c.type_code, "flags": c.flags, "enum_values": c.enum_values})
            })
            .collect::<Vec<_>>();
        cases.push(serde_json::json!({"table":table, "complete_data":complete, "source_data_bytes":length,
            "source_rows":info.record_count, "rows":stats.records, "bytes":stats.bytes_consumed,
            "fragmented_records":stats.fragmented_records, "continuation_blocks":stats.continuation_blocks,
            "deleted_blocks":stats.deleted_blocks, "null_fields":schema.null_fields, "columns":columns,
            "options":info.options, "pack_bits":info.pack_bits, "record_len":info.record_len,
            "sha256": {"frm":hash(&frm), "myi":hash(&myi), "myd":hash(&myd), "decoded_rows":hex(&digest.finalize())},
            "samples":samples}));
        println!(
            "{table}: {} rows, {} bytes, complete={complete}",
            stats.records,
            myd.len()
        );
    }
    let captured_at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_secs();
    let manifest = serde_json::json!({"source_url":first, "volumes":volumes, "captured_at_unix_seconds":captured_at, "row_limit":rows,
        "oracle":"Golden decoded rows captured from the native reader; independent MySQL reference checks are separate.", "tables":cases});
    fs::write(
        output.join("manifest.json"),
        serde_json::to_vec_pretty(&manifest)?,
    )?;
    println!("cache: {:?}", cache.stats());
    Ok(())
}
