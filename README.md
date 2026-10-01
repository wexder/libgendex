# bookjev

Self-hosted search over the Library Genesis catalogue. A single Rust binary builds a local full-text
index from the official LibGen metadata dumps, serves a SolidJS search UI, and fetches books either
to your browser or into a server-side library folder organised by author.

```
                 ┌────────────── bookjev (one process) ───────────────┐
 libgen.li  ──►  │ indexer: discover → FTP range cache → Rust RAR     │
 FTP mirror      │   → MyISAM stream decoder → sqlite staging (tmp)  │
                 │   → Tantivy index  (data/index)                    │
                 │                                                    │
 browser    ◄──► │ axum API (/api/*, OpenAPI)  +  static SolidJS UI   │
                 │ search: BM25 + file heuristics                     │ ··► remote OpenJev
                 │   [optional AI rerank: api │ local model]          │     (provider = api)
 library/   ◄──  │ downloads: mirror resolver → <Author>/<Title>.ext  │
                 └────────────────────────────────────────────────────┘
```

## Quick start (container)

The release workflow publishes images to **`ghcr.io/wexder/libgendex`** for Linux amd64 and arm64.
The default image includes native FTP/RAR/MyISAM ingestion, the UI, and optional remote ranking.

```sh
docker run -d --name bookjev --restart unless-stopped \
  -p 8080:8080 \
  -v bookjev-data:/data \
  -v bookjev-library:/library \
  ghcr.io/wexder/libgendex:0.2.0
```

Open **http://localhost:8080**. The first index import runs in the background; follow it with
`docker logs -f bookjev`. Named volumes persist the index/cache and library across restarts.
For Compose, use `docker compose up -d --pull always`. To build from source instead, use
`docker compose up -d --build`. Compose uses host directories: create `data` and `library` first
and ensure both are writable by UID/GID 1000.

## Kubernetes (Helm)

The release workflow publishes the OCI chart to **`oci://ghcr.io/wexder/charts/libgendex`**:

```sh
helm upgrade --install libgendex oci://ghcr.io/wexder/charts/libgendex \
  --version 0.2.0 --namespace libgendex --create-namespace

kubectl -n libgendex port-forward service/libgendex-libgendex 8080:80
```

The chart creates one instance, a 50 GiB data PVC, and a 100 GiB library PVC using the cluster's
default storage class. Size these for your collections and saved books: the 512 MiB FTP cache
limit excludes staging, the final index, and downloads. Claims are retained after uninstall.
Upgrades use `Recreate` to keep one writer and briefly interrupt serving. Indexing progress is at
`GET /api/index/status`; health probes check the server and do not wait for the first import.

See the [chart README](charts/libgendex/README.md) for ingress/TLS, configuration, private registry
authentication, existing claims, and storage retention.
The library can also mount an existing NFS export using `persistence.library.nfs`; see the
[NFS configuration example](charts/libgendex/README.md#library-on-nfs).

### How the index is built and kept current

The index is bootstrapped from the LibGen FTP snapshot, then kept current through the LibGen API.

1. **Bootstrap (once).** bookjev finds the newest complete snapshot on the FTP mirror
   (`ftp://ftp.libgen.bz/upload/dbbackup/`, `libgen_new-<date>.partNNN.rar`). Rust reads RAR
   ranges on demand into a persistent, size capped disk cache (512 MiB by default), streams the
   MyISAM tables through the native decoder, and keeps only the configured collections. It does
   not save the complete archive or extracted table files. Allow additional disk space for the
   Tantivy index and temporary SQLite staging database. Distant unresolved MyISAM fragments can
   use temporary scratch files, which are removed after the table finishes.
2. **New files by id.** libgen.li's per-collection file ids (`fiction_id`, `libgen_id`) are the
   same ids the dumps contain, so after import bookjev walks
   `json.php?object=f&topic=f|l&id_start=…&id_end=…` upward from the highest id in the dump –
   like update_libgen's `idnewer` – and indexes every new file. Progress is saved per window.
3. **Changes by day.** File changes (removals, edits) of the last `modified_catchup_days` (7) are
   replayed day by day via `mode=modified`, then one day per refresh (`refresh_interval`, 24 h).

Progress shows in the UI footer, `GET /api/index/status` and the logs.
FTP bootstrap logs a heartbeat every 10 seconds, including the current member/table, rows,
uncompressed bytes read, elapsed time, processing rate, and FTP cache activity. Archive header
discovery also reports completed volumes. The status API's `bootstrap` object exposes the same
step counters; the footer shows table rows, byte progress, processing speed, and elapsed time.
Byte and row totals describe the current table, not overall import completion. During metadata
discovery, joins, and commit, totals can be unknown; the phase and elapsed time still update.

### Try it without the real dumps

```sh
make sample   # writes data/sample.sql (39 records in the real libgen.li schema)
```

and use a source with `local_path = "data/sample.sql"` (or mount it into the container).

## Configuration

Defaults → `bookjev.toml` (path via `BOOKJEV_CONFIG`) → environment variables
`BOOKJEV_<SECTION>__<KEY>`. See [`bookjev.example.toml`](bookjev.example.toml) for every option.
Useful ones:

| Env var | Meaning |
|---|---|
| `BOOKJEV_PATHS__DATA_DIR` | index, state, capped FTP range cache and temporary staging (container: `/data`) |
| `BOOKJEV_PATHS__LIBRARY_DIR` | server-side book library (container: `/library`) |
| `BOOKJEV_INDEXER__REFRESH_INTERVAL` | how often to refresh through the HTTP API (`24h`) |
| `BOOKJEV_INDEXER__FTP_CACHE_MB` | persistent FTP range cache cap in MiB (`512`; `0` disables it) |
| `BOOKJEV_RANKING__PROVIDER` | optional AI re-ranking: `none` (default), `api` or `local` |
| `BOOKJEV_RANKING__API__URL` / `__API__API_KEY` | remote OpenJev server |
| `BOOKJEV_RANKING__LOCAL__THREADS` / `__LOCAL__MODEL_PATH` | local model threads (0 = auto) / local GGUF instead of downloading |
| `BOOKJEV_LOG__LEVEL` / `BOOKJEV_LOG__JSON` | logging (`RUST_LOG` also works) |

## Ranking

Search runs BM25 over title (×3), author (×2), series and publisher with ASCII folding, so
`exupery` finds *Saint-Exupéry*; ISBNs match exactly. All terms are required first, then one-typo
fuzzy matching, then any term. Results are then ordered with file heuristics: format preference
(EPUB > AZW3 > MOBI > FB2 > … > PDF > DJVU), file-size sanity, metadata completeness and a
study-guide/summary keyword penalty.

**AI re-ranking is an optional component** (`ranking.provider`, default `none`). When enabled it
re-orders the top `rerank_top` hits using OpenJev-style typed questions, answered from model
probabilities rather than generated text:

| `provider` | What runs | Cost |
|---|---|---|
| `none` | nothing extra | – |
| `api` | each hit is sent to a remote [OpenJev](https://github.com/razorback16/openjev) (or TypeSafe Jev) `/v1/systemone` server: relevance (score), e-reader suitability (score), genuine edition (yes/no) | network only |
| `local` | in-process [Qwen3-1.7B](https://huggingface.co/Qwen/Qwen3-1.7B) (Q4_K_M GGUF via llama.cpp) on the CPU; needs a build with the `local` feature | ~1.1 GB model download, ~0.9 GB RAM, 2–4 s per new query on a 6-core desktop CPU |

`local` scores the top 8 distinct works (title + author, so all formats of a book share one
answer) with two yes/no questions: *is this exactly the book being searched for?* and *is this a
study guide, summary or companion rather than the book itself?* The query and each work are
evaluated once and the questions reuse that KV cache. Answers are log-odds, calibrated within
the search, and cached per (query, work). E-reader fit always comes from the file heuristic.

Whatever the provider, a failure or timeout falls back to the heuristic, and the UI first shows
instant results (`?ai=false`) and swaps in the AI-ranked list when it arrives.

Building with the local model:

```sh
cargo build --release --features local                        # needs cmake + a C++ toolchain
docker build --build-arg FEATURES=local -t bookjev:local .     # or uncomment the arg in docker-compose.yml
```

## Downloads

- **Download** streams the file through the server to the browser with a proper file name.
- **Save to library** downloads on the server to `<library>/<First Author>/<Title> (<year>).<ext>`
  (written to `.part` and renamed on completion). Active jobs show in the header panel.

File links are resolved through `download.resolvers`: by default libgen.li-style mirrors, whose
`ads.php?md5=` page links a `get.php` URL.

## API

OpenAPI spec: `GET /api/openapi.json` (also committed as [`openapi.json`](openapi.json)).
The frontend client in `web/src/api` is generated from it with `@hey-api/openapi-ts`.

| | |
|---|---|
| `GET /api/search?q=&ext=&lang=&limit=&ai=` | ranked results (`ai=false`: skip the AI rerank) |
| `GET /api/books/{md5}` | metadata |
| `GET /api/books/{md5}/file` | stream the file to the client |
| `POST /api/books/{md5}/save` | queue a library download |
| `GET /api/downloads` | library download jobs |
| `GET /api/index/status`, `POST /api/index/refresh` | indexer state / trigger a refresh |
| `GET /api/health` | liveness + book count |

## Development

```sh
nix develop                      # or direnv; provides rust nightly, node, sqlite, cmake
make openapi                     # regenerate openapi.json after API changes
cd web && npm ci && npm run gen:api && npm run dev   # UI on :5173, proxies /api to :8080
cargo run                        # API on :8080; RAR and MyISAM ingest run in Rust
cargo run --features local       # with the in-process ranking model
cargo test --workspace --all-targets
```

The parser is a separate [`myisam-reader` crate](crates/myisam-reader/README.md), with offline tests
against real snapshot bytes and independently generated SQL expectations. Run
`cargo test --workspace --all-targets` for the parser and application integration checks. A full live snapshot
verification command and storage details are in
[`docs/ingest.md`](docs/ingest.md).

## Releases

After committing your changes, bump the version and push its release commit and tag:

```sh
./scripts/release.mjs patch --dry-run
./scripts/release.mjs patch   # Also accepts minor, major, or an explicit version
```

GitHub Actions validates pull requests and publishes the container and OCI chart on version tags.
Container tags are `0.2.0`, `v0.2.0`, and `latest` for stable releases; chart versions omit `v`.
See [release preparation and publishing](docs/releasing.md) for versioning, package visibility,
CI checks, and the tag workflow. This checkout prepares the publishing setup; artifacts become
available after a successful release workflow.

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
