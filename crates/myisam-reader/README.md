# myisam-reader

A separate Rust crate for legacy MySQL 5.x FRM schemas and fixed/dynamic MyISAM records. Bookjev
uses it through a path dependency. FTP, RAR, collection filtering and search indexing stay in the
application; this crate accepts any `std::io::Read` source and starts no external programs.

## API

```rust,no_run
use std::{fs, io};
use myisam_reader::{FrmSchema, MyisamInfo, read_myi_header, value, walk_records};

# fn main() -> io::Result<()> {
let schema = FrmSchema::parse(&fs::read("editions.frm")?)?;
let header = read_myi_header(fs::File::open("editions.MYI")?)?;
let info = MyisamInfo::parse_myi(&header)?;
let decoder = schema.decoder(&info)?.project(&["e_id", "title"])?;
let data = fs::File::open("editions.MYD")?;
let length = data.metadata()?.len();
let stats = walk_records(data, length, &info, None, |packed| {
    let row = decoder.unpack(packed)?;
    println!("{:?}", value(&schema, &row, "title")?);
    Ok(())
})?;
assert_eq!(stats.records, info.record_count);
# Ok(())
# }
```

- `read_myi_header` reads only the bounded leading header, even if the index is many gigabytes.
- `walk_records` walks fixed or dynamic data as a forward stream, including deleted blocks and
  arbitrarily placed forward/backward continuation blocks. `walk_records_in` selects the scratch
  directory used for fragment spill. Scratch files are removed on success or error.
- `walk_records_with_options` accepts `WalkOptions` to set row/record limits, fragment memory
  budgets and scratch storage. Zero pending/earlier budgets force fragment storage onto disk.
  Pending rows use ordered eviction, avoiding a full working-set scan for each spill.
- `FrmSchema::decoder` binds physical record definitions to SQL columns by record offsets. Reserved
  NULL bitmap bytes do not have SQL columns. Reuse one decoder for all records of a table.
- `RowDecoder::project` copies only requested columns; all fields are still consumed and validated.
  `with_value_limit` avoids copying oversized values. Skipped fields and SQL NULL each return a
  `None` slot, so use an unrestricted decoder if that distinction matters.
- `number` converts supported unsigned integer/ENUM/SET slots. `value` returns SQL text for those
  slots and UTF-8 text fields, preserving VARCHAR/TEXT whitespace and removing CHAR right padding.
- The walker reports counts and bytes but does not assume that input is a complete snapshot. A
  prefix fixture retains its original `.MYI` row count; the caller decides whether to compare it.

Support is scoped to the tested MySQL 5.x layouts. `myisampack` compressed tables are rejected.
This is a physical row reader, not a complete SQL type/charset conversion engine: FLOAT, DECIMAL,
date/time formatting and non-UTF-8 text conversion are not provided by the scalar helpers.
Row checksum bytes are consumed; the caller should verify source integrity (Bookjev checks RAR
member CRCs). Fragment assembly uses RAM budgets of 128 MiB for pending rows and 32 MiB for earlier
continuations, with a 128 MiB single-row safety limit. Temporary assembly allocations and returned
rows use additional memory. Distant or excessive unresolved fragments use embedded SQLite scratch
storage; the complete extracted table is not saved.

## Offline test harness

```sh
cargo test -p myisam-reader
cargo test --workspace --all-targets
cargo clippy --workspace --all-targets -- -D warnings
```

The checked-in September 2026 snapshot fixtures cover all five production table layouts:

| Table | Active rows in fixture | Data bytes | Coverage |
|---|---:|---:|---|
| editions | 1,000 | 99,664 | dynamic records, title/author text, packing and visibility |
| editions_add_descr | 1,000 | 54,048 | descriptors and nullable BLOB values |
| editions_to_files | 1,000 | 27,000 | fixed records and integer edition/file links |
| files | 1,000 | 247,056 | many columns, hidden NULL fields, ENUMs, MD5 and file metadata |
| elem_descr | 217 | 27,680 | complete table, 38 fragmented rows, 57 continuations, 4 deletions |

The four large tables are bounded physical prefixes, not rewritten synthetic tables. Their FRM
and MYI headers remain unchanged, including the original full-snapshot row counts.

For each fixture the harness checks:

1. SHA-256 of the original schema, index header and data bytes.
2. Column names, types, offsets, lengths, ENUM labels, NULL metadata and physical record options.
3. A canonical decoded-row digest across every field (native regression baseline).
4. Counts, fragmentation/deletion statistics and exact bytes consumed with 1-, 7-, 64-, 4096-byte
   and unrestricted reads, exercising arbitrary stream boundaries.
5. Projection equivalence, unknown-column errors, and truncated schema/index/data rejection.
6. The same decoded digest with fragment memory budgets set to zero, verifying the disk path and
   scratch cleanup after both success and an intentional consumer error.
7. Every selected scalar field of every row against an independent SQL reference: 4,217 rows read
   by MariaDB 11.4.9. This comparison is a multiset because stream reconstruction can complete
   fragmented rows in a different order from a database scan.

Unit tests cover packing flag variants, short/extended VARCHAR lengths, NULLs, BLOBs, projection
limits, CHAR/VARCHAR whitespace, ENUM/SET values, malformed-header mutation, forward/backward
fragments, every dynamic block layout, spill round trips, collisions and scratch cleanup after
errors. A 10,000-row pressure case verifies that many pending fragments spill and complete correctly.

The SQL cross-check found a VARCHAR whitespace bug during harness development. A regression test
and the real title with a trailing space now preserve that behavior correctly.

### Optional fixture capture and independent reference generation

Normal tests need neither a network connection nor MariaDB. Refresh explicitly from the repository
root when changing fixture data:

```sh
cargo run --release --example capture-myisam-fixtures -- \
  ftp://ftp.libgen.bz/upload/dbbackup/libgen_new-2026-09-06.part001.rar \
  62 /tmp/bookjev-fixture-cache crates/myisam-reader/tests/fixtures/libgen-2026-09-06 1000

nix shell nixpkgs#mariadb --command bash crates/myisam-reader/tools/generate-reference.sh
```

The optional reference script requires Node and MariaDB development tools. It creates an isolated
temporary database with networking disabled, rebuilds only the omitted indexes with `myisamchk`,
checks that `.MYD` bytes remain unchanged, queries every fixture row, and writes
`mysql-reference.json`. The server and temporary files are removed afterwards. These tools are
absent from the production binary's dependencies and runtime Docker image.

Review refreshed expectations before accepting them. Hashes generated by the native reader are
regression baselines; the SQL reference supplies the independent scalar-value check. Fixture source,
bounds and hashes are recorded in `tests/fixtures/libgen-2026-09-06/manifest.json`.

## Repeatable performance measurement

```sh
cargo bench -p myisam-reader --bench real_data -- --iterations 5000
```

This reads fixture bytes into RAM once and reports decode-only rows/s and MiB/s for each real table.
It excludes FTP, RAR decompression, disk staging and indexing, so it does not predict full bootstrap
wall time. The fixture sizes are small and warm; benchmark results depend on CPU/build options.

An i5-10400F run measured about 0.72 million rows/s for the wide `files` table and 9.2 million
rows/s for fixed edition/file links (128–237 MiB/s across the five fixtures). The exact command,
hardware and measurements are saved in [`benchmarks/2026-10-01.json`](benchmarks/2026-10-01.json).
