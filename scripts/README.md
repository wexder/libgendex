# Development scripts

## Release

`release.mjs` bumps versions, creates a release commit and annotated tag, and pushes both
to GitHub. Requires Node.js and Git with push access to the remote.

```sh
./scripts/release.mjs patch --dry-run
./scripts/release.mjs patch       # Default: next patch release
./scripts/release.mjs minor
./scripts/release.mjs major
./scripts/release.mjs 0.2.0-rc.1  # Explicit SemVer, must increase
```

The remote defaults to `origin`; override it with `--remote NAME`. A real release requires a
clean checkout on a branch, matching Cargo/chart/lockfile/OpenAPI versions, and an unused tag.
The script updates the root package and lockfile, chart, OpenAPI version, Docker/Compose
defaults, and documentation examples. It pushes the current branch and tag atomically,
including any unpushed commits on that branch. It does not run tests; the tag triggers release CI.

See the [release guide](../docs/releasing.md) for publishing and push failure recovery.

## Sample metadata

`gen-sample-dump.pl` generates synthetic SQL metadata for trying the application locally:

```sh
make sample
```

It requires Perl and writes `data/sample.sql`. Configuration instructions are in the
[development guide](../docs/development.md#sample-metadata).

The parser's optional SQL reference generator lives in
[`crates/myisam-reader/tools`](../crates/myisam-reader/README.md#optional-fixture-capture-and-independent-reference-generation).
