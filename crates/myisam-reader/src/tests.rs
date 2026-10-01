use super::*;
use crate::records::{
    Continuation, FragmentStore, PendingRow, walk_dynamic_forward, walk_dynamic_forward_limited,
};
use std::collections::HashMap;
use std::{fs, path::PathBuf};

struct TempDir(PathBuf);
impl TempDir {
    fn new(label: &str) -> Self {
        static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "myisam-test-{label}-{}-{}",
            std::process::id(),
            SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }
}
impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn layout(defs: &[(i16, usize)]) -> (FrmSchema, MyisamInfo) {
    let mut offset = 0;
    let columns = defs
        .iter()
        .enumerate()
        .map(|(i, (_, length))| {
            let col = FrmColumn {
                name: format!("c{i}"),
                offset,
                length: *length,
                flags: 0,
                type_code: 253,
                enum_values: vec![],
            };
            offset += length;
            col
        })
        .collect::<Vec<_>>();
    let positions = columns
        .iter()
        .enumerate()
        .map(|(i, c)| (c.name.clone(), i))
        .collect();
    let schema = FrmSchema {
        columns,
        positions,
        null_fields: 0,
        pack_records: true,
    };
    let bits = defs
        .iter()
        .filter(|(k, _)| matches!(k, 1..=4))
        .count()
        .div_ceil(8);
    let info = MyisamInfo {
        options: 1,
        header_len: 0,
        base_pos: 0,
        record_count: 0,
        fields: defs.len() as u32,
        packed_fields: 0,
        record_len: offset as u32,
        packed_record_len: 0,
        min_packed_len: 0,
        pack_bits: bits as u16,
        recdefs: defs
            .iter()
            .map(|(kind, length)| MyisamColumnDef {
                kind: *kind,
                length: *length,
                null_bit: 0,
                null_pos: 0,
            })
            .collect(),
    };
    (schema, info)
}

#[test]
fn packing_bitmap_controls_space_zero_and_blob_fields() {
    let (schema, info) = layout(&[(1, 5), (2, 5), (3, 4), (4, 10)]);
    let row = schema
        .unpack(
            &[0b0111, 2, b'a', b'b', 2, b'c', b'd', 3, 0, b'x', b'y', b'z'],
            &info,
        )
        .unwrap();
    assert_eq!(
        row,
        vec![
            Some(b"ab   ".to_vec()),
            Some(b"   cd".to_vec()),
            Some(vec![0; 4]),
            Some(b"xyz".to_vec())
        ]
    );
    let row = schema
        .unpack(
            &[
                0b1000, b'a', b'b', b'c', b'd', b'e', b'f', b'g', b'h', b'i', b'j', 1, 2, 3, 4,
            ],
            &info,
        )
        .unwrap();
    assert_eq!(
        row,
        vec![
            Some(b"abcde".to_vec()),
            Some(b"fghij".to_vec()),
            Some(vec![1, 2, 3, 4]),
            Some(vec![])
        ]
    );
}

#[test]
fn varchar_extended_length_is_big_endian_and_short_255_is_literal() {
    let (schema, info) = layout(&[(8, 1026), (8, 256)]);
    let mut bytes = vec![255, 1, 44]; // 300, not little-endian 11265
    bytes.extend(vec![b'a'; 300]);
    bytes.push(255);
    bytes.extend(vec![b'b'; 255]);
    let row = schema.unpack(&bytes, &info).unwrap();
    assert_eq!(row[0], Some(vec![b'a'; 300]));
    assert_eq!(row[1], Some(vec![b'b'; 255]));
    assert!(schema.unpack(&bytes[..bytes.len() - 1], &info).is_err());
}

#[test]
fn hidden_null_byte_maps_columns_by_record_offset() {
    let (mut schema, mut info) = layout(&[(0, 1), (8, 10)]);
    schema.columns.remove(0);
    schema.positions = [("c1".to_string(), 0)].into();
    info.recdefs[1].null_bit = 2;
    assert_eq!(schema.unpack(&[2, 0], &info).unwrap(), vec![None]);
    assert_eq!(
        schema.unpack(&[0, 2, b'o', b'k'], &info).unwrap(),
        vec![Some(b"ok".to_vec())]
    );
}

#[test]
fn projection_and_copy_limit_still_validate_skipped_fields() {
    let (schema, info) = layout(&[(8, 1026), (8, 1026)]);
    let packed = [2, b'o', b'k', 4, b'l', b'o', b'n', b'g'];
    let decoder = schema.decoder(&info).unwrap().project(&["c0"]).unwrap();
    assert_eq!(
        decoder.unpack(&packed).unwrap(),
        vec![Some(b"ok".to_vec()), None]
    );
    assert!(decoder.unpack(&packed[..packed.len() - 1]).is_err());
    let limited = schema.decoder(&info).unwrap().with_value_limit(3);
    assert_eq!(
        limited.unpack(&packed).unwrap(),
        vec![Some(b"ok".to_vec()), None]
    );
    assert!(limited.unpack(&packed[..packed.len() - 1]).is_err());
    let (schema, info) = layout(&[(1, 100)]);
    assert_eq!(
        schema
            .decoder(&info)
            .unwrap()
            .with_value_limit(3)
            .unpack(&[1, 2, b'o', b'k'])
            .unwrap(),
        vec![None]
    );
}

#[test]
fn sql_scalar_preserves_varchar_whitespace_and_only_trims_char_right_padding() {
    let (mut schema, info) = layout(&[(8, 20), (0, 6)]);
    schema.columns[1].type_code = 254;
    let row = schema
        .unpack(
            &[
                4, b' ', b'x', b' ', b'\t', b' ', b'a', b'b', b' ', b' ', b' ',
            ],
            &info,
        )
        .unwrap();
    assert_eq!(
        value(&schema, &row, "c0").unwrap().as_deref(),
        Some(" x \t")
    );
    assert_eq!(value(&schema, &row, "c1").unwrap().as_deref(), Some(" ab"));
}

#[test]
fn enum_and_set_scalar_values_use_schema_labels() {
    let (mut schema, info) = layout(&[(0, 1), (0, 1)]);
    schema.columns[0].type_code = 247;
    schema.columns[0].enum_values = vec!["N".into(), "Y".into()];
    schema.columns[1].type_code = 248;
    schema.columns[1].enum_values = vec!["a".into(), "b".into(), "c".into()];
    let row = schema.unpack(&[2, 5], &info).unwrap();
    assert_eq!(value(&schema, &row, "c0").unwrap().as_deref(), Some("Y"));
    assert_eq!(value(&schema, &row, "c1").unwrap().as_deref(), Some("a,c"));
    let row = schema.unpack(&[3, 8], &info).unwrap();
    assert!(value(&schema, &row, "c0").is_err());
    assert!(value(&schema, &row, "c1").is_err());
}

#[test]
fn unsupported_scalar_conversion_is_explicit() {
    let (mut schema, info) = layout(&[(0, 4)]);
    schema.columns[0].type_code = 4; // FLOAT is decoded physically but has no scalar formatter.
    let row = schema.unpack(&[0; 4], &info).unwrap();
    assert!(value(&schema, &row, "c0").is_err());
    assert!(number(&schema, &row, "c0").is_err());
}

fn fragmented_head(total: u16, payload: &[u8], next: u64) -> Vec<u8> {
    let mut bytes = vec![5];
    bytes.extend(total.to_be_bytes());
    bytes.extend((payload.len() as u16).to_be_bytes());
    bytes.extend(next.to_be_bytes());
    bytes.extend(payload);
    bytes
}

#[test]
fn forward_backward_and_missing_fragments() {
    let mut forward = fragmented_head(4, b"ab", 15);
    forward.extend([7, 0, 2, b'c', b'd']);
    let mut rows = vec![];
    let stats = walk_dynamic_forward(forward.as_slice(), forward.len() as u64, |r| {
        rows.push(r.to_vec());
        Ok(())
    })
    .unwrap();
    assert_eq!(rows, [b"abcd"]);
    assert_eq!(stats.fragmented_records, 1);
    let mut backward = vec![7, 0, 2, b'c', b'd'];
    backward.extend(fragmented_head(4, b"ab", 0));
    rows.clear();
    walk_dynamic_forward(backward.as_slice(), backward.len() as u64, |r| {
        rows.push(r.to_vec());
        Ok(())
    })
    .unwrap();
    assert_eq!(rows, [b"abcd"]);
    assert!(walk_dynamic_forward(&forward[..15], 15, |_| Ok(())).is_err());
    assert!(walk_dynamic_forward(&[0u8; 20][..], 20, |_| Ok(())).is_err());
}

fn dynamic_block(kind: u8, payload: &[u8], total: usize, next: u64) -> Vec<u8> {
    let mut bytes = vec![kind];
    let len = payload.len() as u32;
    let be24 = |n: u32| n.to_be_bytes()[1..].to_vec();
    match kind {
        1 | 7 => bytes.extend((len as u16).to_be_bytes()),
        2 | 8 => bytes.extend(be24(len)),
        3 | 9 => {
            bytes.extend((len as u16).to_be_bytes());
            bytes.push(3);
        }
        4 | 10 => {
            bytes.extend(be24(len));
            bytes.push(3);
        }
        5 => {
            bytes.extend((total as u16).to_be_bytes());
            bytes.extend((len as u16).to_be_bytes());
            bytes.extend(next.to_be_bytes());
        }
        6 => {
            bytes.extend(be24(total as u32));
            bytes.extend(be24(len));
            bytes.extend(next.to_be_bytes());
        }
        11 => {
            bytes.extend((len as u16).to_be_bytes());
            bytes.extend(next.to_be_bytes());
        }
        12 => {
            bytes.extend(be24(len));
            bytes.extend(next.to_be_bytes());
        }
        13 => {
            bytes.extend((total as u32).to_be_bytes());
            bytes.extend(be24(len));
            bytes.extend(next.to_be_bytes());
        }
        _ => unreachable!(),
    }
    bytes.extend(payload);
    if matches!(kind, 3 | 4 | 9 | 10) {
        bytes.extend([0; 3]);
    }
    bytes
}

#[test]
fn every_dynamic_block_layout_and_padded_continuation_streams() {
    let mut deleted = vec![0, 0, 0, 20];
    deleted.resize(20, 0);
    for kind in 1..=4 {
        let mut stream = deleted.clone();
        stream.extend(dynamic_block(kind, b"abcd", 4, u64::MAX));
        let mut rows = vec![];
        let stats = walk_dynamic_forward(stream.as_slice(), stream.len() as u64, |r| {
            rows.push(r.to_vec());
            Ok(())
        })
        .unwrap();
        assert_eq!(rows, [b"abcd"]);
        assert_eq!(
            (stats.records, stats.deleted_blocks, stats.bytes_consumed),
            (1, 1, stream.len() as u64)
        );
    }
    for head in [5, 6, 13] {
        for middle in [11, 12] {
            for tail in [7, 8, 9, 10] {
                let head_len = dynamic_block(head, b"ab", 6, 0).len() as u64;
                let middle_len = dynamic_block(middle, b"cd", 0, 0).len() as u64;
                let mut stream = deleted.clone();
                stream.extend(dynamic_block(head, b"ab", 6, 20 + head_len));
                stream.extend(dynamic_block(middle, b"cd", 0, 20 + head_len + middle_len));
                stream.extend(dynamic_block(tail, b"ef", 0, u64::MAX));
                let mut rows = vec![];
                let stats = walk_dynamic_forward(stream.as_slice(), stream.len() as u64, |r| {
                    rows.push(r.to_vec());
                    Ok(())
                })
                .unwrap();
                assert_eq!(rows, [b"abcdef"], "layouts {head}/{middle}/{tail}");
                assert_eq!(stats.fragmented_records, 1);
                assert_eq!(stats.continuation_blocks, 2);
                assert_eq!(stats.deleted_blocks, 1);
                assert_eq!(stats.bytes_consumed, stream.len() as u64);
            }
        }
    }
}

#[test]
fn many_pending_rows_spill_in_order_and_complete_without_retaining_entire_input() {
    let scratch = TempDir::new("many-pending");
    let records = 10_000u64;
    let mut data = Vec::new();
    for i in 0..records {
        data.extend(fragmented_head(4, b"ab", 15 * records + 5 * i));
    }
    for _ in 0..records {
        data.extend([7, 0, 2, b'c', b'd']);
    }
    let (_, info) = layout(&[(0, 4)]);
    let options = WalkOptions {
        scratch_dir: scratch.0.clone(),
        pending_bytes: 8192,
        earlier_bytes: 0,
        lookback_bytes: 0,
        ..WalkOptions::default()
    };
    let stats =
        walk_records_with_options(data.as_slice(), data.len() as u64, &info, &options, |row| {
            assert_eq!(row, b"abcd");
            Ok(())
        })
        .unwrap();
    assert_eq!(stats.records, records);
    assert_eq!(stats.fragmented_records, records);
    assert_eq!(stats.continuation_blocks, records);
    assert!(stats.spill_bytes_written > 0);
    assert_eq!(stats.bytes_consumed, data.len() as u64);
    assert_eq!(fs::read_dir(&scratch.0).unwrap().count(), 0);
}

#[test]
fn distant_backward_fragment_spills_and_cleans_up() {
    let dir = TempDir::new("fragments");
    let mut data = vec![7, 0, 2, b'c', b'd'];
    // A deleted block makes the distance larger than the 16 MiB RAM lookback window.
    let size = 0xff_ffffusize;
    data.extend([0, 255, 255, 255]);
    data.resize(5 + size, 0);
    data.extend([1, 0, 2, b'x', b'y']);
    data.extend(fragmented_head(4, b"ab", 0));
    let mut rows = vec![];
    let stats =
        walk_dynamic_forward_limited(data.as_slice(), data.len() as u64, None, &dir.0, |r| {
            rows.push(r.to_vec());
            Ok(())
        })
        .unwrap();
    assert_eq!(rows, [b"xy".to_vec(), b"abcd".to_vec()]);
    assert_eq!(stats.deleted_blocks, 1);
    assert_eq!(stats.spill_bytes_written, 2);
    assert!(stats.spill_disk_bytes > 0);
    assert_eq!(fs::read_dir(&dir.0).unwrap().count(), 0);
    // Error after creating spill must also remove scratch storage.
    data.pop();
    assert!(
        walk_dynamic_forward_limited(data.as_slice(), data.len() as u64, None, &dir.0, |_| Ok(()))
            .is_err()
    );
    assert_eq!(fs::read_dir(&dir.0).unwrap().count(), 0);
}

#[test]
fn real_mysql_fixture_decodes_all_rows_and_descriptor_names() {
    let schema = FrmSchema::parse(include_bytes!(
        "../tests/fixtures/libgen-2026-09-06/elem_descr.frm"
    ))
    .unwrap();
    let header = include_bytes!("../tests/fixtures/libgen-2026-09-06/elem_descr.MYI.header");
    let info = MyisamInfo::parse_myi(header).unwrap();
    let decoder = schema.decoder(&info).unwrap();
    let data = include_bytes!("../tests/fixtures/libgen-2026-09-06/elem_descr.MYD");
    let mut keys = HashMap::new();
    let stats = walk_records(data.as_slice(), data.len() as u64, &info, None, |r| {
        let row = decoder.unpack(r)?;
        keys.insert(
            number(&schema, &row, "key")?.unwrap(),
            value(&schema, &row, "name_en")?.unwrap(),
        );
        Ok(())
    })
    .unwrap();
    assert_eq!(stats.records, 217);
    assert_eq!(stats.records, info.record_count);
    assert_eq!(stats.fragmented_records, 38);
    assert_eq!(stats.continuation_blocks, 57);
    assert_eq!(stats.deleted_blocks, 4);
    assert_eq!(keys[&101].to_lowercase(), "language");
    assert_eq!(keys[&505].to_lowercase(), "isbn");
    assert_eq!(read_myi_header(header.as_slice()).unwrap(), header);
}

#[test]
fn fixed_records_skip_deleted_and_respect_limits() {
    let (_, mut info) = layout(&[(0, 3)]);
    info.options = 0;
    let bytes = [1, 2, 3, 0, 9, 9, 1, 4, 5];
    let stats = walk_records(bytes.as_slice(), 9, &info, None, |_| Ok(())).unwrap();
    assert_eq!(
        (stats.records, stats.deleted_blocks, stats.bytes_consumed),
        (2, 1, 9)
    );
    let stats = walk_records(bytes.as_slice(), 9, &info, Some(1), |_| Ok(())).unwrap();
    assert_eq!(stats.bytes_consumed, 3);
    assert!(walk_records(bytes.as_slice(), 8, &info, None, |_| Ok(())).is_err());
    info.options = 4;
    assert!(walk_records(bytes.as_slice(), 9, &info, None, |_| Ok(())).is_err());
}

#[test]
fn spilled_pending_and_earlier_fragments_round_trip_and_detect_collisions() {
    let dir = TempDir::new("pending-spill");
    let mut fragments = FragmentStore::new(&dir.0);
    fragments
        .spill
        .put_pending(
            20,
            PendingRow {
                expected: 4,
                bytes: b"ab".to_vec(),
            },
        )
        .unwrap();
    assert!(fragments.spill.has_pending(None).unwrap());
    let pending = fragments.spill.take_pending(20).unwrap().unwrap();
    assert_eq!(pending.bytes, b"ab");
    assert!(!fragments.spill.has_pending(None).unwrap());
    fragments
        .spill
        .put_earlier(
            0,
            Continuation {
                bytes: b"cd".to_vec(),
                next: None,
                physical_next: 5,
            },
        )
        .unwrap();
    let row = fragments.resolve(0, 30, pending).unwrap().unwrap();
    assert_eq!(row.bytes, b"abcd");
    fragments
        .spill
        .put_pending(
            50,
            PendingRow {
                expected: 4,
                bytes: b"ab".to_vec(),
            },
        )
        .unwrap();
    assert!(
        fragments
            .resolve(
                50,
                30,
                PendingRow {
                    expected: 4,
                    bytes: b"xy".to_vec()
                }
            )
            .is_err()
    );
    assert!(fs::read_dir(&dir.0).unwrap().count() > 0);
    drop(fragments);
    assert_eq!(fs::read_dir(&dir.0).unwrap().count(), 0);
}

#[test]
fn malformed_schema_metadata_returns_errors_without_panicking() {
    let frm = include_bytes!("../tests/fixtures/libgen-2026-09-06/elem_descr.frm");
    let myi = include_bytes!("../tests/fixtures/libgen-2026-09-06/elem_descr.MYI.header");
    let mut seed = 7u64;
    for i in 0..4000 {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        let mut bytes = if i % 2 == 0 {
            frm.to_vec()
        } else {
            myi.to_vec()
        };
        let at = seed as usize % bytes.len();
        if i % 3 == 0 {
            bytes.truncate(at);
        } else {
            bytes[at] ^= (seed >> 24) as u8;
        }
        if i % 2 == 0 {
            let _ = FrmSchema::parse(&bytes);
        } else {
            let _ = MyisamInfo::parse_myi(&bytes);
        }
    }
}
