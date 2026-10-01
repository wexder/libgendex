//! Offline regression harness. Expectations and fixture bytes are checked into the repository.
use myisam_reader::{
    FrmSchema, MyisamInfo, WalkOptions, read_myi_header, value, walk_records,
    walk_records_with_options,
};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs,
    io::{self, Read},
    path::PathBuf,
};

type Sample = BTreeMap<String, Option<String>>;
#[derive(Deserialize)]
struct Manifest {
    tables: Vec<Case>,
}
#[derive(Deserialize)]
struct Case {
    table: String,
    complete_data: bool,
    source_data_bytes: u64,
    source_rows: u64,
    rows: u64,
    bytes: u64,
    fragmented_records: u64,
    continuation_blocks: u64,
    deleted_blocks: u64,
    null_fields: usize,
    columns: Vec<Column>,
    options: u16,
    pack_bits: u16,
    record_len: u32,
    sha256: Hashes,
    samples: Vec<Sample>,
}
#[derive(Deserialize)]
struct Column {
    name: String,
    offset: usize,
    length: usize,
    type_code: u8,
    flags: u16,
    enum_values: Vec<String>,
}
#[derive(Deserialize)]
struct Hashes {
    frm: String,
    myi: String,
    myd: String,
    decoded_rows: String,
}
#[derive(Deserialize)]
struct Reference {
    engine: String,
    tables: Vec<ReferenceTable>,
}
#[derive(Deserialize)]
struct ReferenceTable {
    table: String,
    rows: Vec<Sample>,
}
fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/libgen-2026-09-06")
}
fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
fn hash(bytes: &[u8]) -> String {
    hex(&Sha256::digest(bytes))
}
fn case(table: &str) -> Case {
    let manifest: Manifest =
        serde_json::from_slice(&fs::read(root().join("manifest.json")).unwrap()).unwrap();
    manifest
        .tables
        .into_iter()
        .find(|c| c.table == table)
        .unwrap()
}
struct ShortReads<'a> {
    bytes: &'a [u8],
    max: usize,
}
impl Read for ShortReads<'_> {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        let n = out.len().min(self.max).min(self.bytes.len());
        out[..n].copy_from_slice(&self.bytes[..n]);
        self.bytes = &self.bytes[n..];
        Ok(n)
    }
}
fn verify(table: &str) {
    let case = case(table);
    let frm = fs::read(root().join(format!("{table}.frm"))).unwrap();
    let myi = fs::read(root().join(format!("{table}.MYI.header"))).unwrap();
    let myd = fs::read(root().join(format!("{table}.MYD"))).unwrap();
    assert_eq!(
        hash(&frm),
        case.sha256.frm,
        "{table}: fixture schema checksum"
    );
    assert_eq!(
        hash(&myi),
        case.sha256.myi,
        "{table}: fixture index checksum"
    );
    assert_eq!(
        hash(&myd),
        case.sha256.myd,
        "{table}: fixture data checksum"
    );
    let schema = FrmSchema::parse(&frm).unwrap();
    let info = MyisamInfo::parse_myi(&myi).unwrap();
    assert_eq!(
        read_myi_header(ShortReads {
            bytes: &myi,
            max: 1
        })
        .unwrap(),
        myi
    );
    let mut with_index_body = myi.clone();
    with_index_body.extend(vec![0xa5; 1024 * 1024]);
    let mut index_source = io::Cursor::new(with_index_body);
    assert_eq!(read_myi_header(&mut index_source).unwrap(), myi);
    assert_eq!(
        index_source.position(),
        myi.len() as u64,
        "{table}: index body must remain unread"
    );
    assert_eq!(schema.null_fields, case.null_fields);
    assert_eq!(info.record_count, case.source_rows);
    assert_eq!(info.options, case.options);
    assert_eq!(info.pack_bits, case.pack_bits);
    assert_eq!(info.record_len, case.record_len);
    if case.complete_data {
        assert_eq!(myd.len() as u64, case.source_data_bytes);
    } else {
        assert!((myd.len() as u64) < case.source_data_bytes);
        assert!(case.rows < case.source_rows);
    }
    assert_eq!(schema.pack_records, info.options & 1 != 0);
    assert_eq!(schema.columns.len(), case.columns.len());
    for (actual, expected) in schema.columns.iter().zip(&case.columns) {
        assert_eq!(
            (
                &actual.name,
                actual.offset,
                actual.length,
                actual.type_code,
                actual.flags,
                &actual.enum_values
            ),
            (
                &expected.name,
                expected.offset,
                expected.length,
                expected.type_code,
                expected.flags,
                &expected.enum_values
            )
        );
    }
    let decoder = schema.decoder(&info).unwrap();
    // Includes one-byte reads and reads that cross both header and field boundaries.
    for max in [1, 7, 64, 4096, usize::MAX] {
        let mut digest = Sha256::new();
        let mut rows = 0usize;
        let stats = walk_records(
            ShortReads { bytes: &myd, max },
            myd.len() as u64,
            &info,
            None,
            |packed| {
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
                if let Some(expected) = case.samples.get(rows) {
                    for (column, expected) in expected {
                        assert_eq!(
                            &value(&schema, &decoded, column)?,
                            expected,
                            "{table} row {rows} column {column}"
                        );
                    }
                }
                rows += 1;
                Ok(())
            },
        )
        .unwrap_or_else(|e| panic!("{table}, read size {max}: {e}"));
        assert_eq!(stats.records, case.rows, "{table}");
        assert_eq!(stats.bytes_consumed, case.bytes);
        assert_eq!(stats.fragmented_records, case.fragmented_records);
        assert_eq!(stats.continuation_blocks, case.continuation_blocks);
        assert_eq!(stats.deleted_blocks, case.deleted_blocks);
        assert_eq!(
            hex(&digest.finalize()),
            case.sha256.decoded_rows,
            "{table}: decoded row checksum"
        );
        if case.complete_data {
            assert_eq!(stats.records, info.record_count);
        }
    }
    let names = case.samples[0]
        .keys()
        .map(String::as_str)
        .collect::<Vec<_>>();
    let projected = schema.decoder(&info).unwrap().project(&names).unwrap();
    walk_records(myd.as_slice(), myd.len() as u64, &info, None, |packed| {
        let full = decoder.unpack(packed)?;
        let selected = projected.unpack(packed)?;
        for (i, column) in schema.columns.iter().enumerate() {
            if names.contains(&column.name.as_str()) {
                assert_eq!(selected[i], full[i]);
            } else {
                assert!(selected[i].is_none());
            }
        }
        Ok(())
    })
    .unwrap();
    assert!(
        schema
            .decoder(&info)
            .unwrap()
            .project(&["missing_column"])
            .is_err()
    );
    // Independent SQL output from MariaDB, generated once and checked in. Compare multisets:
    // streaming reconstruction may emit fragmented rows in a different physical order.
    let reference: Reference =
        serde_json::from_slice(&fs::read(root().join("mysql-reference.json")).unwrap()).unwrap();
    assert!(reference.engine.contains("MariaDB"));
    let expected = reference
        .tables
        .into_iter()
        .find(|r| r.table == table)
        .unwrap();
    let mut actual = Vec::new();
    walk_records(myd.as_slice(), myd.len() as u64, &info, None, |packed| {
        let decoded = decoder.unpack(packed)?;
        let mut fields = Sample::new();
        for name in &names {
            fields.insert((*name).to_owned(), value(&schema, &decoded, name)?);
        }
        actual.push(fields);
        Ok(())
    })
    .unwrap();
    let mut actual = actual
        .into_iter()
        .map(|row| serde_json::to_string(&row).unwrap())
        .collect::<Vec<_>>();
    let mut expected = expected
        .rows
        .into_iter()
        .map(|row| serde_json::to_string(&row).unwrap())
        .collect::<Vec<_>>();
    actual.sort();
    expected.sort();
    assert_eq!(
        actual.len(),
        expected.len(),
        "{table}: independent SQL row count"
    );
    for (row, (actual, expected)) in actual.iter().zip(&expected).enumerate() {
        assert_eq!(actual, expected, "{table}: independent SQL row {row}");
    }
    // Force the real fragments through disk with zero retention budgets. Results must remain
    // identical to the RAM path, and scratch storage must disappear after success or error.
    let scratch = tempfile::tempdir().unwrap();
    let options = WalkOptions {
        scratch_dir: scratch.path().into(),
        pending_bytes: 0,
        earlier_bytes: 0,
        lookback_bytes: 0,
        ..WalkOptions::default()
    };
    let mut digest = Sha256::new();
    let stats = walk_records_with_options(
        myd.as_slice(),
        myd.len() as u64,
        &info,
        &options,
        |packed| {
            for field in decoder.unpack(packed)? {
                if let Some(bytes) = field {
                    digest.update([1]);
                    digest.update((bytes.len() as u64).to_le_bytes());
                    digest.update(bytes);
                } else {
                    digest.update([0]);
                }
            }
            Ok(())
        },
    )
    .unwrap();
    assert_eq!(hex(&digest.finalize()), case.sha256.decoded_rows);
    assert_eq!(stats.records, case.rows);
    assert_eq!(fs::read_dir(scratch.path()).unwrap().count(), 0);
    if case.fragmented_records > 0 {
        assert!(stats.spill_bytes_written > 0);
        let failed =
            walk_records_with_options(myd.as_slice(), myd.len() as u64, &info, &options, |_| {
                if fs::read_dir(scratch.path())?.next().is_some() {
                    return Err(io::Error::other("intentional consumer failure"));
                }
                Ok(())
            });
        assert!(failed.is_err());
        assert_eq!(fs::read_dir(scratch.path()).unwrap().count(), 0);
    }
    // Physical truncation must fail, rather than silently returning a shortened table.
    assert!(
        walk_records(
            &myd[..myd.len() - 1],
            myd.len() as u64,
            &info,
            None,
            |_| Ok(())
        )
        .is_err()
    );
    assert!(FrmSchema::parse(&frm[..64]).is_err());
    assert!(MyisamInfo::parse_myi(&myi[..myi.len() - 1]).is_err());
}
macro_rules! fixture_test {
    ($name:ident) => {
        #[test]
        fn $name() {
            verify(stringify!($name));
        }
    };
}
fixture_test!(editions);
fixture_test!(editions_add_descr);
fixture_test!(editions_to_files);
fixture_test!(files);
fixture_test!(elem_descr);
