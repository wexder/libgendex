# Development

```sh
nix develop                      # or direnv; provides rust nightly, node, sqlite, cmake
make openapi                     # regenerate openapi.json after API changes
cd web && npm ci && npm run gen:api && npm run dev   # UI on :5173, proxies /api to :8080
cargo run                        # API on :8080; RAR and MyISAM ingest run in Rust
cargo run --features local       # with the in-process ranking model
cargo test --workspace --all-targets
```

The parser is a separate [`myisam-reader` crate](../crates/myisam-reader/README.md), with offline tests
against real snapshot bytes and independently generated SQL expectations. Run
`cargo test --workspace --all-targets` for the parser and application integration checks. A full live snapshot
verification command and storage details are in
[`ingest.md`](ingest.md).

## Sample metadata

```sh
make sample   # writes data/sample.sql (39 records in the real libgen.li schema)
```

and use a source with `local_path = "data/sample.sql"` (or mount it into the container).

## Design notes

- **Tantivy over SeekStorm**: both are Rust and fast; Tantivy is the more mature embedded library,
  keeps the index on disk via mmap (small resident memory), and has built-in fuzzy queries and
  tokenizer filters. SeekStorm's advantages are mostly in throughput at a scale we don't need.
- **Storage**: the search index, FTP range cache, and `data/state.json` persist between runs.
  A temporary SQLite file is used during ingest to join `editions`, `editions_to_files`,
  `files` and the language/ISBN descriptors without holding millions of rows in RAM; it is
  deleted afterwards.
- **Updates via API**: `json.php?object=f&mode=modified&timefirst=D&timelast=D+1` per day
  (longer ranges return nothing; a future `timelast` too), editions looked up by id in batches.
  Pages are `limit1=N` (first N) then `limit1=offset&limit2=N`: `limit1=0` returns *everything*,
  which on busy days takes the server over 45 s. Paging stops when a page brings nothing new. Edition-only edits are not followed: some days have tens of thousands of them.
  Files already in the index are not re-fetched (most "modified" files are bookkeeping touches), so
  only new files cost edition lookups. The mirrors only answer browser user agents (curl's default gets a stub nginx page).
  Bootstrapping through the API instead of a dump is impractical: the files table has ~116 M ids
  (mostly scientific articles) and cannot be filtered by collection server-side.
- **Snapshot ingest**: libgen.li publishes multi-volume RAR archives containing MyISAM table files
  (`.frm`, `.MYI`, `.MYD`). The Rust RAR reader fetches cached FTP ranges and streams `.MYD` records
  into bounded SQLite staging; no external `unrar` or MariaDB process is needed. Language and ISBN
  descriptor keys are resolved from `elem_descr`.
- **CPU targets**: published amd64 images use the portable `x86-64` baseline. Development builds
  use `x86-64-v3` (AVX2/FMA) via `.cargo/config.toml`. For a local-model image on compatible
  hardware, build with `--build-arg FEATURES=local --build-arg TARGET_CPU=x86-64-v3`.
- **Resources**: idle RSS is a few MB without the local model. Ingesting a synthetic 1.17M-book dump peaked at ~145 MB
  RSS and took ~15 s after download; searches over it take ~10 ms.
