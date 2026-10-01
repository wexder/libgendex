use crate::{
    MyisamInfo, RowDecoder,
    bytes::{invalid, le_u16, le_u32},
};
use std::{collections::HashMap, io};

/// Column metadata recovered from a MySQL 5.7 `.frm` file. This is deliberately limited to the
/// legacy MySQL 5.x FRM layout used by the LibGen dump.
#[derive(Debug, Clone)]
pub struct FrmSchema {
    pub columns: Vec<FrmColumn>,
    pub null_fields: usize,
    pub pack_records: bool,
    pub(crate) positions: HashMap<String, usize>,
}

#[derive(Debug, Clone)]
pub struct FrmColumn {
    pub name: String,
    /// Byte offset of this column in the unpacked MySQL record.
    pub offset: usize,
    pub length: usize,
    pub flags: u16,
    pub type_code: u8,
    pub enum_values: Vec<String>,
}

impl FrmSchema {
    pub fn parse(bytes: &[u8]) -> io::Result<Self> {
        if bytes.len() < 64 || bytes[0..2] != [0xfe, 0x01] {
            return Err(invalid("not a supported MySQL 5.x .frm file"));
        }
        let names_len = le_u16(bytes, 4)? as usize;
        let form_ptr = le_u32(bytes, 64 + names_len)? as usize;
        let form = bytes
            .get(form_ptr..form_ptr + 288)
            .ok_or_else(|| invalid("truncated .frm form info"))?;
        let count = le_u16(form, 258)? as usize;
        let screen_len = le_u16(form, 260)? as usize;
        let names_len = le_u16(form, 268)? as usize;
        let null_fields = le_u16(form, 282)? as usize;
        let meta_start = form_ptr + 288 + screen_len;
        let meta_end = meta_start
            .checked_add(
                count
                    .checked_mul(17)
                    .ok_or_else(|| invalid("too many .frm columns"))?,
            )
            .ok_or_else(|| invalid(".frm metadata overflow"))?;
        let metadata = bytes
            .get(meta_start..meta_end)
            .ok_or_else(|| invalid("truncated .frm column metadata"))?;
        let names = bytes
            .get(meta_end..meta_end + names_len)
            .ok_or_else(|| invalid("truncated .frm column names"))?;
        if names.len() < 3 {
            return Err(invalid("invalid .frm column names section"));
        }
        let name_bytes = &names[1..names.len() - 2];
        let parsed_names = name_bytes
            .split(|b| *b == 0xff)
            .map(|n| String::from_utf8_lossy(n).into_owned())
            .collect::<Vec<_>>();
        if parsed_names.len() != count {
            return Err(invalid(format!(
                ".frm has {count} columns but {} names",
                parsed_names.len()
            )));
        }
        let labels_len = le_u16(form, 274)? as usize;
        let labels = bytes
            .get(meta_end + names_len..meta_end + names_len + labels_len)
            .ok_or_else(|| invalid("truncated .frm ENUM labels"))?;
        let mut label_groups = Vec::new();
        if !labels.is_empty() {
            for group in labels[..labels.len() - 1].split(|b| *b == 0) {
                if group.len() < 2 {
                    return Err(invalid("invalid .frm ENUM label group"));
                }
                label_groups.push(
                    group[1..group.len() - 1]
                        .split(|b| *b == 0xff)
                        .map(|s| String::from_utf8_lossy(s).into_owned())
                        .collect::<Vec<_>>(),
                );
            }
        }
        let columns: Vec<FrmColumn> = metadata
            .as_chunks::<17>()
            .0
            .iter()
            .zip(parsed_names)
            .map(|(m, name)| FrmColumn {
                name,
                offset: (m[5] as usize | ((m[6] as usize) << 8) | ((m[7] as usize) << 16))
                    .saturating_sub(1),
                length: u16::from_le_bytes([m[3], m[4]]) as usize,
                flags: u16::from_le_bytes([m[8], m[9]]),
                type_code: m[13],
                enum_values: if matches!(m[13], 247 | 248) && m[12] != 0 {
                    label_groups
                        .get(m[12] as usize - 1)
                        .cloned()
                        .unwrap_or_default()
                } else {
                    Vec::new()
                },
            })
            .collect();
        let options = le_u16(bytes, 0x1e)?;
        let positions = columns
            .iter()
            .enumerate()
            .map(|(i, c)| (c.name.clone(), i))
            .collect();
        Ok(Self {
            columns,
            null_fields,
            pack_records: options & 1 != 0,
            positions,
        })
    }

    pub fn column_index(&self, name: &str) -> io::Result<usize> {
        self.positions
            .get(name)
            .copied()
            .ok_or_else(|| invalid(format!(".frm has no `{name}` column")))
    }

    pub fn decoder<'a>(&'a self, info: &'a MyisamInfo) -> io::Result<RowDecoder<'a>> {
        RowDecoder::new(self, info)
    }

    #[cfg(test)]
    pub fn unpack(&self, packed: &[u8], info: &MyisamInfo) -> io::Result<Vec<Option<Vec<u8>>>> {
        self.decoder(info)?.unpack(packed)
    }
}
