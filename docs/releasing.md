# Releases

Repository: [wexder/libgendex](https://github.com/wexder/libgendex).

| Artifact | Registry reference |
| --- | --- |
| Container | `ghcr.io/wexder/libgendex:VERSION` |
| Helm chart | `oci://ghcr.io/wexder/charts/libgendex` with `--version VERSION` |

The workflows derive the owner/repository from GitHub and lowercase registry names. The chart
name is `libgendex`. Published chart defaults point at the container from the same release.

## Bump and publish

Commit your changes, then run from a branch with push access to `origin`:

```sh
./scripts/release.mjs patch --dry-run
./scripts/release.mjs patch
# Or: make release BUMP=minor
# Or: ./scripts/release.mjs 0.2.0-rc.1
```

The script defaults to a patch bump, also accepts `minor`, `major`, or an explicit increasing
SemVer, and supports `--remote NAME`. It requires Node.js and Git; no Cargo build is needed.
For a prerelease, a patch bump promotes its existing core version to stable.

It updates Cargo.toml, the root Cargo.lock entry, chart version/appVersion, OpenAPI's application
version, Docker/Compose defaults, and README examples. Only the root Rust package is bumped;
the MyISAM crate keeps its independent version. If API routes changed, regenerate the spec with
`make openapi` and commit it before releasing. CI checks the entire generated spec.

Before changing files it checks the clean checkout, version consistency, Git identity, remote
access, unused tag, and whether the branch is behind its remote. It creates a `Release VERSION`
commit and an annotated `vVERSION` tag, then pushes the current branch and tag in one atomic
push. Any other unpushed commits on the branch are included. `--dry-run` only prints the plan;
it does not require a clean checkout or contact the remote. The script does not run tests.

If the push fails, the local commit and tag remain. Resolve the failure and rerun the printed
`git push --atomic` command. Do not rerun a version bump to retry that same release. Branch
protection can prevent a direct release commit push; use your repository's merge process in
that case, then tag the merged release commit.

Container and CI builds use stable Rust 1.99.0. The current lockfile requires Rust 1.97.1 or later.
The CI workflow overrides developer-only nightly flags and linker/wrapper settings. Docker builds
ignore `.cargo/config.toml`. Default release images use portable x86-64 code on amd64 and native
arm64 code on arm64. The runtime contains the Rust binary and UI with no MariaDB or unrar.

## Release workflow

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
frontend build, chart checks, and native amd64/arm64 container builds. PRs do not publish packages.

The workflow uses the repository's `GITHUB_TOKEN` with `packages: write` in image publishing jobs
and `contents: write` only when creating the release; no separate registry password is required. Initial repository setup
must permit GitHub Actions. The chart package and container package may initially be private;
set both packages to public in GitHub Packages for anonymous installs, or use the credentials
described in the [chart README](../charts/libgendex/README.md#private-ghcr-packages).
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
