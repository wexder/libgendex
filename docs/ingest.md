# Snapshot ingestion and verification

The production ingestion path uses native Rust. FTP access, RAR decompression, MySQL 5.x
schema parsing, MyISAM row decoding, staging and search indexing run inside the Rust binary.
The runtime does not invoke curl, unrar, MySQL or MariaDB.
Remote bootstrap accepts FTP RAR snapshots. Directory discovery validates the volume order
before opening the archive. HTTP endpoints are used for daily API refreshes and book downloads.

## Verify the production ingest path

Run from the repository root, with a separate work directory and cache directory:

```sh
cargo run --release -- verify-ingest \
  ftp://ftp.libgen.bz/upload/dbbackup/libgen_new-2026-09-06.part001.rar \
  62 /tmp/bookjev-verification /tmp/bookjev-verification-cache 0
```

The final argument is the maximum number of active rows to read from each table. Zero reads the
complete snapshot, validates extraction CRCs and compares every table's active-row count with its
`.MYI` header. A positive number is a quick sample; it does not verify the unread remainder. Small
samples can contain no matching edition/file links and fail the search check; that failure does not
mean those table samples could not be decoded.

This uses the configured collections, extensions and `ftp_cache_mb` limit. After indexing it checks
MD5 lookup, full-text search and staging-file cleanup. It prints a JSON report after progress logs.
A repeat run reuses cached ranges. A bounded cache cannot retain every range of a large snapshot, so
a full repeat still downloads evicted ranges. Verification rebuilds only its dedicated index; use a
separate directory from the running service.

For fixture capture and independent SQL reference generation, see the
[`myisam-reader` test harness](../crates/myisam-reader/README.md#optional-fixture-capture-and-independent-reference-generation).

## Automated checks

```sh
cargo test --workspace --all-targets
```

Tests build a small RAR/MyISAM fixture in Rust and serve it through a local Rust FTP server. They
exercise cold and warm cache reads, short-transfer retries, sequential RETR reuse, seek behavior,
LRU eviction, cache reopening, concurrent writers, fixed and dynamic records, NULLs, BLOBs, extended
VARCHAR lengths, forward/backward fragmentation, temporary fragment spill, search indexing and
failure cleanup. An import with a mismatched MyISAM row count must preserve the previous index.
A daily-refresh test verifies that bootstrapped sources skip snapshot discovery.

The separate `myisam-reader` crate tests all five real table layouts (4,217 rows) against independent
MariaDB SQL expectations. The checked-in descriptor dictionary also validates a complete real table: 217 active
rows, 4 deleted blocks, 57 continuation blocks and 38 fragmented rows. Descriptor 101 is Language;
505 is ISBN. See `crates/myisam-reader/tests/fixtures/libgen-2026-09-06/README.md` for its source.

## Storage and performance

- Only the five required data tables and their small schema/index headers are read. Multi-gigabyte
  `.MYI` indexes are skipped after reading their leading headers.
- RAR members stream through a bounded channel; complete volumes and extracted `.MYD` files are
  not saved. The archive is non-solid, so unrelated members can be skipped.
- FTP uses `SIZE`, `REST` and `RETR`. Consecutive uncached 64 KiB ranges reuse one transfer. Failed
  or short reads reconnect and retry with bounded timeouts/backoff.
- Cached source blocks are persistent and capped (512 MiB by default); eviction is logarithmic in
  the number of cached blocks. A directory lock prevents concurrent processes from corrupting one
  cache. Zero disables disk caching. The cap counts source bytes; filesystem overhead, tiny size
  metadata and the lock file are additional.
- Only selected collections and useful scalar fields go into temporary SQLite staging. Long
  descriptions and irrelevant BLOBs are decoded without copying them into staging.
- Distant or numerous unresolved row fragments spill to temporary SQLite rather than retaining an
  unbounded memory working set. Scratch files are removed after success or error.
- Allow space for staging and the final Tantivy index. The cache cap is not a cap on total application
  storage. An interrupted full bootstrap starts decoding again; cached ranges survive the restart.

The native reader is scoped to the legacy MySQL 5.x FRM and fixed/dynamic MyISAM layouts used by this
snapshot. It explicitly rejects `myisampack` compressed tables. Supporting future layouts requires
reader changes; a bundled MariaDB would handle more formats but require complete extracted table
files, a database runtime and substantially more storage.

## Observed full import

A user run reached the final `source indexed` message in about 80 minutes. This confirms
completion of FTP reads, native RAR/MyISAM decoding, staging, and search index commit for that
run. Hardware, mirror speed, and selected collections affect the result; this is an observed
end-to-end time, not a parser-only benchmark or a guaranteed duration.
