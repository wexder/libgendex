//! Streaming parser for mysqldump output. Only the tables the caller asks for are tokenized;
//! everything else is skipped line by line, so memory stays bounded by the longest INSERT line.

use std::{collections::HashMap, io::BufRead};

use anyhow::{Context, Result, bail};

pub struct Row<'a> {
    buf: &'a [u8],
    fields: &'a [Option<(usize, usize)>],
}

impl<'a> Row<'a> {
    pub fn str(&self, idx: usize) -> Option<&'a str> {
        let (s, e) = (*self.fields.get(idx)?)?;
        std::str::from_utf8(&self.buf[s..e]).ok()
    }

    pub fn string(&self, idx: usize) -> String {
        match self.fields.get(idx).copied().flatten() {
            Some((s, e)) => String::from_utf8_lossy(&self.buf[s..e]).trim().to_string(),
            None => String::new(),
        }
    }

    pub fn i64(&self, idx: usize) -> Option<i64> {
        self.str(idx)?.trim().parse().ok()
    }
}

/// Column name -> index lookup for a table, captured from its CREATE TABLE statement.
#[derive(Debug, Default, Clone)]
pub struct Columns(HashMap<String, usize>);

impl Columns {
    pub fn idx(&self, name: &str) -> Result<usize> {
        self.0
            .get(name)
            .copied()
            .with_context(|| format!("column `{name}` missing from dump"))
    }
}

pub fn parse<R, W, F>(mut reader: R, wanted: W, mut on_row: F) -> Result<()>
where
    R: BufRead,
    W: Fn(&str) -> bool,
    F: FnMut(&str, &Columns, &Row) -> Result<()>,
{
    let mut line = Vec::with_capacity(1 << 20);
    let mut tables: HashMap<String, Columns> = HashMap::new();
    let mut creating: Option<(String, Vec<String>)> = None;
    let mut scratch = Vec::with_capacity(1 << 16);
    let mut fields = Vec::with_capacity(128);

    loop {
        line.clear();
        if reader.read_until(b'\n', &mut line)? == 0 {
            break;
        }

        if let Some((_, cols)) = creating.as_mut() {
            if line.starts_with(b"  `") {
                if let Some(name) = backticked(&line[2..]) {
                    cols.push(name.to_string());
                }
                continue;
            }
            if line.starts_with(b")") {
                let (name, cols) = creating.take().unwrap();
                tables.insert(
                    name,
                    Columns(cols.into_iter().enumerate().map(|(i, c)| (c, i)).collect()),
                );
            }
            continue;
        }

        if let Some(rest) = line.strip_prefix(b"CREATE TABLE ") {
            if let Some(name) = backticked(rest)
                && wanted(name)
            {
                creating = Some((name.to_string(), Vec::new()));
            }
            continue;
        }

        let Some(rest) = line.strip_prefix(b"INSERT INTO ") else {
            continue;
        };
        let Some(name) = backticked(rest) else {
            continue;
        };
        if !wanted(name) {
            continue;
        }
        let name = name.to_string();
        let cols = tables
            .get(&name)
            .with_context(|| format!("INSERT for `{name}` before its CREATE TABLE"))?
            .clone();
        let Some(values_at) = find(rest, b" VALUES ") else {
            continue;
        };
        let mut pos = values_at + 8 + (line.len() - rest.len());
        let data = &line[..];

        while pos < data.len() {
            match data[pos] {
                b'(' => {
                    pos = parse_tuple(data, pos + 1, &mut scratch, &mut fields)?;
                    on_row(
                        &name,
                        &cols,
                        &Row {
                            buf: &scratch,
                            fields: &fields,
                        },
                    )?;
                }
                b';' | b'\n' | b'\r' => break,
                _ => pos += 1,
            }
        }
    }
    Ok(())
}

fn backticked(s: &[u8]) -> Option<&str> {
    let s = s.strip_prefix(b"`")?;
    let end = s.iter().position(|&b| b == b'`')?;
    std::str::from_utf8(&s[..end]).ok()
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

/// Parses one `(...)` tuple starting just after the opening paren. Unescaped values are appended
/// to `scratch`, and `fields` receives their byte ranges (None for NULL).
fn parse_tuple(
    data: &[u8],
    mut pos: usize,
    scratch: &mut Vec<u8>,
    fields: &mut Vec<Option<(usize, usize)>>,
) -> Result<usize> {
    scratch.clear();
    fields.clear();
    loop {
        let Some(&c) = data.get(pos) else {
            bail!("unterminated tuple")
        };
        match c {
            b'\'' => {
                pos += 1;
                let start = scratch.len();
                loop {
                    let Some(&c) = data.get(pos) else {
                        bail!("unterminated string")
                    };
                    match c {
                        b'\\' => {
                            let esc = *data.get(pos + 1).context("dangling escape")?;
                            scratch.push(match esc {
                                b'0' => 0,
                                b'n' => b'\n',
                                b'r' => b'\r',
                                b't' => b'\t',
                                b'b' => 8,
                                b'Z' => 26,
                                other => other,
                            });
                            pos += 2;
                        }
                        b'\'' if data.get(pos + 1) == Some(&b'\'') => {
                            scratch.push(b'\'');
                            pos += 2;
                        }
                        b'\'' => {
                            pos += 1;
                            break;
                        }
                        _ => {
                            scratch.push(c);
                            pos += 1;
                        }
                    }
                }
                fields.push(Some((start, scratch.len())));
            }
            b',' => pos += 1,
            b')' => return Ok(pos + 1),
            _ => {
                let end = data[pos..]
                    .iter()
                    .position(|&b| b == b',' || b == b')')
                    .map(|p| pos + p)
                    .context("unterminated literal")?;
                let lit = &data[pos..end];
                if lit.eq_ignore_ascii_case(b"NULL") {
                    fields.push(None);
                } else {
                    let start = scratch.len();
                    scratch.extend_from_slice(lit);
                    fields.push(Some((start, scratch.len())));
                }
                pos = end;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_inserts() {
        let dump = "CREATE TABLE `t` (\n  `id` int,\n  `name` varchar(10),\n  `x` int,\n  PRIMARY KEY (`id`)\n) ENGINE=MyISAM;\n\
                    CREATE TABLE `skip` (\n  `a` int\n);\n\
                    INSERT INTO `skip` VALUES (1);\n\
                    INSERT INTO `t` VALUES (1,'it\\'s, ok',NULL),(2,'a''b\\\\',-3.5);\n";
        let mut rows = Vec::new();
        parse(
            dump.as_bytes(),
            |t| t == "t",
            |_, cols, row| {
                rows.push((
                    row.i64(cols.idx("id")?),
                    row.string(cols.idx("name")?),
                    row.str(cols.idx("x")?).map(str::to_string),
                ));
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(
            rows,
            vec![
                (Some(1), "it's, ok".to_string(), None),
                (Some(2), "a'b\\".to_string(), Some("-3.5".to_string())),
            ]
        );
    }
}
