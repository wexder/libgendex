# Real MySQL 5.x snapshot fixtures

Captured from `ftp://ftp.libgen.bz/upload/dbbackup/libgen_new-2026-09-06.part001.rar`
(62 volumes) on 2026-10-01 using the native Rust FTP/RAR reader.

- Four large data files are physical prefixes ending after 1,000 complete active records.
- `elem_descr.MYD` is complete: 217 active rows, 4 deleted blocks, 57 continuation blocks,
  38 fragmented records. Descriptor 101 is Language; 505 is ISBN.
- FRM files and leading MYI headers are original bytes. Source row counts are intentionally
  retained and exceed the prefix counts for the large tables.
- `manifest.json` records source URL, bounds, schema metadata, byte hashes, sample scalars and
  native decoded-byte regression hashes.
- `mysql-reference.json` contains independent MariaDB SQL output for every selected scalar of
  all 4,217 fixture rows. Omitted indexes were rebuilt; original data bytes were checked unchanged.

Fixtures contain catalog metadata and descriptor names, not book contents. Together the binary
schema/header/data fixtures are about 0.6 MiB; expectations add about 0.8 MiB.

See the crate README for capture/reference-generation commands and test coverage.
