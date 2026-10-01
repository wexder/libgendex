//! Temporary on-disk staging used to join the dump tables (editions, files and their links)
//! without holding millions of records in memory. The database is deleted after each run.

use std::{
    collections::{HashMap, HashSet},
    io::BufRead,
    path::Path,
};

use anyhow::Result;
use rusqlite::{Connection, params};
use tracing::info;

use super::sqldump;
use crate::search::Book;
use myisam_reader::FrmSchema;

const TABLES: [&str; 5] = [
    "editions",
    "editions_add_descr",
    "editions_to_files",
    "files",
    "elem_descr",
];

pub struct Stage {
    db: Connection,
    /// Highest `libgen_id` (`l`) and `fiction_id` (`f`) seen, where API new-file syncing continues.
    pub max_ids: HashMap<String, u64>,
    // Declared after `db`, so SQLite closes before the file is removed on success or error.
    _cleanup: StageCleanup,
}

struct StageCleanup(std::path::PathBuf);
impl Drop for StageCleanup {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

pub struct StageOptions<'a> {
    pub extensions: &'a HashSet<String>,
    /// LibGen topics to keep (`libgen_topic` of editions and files); empty keeps everything.
    pub topics: &'a HashSet<String>,
    pub language_key: Option<i64>,
    pub isbn_key: Option<i64>,
}

impl Stage {
    pub fn create(path: &Path) -> Result<Self> {
        let _ = std::fs::remove_file(path);
        let cleanup = StageCleanup(path.to_path_buf());
        let db = Connection::open(path)?;
        db.execute_batch(
            "PRAGMA journal_mode=OFF; PRAGMA synchronous=OFF; PRAGMA cache_size=-16000; PRAGMA temp_store=FILE;
             CREATE TABLE editions(e_id INTEGER PRIMARY KEY, title TEXT, author TEXT, series TEXT, publisher TEXT, year TEXT);
             CREATE TABLE descr(e_id INTEGER, key INTEGER, value TEXT);
             CREATE TABLE etf(f_id INTEGER, e_id INTEGER);
             CREATE TABLE files(f_id INTEGER PRIMARY KEY, md5 TEXT, ext TEXT, size INTEGER, pages INTEGER);
             CREATE TABLE keys(key INTEGER PRIMARY KEY, name TEXT);",
        )?;
        Ok(Self {
            db,
            max_ids: HashMap::new(),
            _cleanup: cleanup,
        })
    }

    pub fn load(&mut self, reader: impl BufRead, opts: &StageOptions) -> Result<()> {
        let tx = self.db.transaction()?;
        {
            let mut ed = tx.prepare("INSERT OR REPLACE INTO editions VALUES (?,?,?,?,?,?)")?;
            let mut de = tx.prepare("INSERT INTO descr VALUES (?,?,?)")?;
            let mut etf = tx.prepare("INSERT INTO etf VALUES (?,?)")?;
            let mut fi = tx.prepare("INSERT OR REPLACE INTO files VALUES (?,?,?,?,?)")?;
            let mut ks = tx.prepare("INSERT OR REPLACE INTO keys VALUES (?,?)")?;
            let mut counts = [0u64; 5];
            // Edition ids kept so far (edition ids reach ~2e8, so a bitset is ~26 MB); descriptors and
            // file links of other collections are dropped before they reach the staging DB.
            let mut kept: Vec<u64> = Vec::new();
            let is_kept = |kept: &Vec<u64>, id: u64| {
                kept.get((id / 64) as usize)
                    .is_some_and(|w| w & (1 << (id % 64)) != 0)
            };
            let wanted_topic = |topic: &str| opts.topics.is_empty() || opts.topics.contains(topic);
            // Only the language and ISBN descriptors are used (keys 101 and 505 in libgen.li).
            let descr_keys = [
                opts.language_key.unwrap_or(101),
                opts.isbn_key.unwrap_or(505),
            ];

            sqldump::parse(
                reader,
                |t| TABLES.contains(&t),
                |table, c, row| {
                    match table {
                        "editions" => {
                            if !row.string(c.idx("visible")?).is_empty()
                                || !wanted_topic(&row.string(c.idx("libgen_topic")?))
                            {
                                return Ok(());
                            }
                            let title = row.string(c.idx("title")?);
                            if title.is_empty() {
                                return Ok(());
                            }
                            ed.execute(params![
                                row.i64(c.idx("e_id")?),
                                title,
                                row.string(c.idx("author")?),
                                row.string(c.idx("series_name")?),
                                row.string(c.idx("publisher")?),
                                row.string(c.idx("year")?),
                            ])?;
                            let id = row.i64(c.idx("e_id")?).unwrap_or(0).max(0) as u64;
                            let word = (id / 64) as usize;
                            if word >= kept.len() {
                                kept.resize(word + 1, 0);
                            }
                            kept[word] |= 1 << (id % 64);
                            counts[0] += 1;
                        }
                        "editions_add_descr" => {
                            let e_id = row.i64(c.idx("e_id")?).unwrap_or(0).max(0) as u64;
                            let key = row.i64(c.idx("key")?).unwrap_or(0);
                            if !descr_keys.contains(&key) || !is_kept(&kept, e_id) {
                                return Ok(());
                            }
                            let value = row.string(c.idx("value")?);
                            if value.is_empty() || value.len() > 64 {
                                return Ok(());
                            }
                            de.execute(params![
                                row.i64(c.idx("e_id")?),
                                row.i64(c.idx("key")?),
                                value
                            ])?;
                            counts[1] += 1;
                        }
                        "editions_to_files" => {
                            let e_id = row.i64(c.idx("e_id")?).unwrap_or(0).max(0) as u64;
                            if !is_kept(&kept, e_id) {
                                return Ok(());
                            }
                            etf.execute(params![row.i64(c.idx("f_id")?), row.i64(c.idx("e_id")?)])?;
                            counts[2] += 1;
                        }
                        "files" => {
                            for (topic, column) in [("l", "libgen_id"), ("f", "fiction_id")] {
                                let id = row.i64(c.idx(column)?).unwrap_or(0).max(0) as u64;
                                let max = self.max_ids.entry(topic.to_string()).or_default();
                                *max = (*max).max(id);
                            }
                            if !wanted_topic(&row.string(c.idx("libgen_topic")?)) {
                                return Ok(());
                            }
                            let ext = row.string(c.idx("extension")?).to_lowercase();
                            if !opts.extensions.contains(&ext)
                                || !row.string(c.idx("visible")?).is_empty()
                                || row.str(c.idx("broken")?) == Some("Y")
                            {
                                return Ok(());
                            }
                            let md5 = row.string(c.idx("md5")?).to_lowercase();
                            if md5.len() != 32 {
                                return Ok(());
                            }
                            fi.execute(params![
                                row.i64(c.idx("f_id")?),
                                md5,
                                ext,
                                row.i64(c.idx("filesize")?).unwrap_or(0),
                                row.i64(c.idx("pages")?).unwrap_or(0),
                            ])?;
                            counts[3] += 1;
                        }
                        "elem_descr" => {
                            ks.execute(params![
                                row.i64(c.idx("key")?),
                                row.string(c.idx("name_en")?)
                            ])?;
                            counts[4] += 1;
                        }
                        _ => {}
                    }
                    let total: u64 = counts.iter().sum();
                    if total.is_multiple_of(1_000_000) {
                        info!(
                            editions = counts[0],
                            descr = counts[1],
                            links = counts[2],
                            files = counts[3],
                            "staging"
                        );
                    }
                    Ok(())
                },
            )?;
            info!(
                editions = counts[0],
                descr = counts[1],
                links = counts[2],
                files = counts[3],
                keys = counts[4],
                "staging complete"
            );
        }
        tx.commit()?;
        self.db.execute_batch(
            "CREATE INDEX etf_f ON etf(f_id); CREATE INDEX descr_e ON descr(e_id, key);",
        )?;
        Ok(())
    }

    /// Loads decoded MyISAM records through the same bounded SQLite staging database used for
    /// SQL dumps. `pump` visits the five relevant members in table order and calls `on_row` once
    /// per decoded record; member bytes and records never go to disk.
    pub fn load_myisam(
        &mut self,
        opts: &StageOptions,
        mut pump: impl FnMut(
            &mut dyn FnMut(&str, &FrmSchema, &[Option<Vec<u8>>]) -> Result<()>,
        ) -> Result<()>,
    ) -> Result<()> {
        let tx = self.db.transaction()?;
        {
            let mut ed = tx.prepare("INSERT OR REPLACE INTO editions VALUES (?,?,?,?,?,?)")?;
            let mut de = tx.prepare("INSERT INTO descr VALUES (?,?,?)")?;
            let mut etf = tx.prepare("INSERT INTO etf VALUES (?,?)")?;
            let mut fi = tx.prepare("INSERT OR REPLACE INTO files VALUES (?,?,?,?,?)")?;
            let mut ks = tx.prepare("INSERT OR REPLACE INTO keys VALUES (?,?)")?;
            let mut counts = [0u64; 5];
            let mut kept: Vec<u64> = Vec::new();
            let is_kept = |kept: &Vec<u64>, id: u64| {
                kept.get((id / 64) as usize)
                    .is_some_and(|w| w & (1 << (id % 64)) != 0)
            };
            let wanted_topic = |topic: &str| opts.topics.is_empty() || opts.topics.contains(topic);
            let descr_keys = [
                opts.language_key.unwrap_or(101),
                opts.isbn_key.unwrap_or(505),
            ];
            pump(&mut |table, schema, row| {
                let text = |name: &str| -> Result<String> {
                    Ok(myisam_reader::value(schema, row, name)?.unwrap_or_default())
                };
                let num = |name: &str| -> Result<Option<i64>> {
                    Ok(myisam_reader::number(schema, row, name)?.map(|n| n as i64))
                };
                match table {
                    "editions" => {
                        if !text("visible")?.is_empty() || !wanted_topic(&text("libgen_topic")?) {
                            return Ok(());
                        }
                        let title = text("title")?;
                        if title.is_empty() {
                            return Ok(());
                        }
                        ed.execute(params![
                            num("e_id")?,
                            title,
                            text("author")?,
                            text("series_name")?,
                            text("publisher")?,
                            text("year")?,
                        ])?;
                        let id = num("e_id")?.unwrap_or(0).max(0) as u64;
                        let word = (id / 64) as usize;
                        if word >= kept.len() {
                            kept.resize(word + 1, 0);
                        }
                        kept[word] |= 1 << (id % 64);
                        counts[0] += 1;
                    }
                    "editions_add_descr" => {
                        let e_id = num("e_id")?.unwrap_or(0).max(0) as u64;
                        let key = num("key")?.unwrap_or(0);
                        if !descr_keys.contains(&key) || !is_kept(&kept, e_id) {
                            return Ok(());
                        }
                        let value = text("value")?;
                        if value.is_empty() || value.len() > 64 {
                            return Ok(());
                        }
                        de.execute(params![num("e_id")?, num("key")?, value])?;
                        counts[1] += 1;
                    }
                    "editions_to_files" => {
                        let e_id = num("e_id")?.unwrap_or(0).max(0) as u64;
                        if !is_kept(&kept, e_id) {
                            return Ok(());
                        }
                        etf.execute(params![num("f_id")?, num("e_id")?])?;
                        counts[2] += 1;
                    }
                    "files" => {
                        for (topic, column) in [("l", "libgen_id"), ("f", "fiction_id")] {
                            let id = num(column)?.unwrap_or(0).max(0) as u64;
                            let max = self.max_ids.entry(topic.to_string()).or_default();
                            *max = (*max).max(id);
                        }
                        if !wanted_topic(&text("libgen_topic")?) {
                            return Ok(());
                        }
                        let ext = text("extension")?.to_lowercase();
                        if !opts.extensions.contains(&ext)
                            || !text("visible")?.is_empty()
                            || text("broken")? == "Y"
                        {
                            return Ok(());
                        }
                        let md5 = text("md5")?.to_lowercase();
                        if md5.len() != 32 {
                            return Ok(());
                        }
                        fi.execute(params![
                            num("f_id")?,
                            md5,
                            ext,
                            num("filesize")?.unwrap_or(0),
                            num("pages")?.unwrap_or(0),
                        ])?;
                        counts[3] += 1;
                    }
                    "elem_descr" => {
                        ks.execute(params![num("key")?, text("name_en")?])?;
                        counts[4] += 1;
                    }
                    _ => return Err(anyhow::anyhow!("unexpected MyISAM table `{table}`")),
                }
                let total: u64 = counts.iter().sum();
                if total > 0 && total.is_multiple_of(1_000_000) {
                    info!(
                        table,
                        editions = counts[0],
                        descr = counts[1],
                        links = counts[2],
                        files = counts[3],
                        "MyISAM staging"
                    );
                }
                Ok(())
            })?;
            info!(
                editions = counts[0],
                descr = counts[1],
                links = counts[2],
                files = counts[3],
                keys = counts[4],
                "MyISAM staging complete"
            );
        }
        tx.commit()?;
        self.db.execute_batch(
            "CREATE INDEX etf_f ON etf(f_id); CREATE INDEX descr_e ON descr(e_id, key);",
        )?;
        Ok(())
    }

    fn resolve_key(&self, configured: Option<i64>, pattern: &str) -> Result<Option<i64>> {
        if configured.is_some() {
            return Ok(configured);
        }
        let key = self
            .db
            .query_row(
                "SELECT key FROM keys WHERE lower(name) LIKE ?1 ORDER BY length(name), key LIMIT 1",
                [pattern],
                |r| r.get(0),
            )
            .ok();
        Ok(key)
    }

    /// Streams joined books; a file linked to several editions takes the lowest edition id.
    pub fn for_each_book(
        &self,
        source: &str,
        opts: &StageOptions,
        mut f: impl FnMut(Book) -> Result<()>,
    ) -> Result<()> {
        let lang_key = self.resolve_key(opts.language_key, "language%")?;
        let isbn_key = self.resolve_key(opts.isbn_key, "isbn%")?;
        info!(?lang_key, ?isbn_key, "resolved descriptor keys");

        let mut stmt = self.db.prepare(
            "SELECT f.md5, f.ext, f.size, f.pages, e.title, e.author, e.series, e.publisher, e.year,
                    (SELECT value FROM descr d WHERE d.e_id = e.e_id AND d.key = ?1 LIMIT 1),
                    (SELECT group_concat(value, '|') FROM descr d WHERE d.e_id = e.e_id AND d.key = ?2)
             FROM files f
             JOIN editions e ON e.e_id = (SELECT min(e_id) FROM etf WHERE etf.f_id = f.f_id)",
        )?;
        let mut rows = stmt.query(params![lang_key.unwrap_or(-1), isbn_key.unwrap_or(-1)])?;
        while let Some(r) = rows.next()? {
            let isbn: Option<String> = r.get(10)?;
            f(Book {
                md5: r.get(0)?,
                extension: r.get(1)?,
                filesize: r.get::<_, i64>(2)?.max(0) as u64,
                pages: r.get::<_, i64>(3)?.max(0) as u64,
                title: r.get(4)?,
                author: r.get(5)?,
                series: r.get(6)?,
                publisher: r.get(7)?,
                year: r.get(8)?,
                language: r.get::<_, Option<String>>(9)?.unwrap_or_default(),
                isbn: isbn
                    .map(|s| {
                        s.split('|')
                            .map(|i| i.replace(['-', ' '], ""))
                            .filter(|i| !i.is_empty())
                            .collect()
                    })
                    .unwrap_or_default(),
                source: source.to_string(),
            })?;
        }
        Ok(())
    }
}
