use crate::bytes::{be_u16, be_u32, be_u64, invalid};
use std::io;

#[derive(Debug, Clone)]
pub struct MyisamInfo {
    pub options: u16,
    pub header_len: usize,
    pub base_pos: usize,
    pub record_count: u64,
    pub fields: u32,
    pub packed_fields: u32,
    pub record_len: u32,
    pub packed_record_len: u32,
    pub min_packed_len: u32,
    pub pack_bits: u16,
    pub recdefs: Vec<MyisamColumnDef>,
}

#[derive(Debug, Clone)]
pub struct MyisamColumnDef {
    pub kind: i16,
    pub length: usize,
    pub null_bit: u8,
    pub null_pos: usize,
}

impl MyisamInfo {
    pub fn parse_myi(bytes: &[u8]) -> io::Result<Self> {
        if bytes.len() < 32 || bytes[..4] != [0xfe, 0xfe, 0x07, 0x01] {
            return Err(invalid(
                "not a supported MyISAM index header (expected MySQL 5.x magic)",
            ));
        }
        let options = be_u16(bytes, 4)?;
        let header_len = be_u16(bytes, 6)? as usize;
        let base_pos = be_u16(bytes, 12)? as usize;
        if header_len > bytes.len() || base_pos + 100 > header_len || base_pos < 36 {
            return Err(invalid("truncated MyISAM index header"));
        }

        // MyISAM 5.x stores the current row count after the fixed state prefix:
        // 24-byte file header, then open_count/changed/sortkey, then ha_rows.
        let record_count = be_u64(bytes, 28)?;
        let fields = be_u32(bytes, base_pos + 64)?;
        let packed_fields = be_u32(bytes, base_pos + 68)?;
        let record_len = be_u32(bytes, base_pos + 44)?;
        let packed_record_len = be_u32(bytes, base_pos + 48)?;
        let min_packed_len = be_u32(bytes, base_pos + 52)?;
        let pack_bits = be_u16(bytes, base_pos + 76)?;

        if fields == 0 || fields > 4096 || record_len == 0 || record_len > 1_000_000 {
            return Err(invalid("implausible MyISAM base-info values"));
        }
        let base_info_len = be_u16(bytes, 10)? as usize;
        if base_info_len < 100 {
            return Err(invalid("unsupported MyISAM base-info size"));
        }
        let mut rec_at = base_pos
            .checked_add(base_info_len)
            .ok_or_else(|| invalid("MyISAM base-info offset overflow"))?;
        let keys = bytes[18] as usize;
        let uniques = bytes[19] as usize;
        for _ in 0..keys {
            let def = bytes
                .get(rec_at..rec_at + 12)
                .ok_or_else(|| invalid("truncated MyISAM key definition"))?;
            let segs = def[0] as usize;
            rec_at = rec_at
                .checked_add(12 + segs * 18)
                .ok_or_else(|| invalid("MyISAM key segment overflow"))?;
            if rec_at > header_len {
                return Err(invalid("MyISAM key definitions exceed header length"));
            }
        }
        for _ in 0..uniques {
            let def = bytes
                .get(rec_at..rec_at + 4)
                .ok_or_else(|| invalid("truncated MyISAM unique definition"))?;
            let segs = def[0] as usize;
            rec_at = rec_at
                .checked_add(4 + segs * 18)
                .ok_or_else(|| invalid("MyISAM unique segment overflow"))?;
            if rec_at > header_len {
                return Err(invalid("MyISAM unique definitions exceed header length"));
            }
        }
        let rec_end = rec_at
            .checked_add(fields as usize * 7)
            .ok_or_else(|| invalid("MyISAM column definitions overflow"))?;
        if rec_end > header_len {
            return Err(invalid("MyISAM column definitions exceed header length"));
        }
        let raw_defs = bytes
            .get(rec_at..rec_end)
            .ok_or_else(|| invalid("truncated MyISAM column definitions"))?;
        let recdefs: Vec<MyisamColumnDef> = raw_defs
            .as_chunks::<7>()
            .0
            .iter()
            .map(|d| MyisamColumnDef {
                kind: i16::from_be_bytes([d[0], d[1]]),
                length: u16::from_be_bytes([d[2], d[3]]) as usize,
                null_bit: d[4],
                null_pos: u16::from_be_bytes([d[5], d[6]]) as usize,
            })
            .collect();
        let def_record_len = recdefs.iter().map(|d| d.length as u64).sum::<u64>();
        if def_record_len != record_len as u64 {
            return Err(invalid(format!(
                "MyISAM record definitions total {def_record_len} bytes, header says {record_len}"
            )));
        }
        Ok(Self {
            options,
            header_len,
            base_pos,
            record_count,
            fields,
            packed_fields,
            record_len,
            packed_record_len,
            min_packed_len,
            pack_bits,
            recdefs,
        })
    }
}

pub fn read_myi_header(mut reader: impl io::Read) -> io::Result<Vec<u8>> {
    let mut bytes = vec![0u8; 8];
    reader.read_exact(&mut bytes)?;
    if bytes[..4] != [0xfe, 0xfe, 0x07, 0x01] {
        return Err(invalid("unsupported MyISAM index header"));
    }
    let length = u16::from_be_bytes([bytes[6], bytes[7]]) as usize;
    if length < 128 {
        return Err(invalid("implausible MyISAM header length"));
    }
    bytes.resize(length, 0);
    reader.read_exact(&mut bytes[8..])?;
    Ok(bytes)
}
