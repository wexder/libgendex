import { readFileSync, writeFileSync, appendFileSync, cpSync, mkdtempSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

// Keep version validation and OCI naming identical in CI and publishing.
const cargo = readFileSync("Cargo.toml", "utf8");
const chart = readFileSync("charts/bookjev/Chart.yaml", "utf8");
const version = cargo.match(/^version\s*=\s*"([^"]+)"/m)?.[1];
const chartVersion = chart.match(/^version:\s*(\S+)/m)?.[1];
const appVersion = chart.match(/^appVersion:\s*"([^"]+)"/m)?.[1];
const semver = /^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)(?:-([0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*))?$/;
if (!version || !semver.test(version) || version.length > 100) {
  throw new Error("Cargo package version must be SemVer without build metadata");
}
const prereleaseIdentifiers = semver.exec(version)[4]?.split(".") || [];
if (prereleaseIdentifiers.some((part) => /^0\d+$/.test(part))) {
  throw new Error("Numeric SemVer prerelease identifiers cannot have leading zeroes");
}
if (chartVersion !== version || appVersion !== version) {
  throw new Error("Cargo version, chart version, and chart appVersion must match");
}
if (process.argv.includes("--check")) {
  console.log(`Release versions agree: ${version}`);
  process.exit(0);
}
if (process.env.GITHUB_REF_NAME !== `v${version}`) {
  throw new Error(`Release tag must be v${version}; bump Cargo.toml, Cargo.lock, and Chart.yaml together`);
}
const repo = process.env.GITHUB_REPOSITORY?.toLowerCase();
if (!repo || !/^[a-z0-9_.-]+\/[a-z0-9_.-]+$/.test(repo)) {
  throw new Error("GITHUB_REPOSITORY must be owner/repository");
}
const owner = repo.split("/")[0];
const image = `ghcr.io/${repo}`;
const chartRegistry = `oci://ghcr.io/${owner}/charts`;
const directory = mkdtempSync(join(process.env.RUNNER_TEMP || tmpdir(), "bookjev-release-"));
const chartDirectory = join(directory, "bookjev");
cpSync("charts/bookjev", chartDirectory, { recursive: true });
const values = readFileSync(join(chartDirectory, "values.yaml"), "utf8");
if (!/^  repository: \S+$/m.test(values)) {
  throw new Error("Chart image repository is missing");
}
writeFileSync(join(chartDirectory, "values.yaml"), values.replace(/^  repository: \S+$/m, `  repository: ${image}`));
const outputs = {
  version, image, chart_registry: chartRegistry, chart_dir: chartDirectory,
  prerelease: String(version.includes("-")),
};
if (!process.env.GITHUB_OUTPUT) throw new Error("GITHUB_OUTPUT is required when packaging a release");
appendFileSync(process.env.GITHUB_OUTPUT, Object.entries(outputs).map(([key, value]) => `${key}=${value}\n`).join(""));
console.log(JSON.stringify(outputs, null, 2));
