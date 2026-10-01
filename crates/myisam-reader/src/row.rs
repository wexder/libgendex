use crate::{
    FrmSchema, MyisamInfo,
    bytes::{invalid, read_le_len, read_packed_string_len, read_varchar_len},
};
use std::io;

/// Binds physical MyISAM fields to SQL columns once. Reserved NULL bitmap fields have no SQL
/// column, and columns need not be stored in their declaration order.
pub struct RowDecoder<'a> {
    schema: &'a FrmSchema,
    info: &'a MyisamInfo,
    mapping: Vec<Option<usize>>,
    selected: Vec<bool>,
    value_limit: usize,
    null_bytes: usize,
}

impl<'a> RowDecoder<'a> {
    pub(crate) fn new(schema: &'a FrmSchema, info: &'a MyisamInfo) -> io::Result<Self> {
        let mut mapping = Vec::with_capacity(info.recdefs.len());
        let mut offset = 0usize;
        let mut found = vec![false; schema.columns.len()];
        for def in &info.recdefs {
            let col = schema.columns.iter().position(|c| c.offset == offset);
            if let Some(idx) = col {
                found[idx] = true;
            } else if def.kind != 0 {
                return Err(invalid(format!(
                    "unmapped packed MyISAM field at offset {offset}"
                )));
            }
            mapping.push(col);
            offset += def.length;
        }
        if found.iter().any(|v| !v) {
            let missing = schema
                .columns
                .iter()
                .zip(found)
                .filter(|(_, f)| !f)
                .map(|(c, _)| c.name.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            return Err(invalid(format!(
                "MyISAM record layout does not cover columns: {missing}"
            )));
        }
        let null_bytes = info
            .recdefs
            .iter()
            .filter(|d| d.null_bit != 0)
            .map(|d| d.null_pos + 1)
            .max()
            .unwrap_or(0);
        Ok(Self {
            schema,
            info,
            mapping,
            selected: vec![true; schema.columns.len()],
            value_limit: usize::MAX,
            null_bytes,
        })
    }

    /// Validate every field, but copy only the named columns into the decoded row.
    /// Unselected columns have a `None` slot, just like SQL NULL values.
    pub fn project(mut self, names: &[&str]) -> io::Result<Self> {
        self.selected.fill(false);
        for name in names {
            self.selected[self.schema.column_index(name)?] = true;
        }
        Ok(self)
    }

    /// Skip copying values larger than this limit. Their fields are still consumed and validated.
    /// Skipped values have a `None` slot. No limit is applied by default.
    pub fn with_value_limit(mut self, bytes: usize) -> Self {
        self.value_limit = bytes;
        self
    }

    pub fn unpack(&self, packed: &[u8]) -> io::Result<Vec<Option<Vec<u8>>>> {
        let pack_bytes = self.info.pack_bits as usize;
        if packed.len() < pack_bytes {
            return Err(invalid("truncated MyISAM packing bitmap"));
        }
        let mut pos = pack_bytes;
        let mut pack_bit = 0usize;
        let mut record_offset = 0usize;
        let mut null_map = vec![0u8; self.null_bytes];
        let mut values = vec![None; self.schema.columns.len()];
        for (def, col) in self.info.recdefs.iter().zip(&self.mapping) {
            let kind = def.kind;
            let width = def.length;
            let packed_flag = if matches!(kind, 1..=4) {
                let flag = *packed
                    .get(pack_bit / 8)
                    .filter(|_| pack_bit / 8 < pack_bytes)
                    .ok_or_else(|| invalid("MyISAM field exceeds packing bitmap"))?;
                let set = flag & (1 << (pack_bit % 8)) != 0;
                pack_bit += 1;
                set
            } else {
                false
            };
            let mut padding = 0usize;
            let data = match kind {
                0 | 9 => take(packed, &mut pos, width)?,
                3 if packed_flag => {
                    padding = width;
                    &[]
                }
                3 => take(packed, &mut pos, width)?,
                4 if packed_flag => &[],
                4 => {
                    // MySQL uses an eight-byte portable pointer slot even on 32-bit hosts.
                    let prefix = width
                        .checked_sub(8)
                        .filter(|n| (1..=4).contains(n))
                        .ok_or_else(|| invalid("invalid MyISAM BLOB record width"))?;
                    let len = read_le_len(packed, &mut pos, prefix)?;
                    take(packed, &mut pos, len)?
                }
                1 | 2 => {
                    let len = if packed_flag {
                        read_packed_string_len(packed, &mut pos, width)?
                    } else {
                        width
                    };
                    if len > width {
                        return Err(invalid("packed string exceeds MyISAM field width"));
                    }
                    padding = width - len;
                    take(packed, &mut pos, len)?
                }
                8 => {
                    let prefix_width = if width <= 256 { 1 } else { 2 };
                    let len = if prefix_width == 1 {
                        take(packed, &mut pos, 1)?[0] as usize
                    } else {
                        read_varchar_len(packed, &mut pos)?
                    };
                    if len > width - prefix_width {
                        return Err(invalid("VARCHAR exceeds MyISAM field width"));
                    }
                    take(packed, &mut pos, len)?
                }
                _ => return Err(invalid(format!("unsupported MyISAM packing kind {kind}"))),
            };
            // NULL flags occupy reserved normal fields in the record and are decoded before the
            // columns they describe. Their .MYI bit/position metadata is authoritative.
            if col.is_none() && record_offset < null_map.len() {
                let n = data.len().min(null_map.len() - record_offset);
                null_map[record_offset..record_offset + n].copy_from_slice(&data[..n]);
            }
            if let Some(idx) = col {
                let is_null = def.null_bit != 0
                    && null_map
                        .get(def.null_pos)
                        .is_some_and(|b| b & def.null_bit != 0);
                if self.selected[*idx]
                    && !is_null
                    && data.len().saturating_add(padding) <= self.value_limit
                {
                    let mut value = Vec::with_capacity(data.len() + padding);
                    if kind == 2 {
                        value.resize(padding, b' ');
                    }
                    value.extend_from_slice(data);
                    if kind == 1 {
                        value.resize(width, b' ');
                    }
                    if kind == 3 && packed_flag {
                        value.resize(width, 0);
                    }
                    values[*idx] = Some(value);
                }
            }
            record_offset += width;
        }
        let checksum_bytes = usize::from(self.info.options & 32 != 0);
        if pos + checksum_bytes != packed.len() {
            return Err(invalid(format!(
                "MyISAM row has {} bytes but decoder consumed {pos} (+{checksum_bytes} checksum)",
                packed.len()
            )));
        }
        Ok(values)
    }
}

fn take<'a>(bytes: &'a [u8], pos: &mut usize, len: usize) -> io::Result<&'a [u8]> {
    let end = pos
        .checked_add(len)
        .ok_or_else(|| invalid("MyISAM field length overflow"))?;
    let data = bytes
        .get(*pos..end)
        .ok_or_else(|| invalid(format!("truncated MyISAM field at {pos}, length {len}")))?;
    *pos = end;
    Ok(data)
}

/// Formats a decoded MyISAM value as the same textual scalar the SQL dump parser exposes.
pub fn value(
    schema: &FrmSchema,
    row: &[Option<Vec<u8>>],
    name: &str,
) -> io::Result<Option<String>> {
    let idx = schema.column_index(name)?;
    let Some(bytes) = row
        .get(idx)
        .ok_or_else(|| invalid("decoded MyISAM row is shorter than schema"))?
    else {
        return Ok(None);
    };
    let col = &schema.columns[idx];
    if matches!(col.type_code, 1..=3 | 8 | 9 | 244) {
        return Ok(number(schema, row, name)?.map(|n| n.to_string()));
    }
    if matches!(col.type_code, 247 | 248) {
        let ordinal = number(schema, row, name)?.unwrap_or(0);
        let labels = &col.enum_values;
        if col.type_code == 248 {
            if labels.len() < 64 && ordinal >> labels.len() != 0 {
                return Err(invalid(format!("SET bits out of range for `{name}`")));
            }
            return Ok(Some(
                labels
                    .iter()
                    .enumerate()
                    .take(64)
                    .filter(|(i, _)| ordinal & (1u64 << i) != 0)
                    .map(|(_, label)| label.as_str())
                    .collect::<Vec<_>>()
                    .join(","),
            ));
        }
        return Ok(Some(if ordinal == 0 {
            String::new()
        } else {
            labels
                .get(ordinal.saturating_sub(1) as usize)
                .ok_or_else(|| {
                    invalid(format!("ENUM ordinal {ordinal} out of range for `{name}`"))
                })?
                .to_string()
        }));
    }
    if !matches!(col.type_code, 15 | 249..=254) {
        return Err(invalid(format!(
            "SQL scalar conversion for type {} in `{name}` is unsupported",
            col.type_code
        )));
    }
    let text = String::from_utf8_lossy(bytes);
    // SQL CHAR reads omit right padding; VARCHAR/TEXT preserve user whitespace.
    Ok(Some(if col.type_code == 254 {
        text.trim_end_matches(' ').to_owned()
    } else {
        text.into_owned()
    }))
}

/// Reads supported unsigned integer, ENUM and SET slots from their little-endian bytes.
pub fn number(schema: &FrmSchema, row: &[Option<Vec<u8>>], name: &str) -> io::Result<Option<u64>> {
    let idx = schema.column_index(name)?;
    let Some(bytes) = row
        .get(idx)
        .ok_or_else(|| invalid("decoded MyISAM row is shorter than schema"))?
    else {
        return Ok(None);
    };
    let col = &schema.columns[idx];
    if !matches!(col.type_code, 1..=3 | 8 | 9 | 244 | 247 | 248) {
        return Err(invalid(format!(
            "column `{name}` is not a supported integer, ENUM or SET"
        )));
    }
    if bytes.len() > 8 {
        return Err(invalid(format!(
            "numeric field `{name}` is {} bytes",
            bytes.len()
        )));
    }
    Ok(Some(
        bytes
            .iter()
            .enumerate()
            .fold(0u64, |v, (i, b)| v | ((*b as u64) << (8 * i))),
    ))
}
