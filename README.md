# Libgendex

Self-hosted search and downloads for the Library Genesis catalogue. A single Rust binary serves
the web UI, imports metadata through a bounded FTP cache, and refreshes it daily through the API.
RAR and MyISAM processing run natively, with no MariaDB or external archive tools.

## Docker

```sh
docker run -d --name libgendex --restart unless-stopped \
  -p 8080:8080 \
  -v libgendex-data:/data \
  -v libgendex-library:/library \
  ghcr.io/wexder/libgendex:0.2.0
```

Open **http://localhost:8080**. The first import runs in the background; follow it with
`docker logs -f libgendex`. Images support amd64 and arm64.

With Compose, run `docker compose up -d --pull always`, or `docker compose up -d --build` to build
locally. Create its host directories `data/` and `library/` with write access for UID/GID 1000.

## Kubernetes

```sh
helm upgrade --install libgendex oci://ghcr.io/wexder/charts/libgendex \
  --version 0.2.0 --namespace libgendex --create-namespace

kubectl -n libgendex port-forward service/libgendex-libgendex 8080:80
```

The chart runs one instance with retained data and library PVCs. It also supports an existing NFS
export for the library. See the [chart guide](charts/libgendex/README.md) for storage, NFS, ingress,
configuration, and private registry access.

## Configuration and storage

Settings come from defaults, then `libgendex.toml`, then `LIBGENDEX_<SECTION>__<KEY>` environment
variables. Use `LIBGENDEX_CONFIG` to choose another configuration path.
See [libgendex.example.toml](libgendex.example.toml) for all options.

| Environment variable | Default / purpose |
| --- | --- |
| `LIBGENDEX_PATHS__DATA_DIR` | `/data` in the container: index, state, cache, temporary staging |
| `LIBGENDEX_PATHS__LIBRARY_DIR` | `/library` in the container: saved books |
| `LIBGENDEX_INDEXER__FTP_CACHE_MB` | `512` MiB cache cap; `0` disables caching |
| `LIBGENDEX_INDEXER__REFRESH_INTERVAL` | `24h` API refresh interval |
| `LIBGENDEX_RANKING__PROVIDER` | `none`; optional `api` or a custom build with `local` |

The cache cap excludes the final index, SQLite staging, and downloaded books. Import progress is
shown in the UI, logs, and `GET /api/index/status`. Files can be downloaded to your browser or
saved to the server library. [Ingestion details](docs/ingest.md) · [Search and ranking](docs/ranking.md)
· [Downloads and API](docs/api.md).

## Development

```sh
nix develop
make build          # Build the UI and Rust binary
cargo run           # Serve on port 8080
```

See the [development guide](docs/development.md) and the separate
[MyISAM reader crate](crates/myisam-reader/README.md).

## Releases

After committing your changes:

```sh
./scripts/release.mjs patch --dry-run
./scripts/release.mjs patch
```

The script also accepts `minor`, `major`, or an explicit version. It bumps versions, creates a
commit and annotated tag, and pushes both. GitHub Actions publishes the container and Helm chart
to GHCR. See the [release guide](docs/releasing.md).
