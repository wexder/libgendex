# Releases

Repository: [wexder/libgendex](https://github.com/wexder/libgendex).

| Artifact | Registry reference |
| --- | --- |
| Container | `ghcr.io/wexder/libgendex:VERSION` |
| Helm chart | `oci://ghcr.io/wexder/charts/bookjev` with `--version VERSION` |

The workflows derive the owner/repository from GitHub and lowercase registry names. The chart
name remains `bookjev`. Published chart defaults point at the container from the same release.

## Prepare a version

1. Update the root package version in `Cargo.toml`.
2. Run `cargo check` to update the root package version in `Cargo.lock`.
3. Update `version` and `appVersion` in `charts/bookjev/Chart.yaml` to that same version.
4. Update the example release versions in README, chart README, and Compose.
5. Run `make openapi` after a package version bump, since the spec includes the application version.
6. Run the checks, review, commit, and push the changes before tagging.

```sh
node .github/scripts/prepare-release.mjs --check
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --all-targets --locked
cargo test -p myisam-reader --doc --locked
helm lint charts/bookjev --strict
docker build -t bookjev:release-check .
```

Container and CI builds use stable Rust 1.99.0. The current lockfile requires Rust 1.97.1 or later.
The CI workflow overrides developer-only nightly flags and linker/wrapper settings. Docker builds
ignore `.cargo/config.toml`. Default release images use portable x86-64 code on amd64 and native
arm64 code on arm64. The runtime contains the Rust binary and UI with no MariaDB or unrar.

## Publish

After merging the prepared version, push its tag:

```sh
git tag -a v0.1.0 -m "Release 0.1.0"
git push origin v0.1.0
```

The `Release` workflow validates the tag before running the complete CI workflow, then:

1. Checks the tag against Cargo and chart versions.
2. Packages and lints the chart with its real image repository.
3. Builds amd64/arm64 images on separate native runners, with SBOM and provenance, then publishes
   their combined multi-platform tags. Each architecture has its own build cache.
4. Pushes the OCI chart to GHCR and pulls it back to compare package bytes.
5. Creates a GitHub release containing the chart package and registry references.

Stable images receive `VERSION`, `vVERSION`, and `latest` tags. Prereleases such as `0.2.0-rc.1`
receive version tags and a prerelease GitHub release; they do not move `latest`. Use versions
without SemVer build metadata. Release tags must match package/chart versions exactly.

PRs and pushes to `main`/`master` run format, Clippy, offline Rust tests, OpenAPI consistency,
frontend build, chart checks, and native amd64/arm64 container smoke checks. PRs do not publish packages.
The smoke check starts the image as UID 1000 with a read-only root filesystem and verifies both
the health endpoint and UI with indexing disabled.

The workflow uses the repository's `GITHUB_TOKEN` with `packages: write` in image publishing jobs
and `contents: write` only when creating the release; no separate registry password is required. Initial repository setup
must permit GitHub Actions. The chart package and container package may initially be private;
set both packages to public in GitHub Packages for anonymous installs, or use the credentials
described in the [chart README](../charts/bookjev/README.md#private-ghcr-packages).
If a package already exists, grant this repository access under its package Actions settings.
See [GitHub's GHCR documentation](https://docs.github.com/en/packages/working-with-a-github-packages-registry/working-with-the-container-registry).
Native ARM64 builds use `ubuntu-24.04-arm`, supported for public and private repositories;
see [GitHub's runner reference](https://docs.github.com/en/actions/reference/runners/github-hosted-runners).

Registry publication is not atomic across artifacts: if a later step fails, an earlier artifact
can already exist. Rerun the failed workflow after resolving the cause; inspect any existing
release before retrying its creation. Keep release tags immutable once published.

## Operations after deployment

Use `helm upgrade --install` with the new chart version to update Kubernetes. Its `Recreate`
strategy stops the old writer before starting the new one. The data and library PVCs persist.
Check `GET /api/health`, `GET /api/index/status`, and pod logs after upgrading. The full first
import can take around 80 minutes based on an observed production run; health probes check the
server independently of import completion.
