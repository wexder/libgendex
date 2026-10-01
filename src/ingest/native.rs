//! Native RAR/MyISAM bootstrap: project rows, stage joins, and commit the source index.

use super::{
    DumpStats, ftp_range,
    progress::{BootstrapReporter, ProgressReader},
    stage::{Stage, StageOptions},
};
use crate::{config::Config, search::SearchIndex};
use anyhow::{Context, Result, bail};
use myisam_reader as myisam;
use std::{
    collections::HashSet,
    io::Read,
    sync::{Arc, atomic::Ordering},
};
use tracing::info;

pub(super) fn build_remote(
    cfg: &Config,
    index: &SearchIndex,
    source: &str,
    topics: &[String],
    urls: Vec<String>,
    reporter: Arc<BootstrapReporter>,
) -> Result<DumpStats> {
    let cache = Arc::new(ftp_range::DiskRangeCache::open(
        ftp_range::cache_dir(&cfg.paths.data_dir),
        cfg.indexer.ftp_cache_mb.saturating_mul(1024 * 1024),
    )?);
    *reporter.cache.lock().unwrap() = Some(cache.clone());
    let result = build_myisam_archive_with_progress(
        cfg,
        index,
        source,
        topics,
        None,
        reporter.clone(),
        |name| ftp_range::stream_named_member(urls.clone(), cache.clone(), name),
    );
    reporter.log(source);
    reporter.cache.lock().unwrap().take();
    info!(?result, stats = ?cache.stats(), "FTP bootstrap finished");
    result
}

fn ingest_columns(table: &str) -> &'static [&'static str] {
    match table {
        "editions" => &[
            "e_id",
            "libgen_topic",
            "visible",
            "title",
            "author",
            "series_name",
            "publisher",
            "year",
        ],
        "editions_add_descr" => &["e_id", "key", "value"],
        "editions_to_files" => &["e_id", "f_id"],
        "files" => &[
            "f_id",
            "libgen_id",
            "fiction_id",
            "libgen_topic",
            "extension",
            "visible",
            "broken",
            "md5",
            "filesize",
            "pages",
        ],
        "elem_descr" => &["key", "name_en"],
        _ => unreachable!("unknown ingest table"),
    }
}

pub(super) fn build_myisam_archive(
    cfg: &Config,
    index: &SearchIndex,
    source: &str,
    topics: &[String],
    limit: Option<u64>,
    members: impl FnMut(&str) -> std::io::Result<ftp_range::StreamedMember>,
) -> Result<DumpStats> {
    build_myisam_archive_with_progress(
        cfg,
        index,
        source,
        topics,
        limit,
        BootstrapReporter::new(),
        members,
    )
}

#[allow(clippy::too_many_arguments)]
fn build_myisam_archive_with_progress(
    cfg: &Config,
    index: &SearchIndex,
    source: &str,
    topics: &[String],
    limit: Option<u64>,
    reporter: Arc<BootstrapReporter>,
    mut members: impl FnMut(&str) -> std::io::Result<ftp_range::StreamedMember>,
) -> Result<DumpStats> {
    let topic_set = topics.iter().cloned().collect::<HashSet<_>>();
    let extensions = cfg
        .indexer
        .extensions
        .iter()
        .map(|e| e.to_lowercase())
        .collect::<HashSet<_>>();
    let opts = StageOptions {
        extensions: &extensions,
        topics: &topic_set,
        language_key: cfg.indexer.language_key,
        isbn_key: cfg.indexer.isbn_key,
    };
    let stage_path = cfg.paths.data_dir.join(format!("stage-{source}.sqlite"));
    let mut stage = Stage::create(&stage_path)?;
    stage.load_myisam(&opts, |on_row| {
        for table in [
            "editions",
            "editions_add_descr",
            "editions_to_files",
            "files",
            "elem_descr",
        ] {
            reporter.begin(
                "opening table metadata",
                Some(table),
                Some(&format!("{table}.frm")),
                0,
                0,
            );
            let frm = read_archive_metadata(members(&format!("{table}.frm"))?, false)?;
            let schema =
                myisam::FrmSchema::parse(&frm).with_context(|| format!("parsing {table}.frm"))?;
            reporter.begin(
                "opening table metadata",
                Some(table),
                Some(&format!("{table}.MYI")),
                0,
                0,
            );
            let myi = read_archive_metadata(members(&format!("{table}.MYI"))?, true)?;
            let info = myisam::MyisamInfo::parse_myi(&myi)
                .with_context(|| format!("parsing {table}.MYI"))?;
            let mut decoder = schema.decoder(&info)?.project(ingest_columns(table))?;
            if table == "editions_add_descr" {
                decoder = decoder.with_value_limit(64);
            }
            reporter.begin(
                "opening table data",
                Some(table),
                Some(&format!("{table}.MYD")),
                info.record_count,
                0,
            );
            let (member, length, stream, worker) = members(&format!("{table}.MYD"))?;
            reporter.begin(
                "decoding and staging table",
                Some(table),
                Some(&member),
                limit.map_or(info.record_count, |n| n.min(info.record_count)),
                length,
            );
            let stream = ProgressReader {
                inner: stream,
                reporter: reporter.clone(),
            };
            let mut decoded = 0u64;
            info!(
                table,
                expected_rows = info.record_count,
                bytes = length,
                "streaming MyISAM table"
            );
            let walk = myisam::walk_records_in(
                stream,
                length,
                &info,
                limit,
                &cfg.paths.data_dir,
                |packed| {
                    let row = decoder.unpack(packed)?;
                    on_row(table, &schema, &row).map_err(std::io::Error::other)?;
                    decoded += 1;
                    reporter.rows.store(decoded, Ordering::Relaxed);
                    if decoded.is_multiple_of(1_000_000) {
                        info!(table, decoded, "MyISAM rows decoded");
                    }
                    Ok(())
                },
            );
            let extraction = worker
                .join()
                .map_err(|_| anyhow::anyhow!("RAR extractor for {member} panicked"))?;
            let stats = walk.with_context(|| format!("decoding {member}"))?;
            let complete = stats.bytes_consumed == length;
            if complete {
                extraction.with_context(|| format!("extracting {member}"))?;
            }
            if complete && stats.records != info.record_count {
                bail!(
                    "{member}: .MYI declares {} rows but stream decoded {}",
                    info.record_count,
                    stats.records
                );
            }
            info!(
                table,
                rows = stats.records,
                fragments = stats.fragmented_records,
                deleted = stats.deleted_blocks,
                bytes = stats.bytes_consumed,
                complete,
                spill_bytes = stats.spill_disk_bytes,
                "MyISAM member streamed"
            );
            reporter.log(source);
        }
        reporter.begin("building staging indexes", None, None, 0, 0);
        Ok(())
    })?;
    reporter.begin("joining and indexing books", None, None, 0, 0);
    let mut writer = index.writer(cfg.indexer.writer_memory_mb)?;
    writer.delete_source(source);
    let mut count = 0u64;
    let res = stage.for_each_book(source, &opts, |book| {
        writer.add(&book)?;
        count += 1;
        reporter.rows.store(count, Ordering::Relaxed);
        if count.is_multiple_of(500_000) {
            info!(source, count, "indexing");
        }
        Ok(())
    });
    match res {
        Ok(()) => {
            reporter.log(source);
            reporter.begin("committing search index", None, None, 0, 0);
            writer.commit()?;
        }
        Err(e) => {
            writer.rollback()?;
            return Err(e);
        }
    }
    let max_ids = std::mem::take(&mut stage.max_ids);
    drop(stage);
    reporter.begin("bootstrap complete", None, None, count, 0);
    reporter.rows.store(count, Ordering::Relaxed);
    Ok((count, max_ids))
}

/// Read small schema metadata, or just the leading header of a multi-gigabyte MyISAM index.
fn read_archive_metadata(member: ftp_range::StreamedMember, myi: bool) -> Result<Vec<u8>> {
    let (name, _, mut stream, worker) = member;
    let read = if myi {
        myisam::read_myi_header(&mut stream)
    } else {
        let mut bytes = Vec::new();
        stream
            .by_ref()
            .take(1_048_577)
            .read_to_end(&mut bytes)
            .and_then(|_| {
                if bytes.len() > 1_048_576 {
                    Err(std::io::Error::other(
                        "FRM exceeds the 1 MiB metadata bound",
                    ))
                } else {
                    Ok(bytes)
                }
            })
    };
    drop(stream);
    let extraction = worker
        .join()
        .map_err(|_| anyhow::anyhow!("RAR extractor for {name} panicked"))?;
    let bytes = read.with_context(|| format!("reading metadata from {name}"))?;
    // Reading a .MYI prefix deliberately stops its producer before its full-file CRC.
    if !myi {
        extraction.with_context(|| format!("extracting {name}"))?;
    }
    Ok(bytes)
}
