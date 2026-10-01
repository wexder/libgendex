#!/usr/bin/env node
import { execFileSync, spawnSync } from "node:child_process";
import { readFileSync, writeFileSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

process.chdir(resolve(dirname(fileURLToPath(import.meta.url)), ".."));

const usage = "Usage: scripts/release.mjs [patch|minor|major|VERSION] [--remote origin] [--dry-run]";
const args = process.argv.slice(2);
let requested = "patch", remote = "origin", dryRun = false, hasVersion = false;
for (let i = 0; i < args.length; i++) {
  if (args[i] === "--help" || args[i] === "-h") {
    console.log(usage);
    process.exit(0);
  } else if (args[i] === "--dry-run") {
    dryRun = true;
  } else if (args[i] === "--remote") {
    remote = args[++i];
  } else if (!hasVersion && !args[i].startsWith("-")) {
    requested = args[i];
    hasVersion = true;
  } else {
    throw new Error(usage);
  }
}
if (!remote || !/^[A-Za-z0-9][A-Za-z0-9._-]*$/.test(remote)) throw new Error("Invalid Git remote name");

const semver = /^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)(?:-([0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*))?$/;
function parseVersion(version) {
  const match = semver.exec(version);
  if (!match || version.length > 100) throw new Error(`Invalid release version: ${version}`);
  const pre = match[4]?.split(".") || [];
  if (pre.some(part => /^0\d+$/.test(part))) throw new Error("Numeric prerelease identifiers cannot have leading zeroes");
  return { core: match.slice(1, 4).map(BigInt), pre };
}
function compareVersions(a, b) {
  for (let i = 0; i < 3; i++) {
    if (a.core[i] !== b.core[i]) return a.core[i] > b.core[i] ? 1 : -1;
  }
  if (!a.pre.length || !b.pre.length) return Number(!a.pre.length) - Number(!b.pre.length);
  for (let i = 0; i < Math.max(a.pre.length, b.pre.length); i++) {
    if (a.pre[i] === undefined) return -1;
    if (b.pre[i] === undefined) return 1;
    if (a.pre[i] === b.pre[i]) continue;
    const aNumeric = /^\d+$/.test(a.pre[i]), bNumeric = /^\d+$/.test(b.pre[i]);
    if (aNumeric && bNumeric) return BigInt(a.pre[i]) > BigInt(b.pre[i]) ? 1 : -1;
    if (aNumeric !== bNumeric) return aNumeric ? -1 : 1;
    return a.pre[i] > b.pre[i] ? 1 : -1;
  }
  return 0;
}
function git(...args) {
  return execFileSync("git", args, { encoding: "utf8", stdio: ["ignore", "pipe", "inherit"] }).trim();
}
function run(...args) {
  execFileSync("git", args, { stdio: "inherit" });
}

let releaseCommit = false, releaseTag = false, tag, branch;
try {
  const files = new Map();
  const read = path => {
    const text = readFileSync(path, "utf8");
    files.set(path, text);
    return text;
  };
  const cargo = read("Cargo.toml");
  const current = cargo.match(/^version\s*=\s*"([^"]+)"/m)?.[1];
  const packageName = cargo.match(/^name\s*=\s*"([^"]+)"/m)?.[1];
  const old = parseVersion(current);
  let version = requested;
  if (["patch", "minor", "major"].includes(requested)) {
    let [major, minor, patch] = old.core;
    if (requested === "major") {
      if (!old.pre.length || minor !== 0n || patch !== 0n) major++;
      minor = patch = 0n;
    } else if (requested === "minor") {
      if (!old.pre.length || patch !== 0n) minor++;
      patch = 0n;
    } else if (!old.pre.length) patch++;
    version = `${major}.${minor}.${patch}`;
  }
  if (compareVersions(parseVersion(version), old) <= 0) throw new Error("The release version must increase");
  tag = `v${version}`;
  const chartPath = "charts/libgendex/Chart.yaml";
  const chart = read(chartPath);
  if (chart.match(/^version:\s*(\S+)/m)?.[1] !== current ||
      chart.match(/^appVersion:\s*"([^"]+)"/m)?.[1] !== current) {
    throw new Error("Cargo and chart versions must match before releasing");
  }
  const lock = read("Cargo.lock");
  const lockEntry = new RegExp(`(\\[\\[package\\]\\]\\r?\\nname = "${packageName}"\\r?\\nversion = ")([^"]+)(")`);
  if (lock.match(lockEntry)?.[2] !== current) throw new Error("Cargo.lock root version does not match Cargo.toml");
  const spec = JSON.parse(read("openapi.json"));
  if (spec.info.version !== current) throw new Error("OpenAPI version does not match Cargo.toml; regenerate it first");

  files.set("Cargo.toml", cargo.replace(/^version\s*=\s*"[^"]+"/m, `version = "${version}"`));
  files.set(chartPath, chart.replace(/^version:\s*\S+/m, `version: ${version}`).replace(/^appVersion:\s*"[^"]+"/m, `appVersion: "${version}"`));
  files.set("Cargo.lock", lock.replace(lockEntry, (_, start, oldVersion, end) => `${start}${version}${end}`));
  spec.info.version = version;
  files.set("openapi.json", `${JSON.stringify(spec, null, 2)}\n`);
  const escaped = current.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
  const examples = new RegExp(`(?<![0-9A-Za-z.+])(v?)${escaped}(?![0-9A-Za-z.+-])`, "g");
  for (const path of ["README.md", "charts/libgendex/README.md", "docker-compose.yml", "Dockerfile", "docs/releasing.md"]) {
    files.set(path, read(path).replace(examples, (_, prefix) => `${prefix}${version}`));
  }

  branch = git("symbolic-ref", "--quiet", "--short", "HEAD");
  console.log(`Release ${current} → ${version} (${tag}) on ${remote}/${branch}`);
  console.log(`Update: ${[...files.keys()].join(", ")}`);
  if (dryRun) {
    console.log("Preview only. No files, commits, tags, or remote refs changed.");
    process.exit(0);
  }
  if (git("status", "--porcelain")) throw new Error("Commit or stash existing changes before releasing");
  git("rev-parse", "--verify", "HEAD");
  git("remote", "get-url", "--push", remote);
  git("var", "GIT_AUTHOR_IDENT");
  git("var", "GIT_COMMITTER_IDENT");
  if (spawnSync("git", ["show-ref", "--verify", "--quiet", `refs/tags/${tag}`]).status !== 1) {
    throw new Error(`Local tag ${tag} already exists or could not be checked`);
  }
  const remoteTag = spawnSync("git", ["ls-remote", "--exit-code", "--tags", remote, `refs/tags/${tag}`], { stdio: "inherit" });
  if (remoteTag.status !== 2) throw new Error(`Remote tag ${tag} already exists or the remote could not be checked`);
  run("fetch", "--no-tags", remote);
  const remoteBranch = `refs/remotes/${remote}/${branch}`;
  if (spawnSync("git", ["show-ref", "--verify", "--quiet", remoteBranch]).status === 0) {
    if (spawnSync("git", ["merge-base", "--is-ancestor", remoteBranch, "HEAD"]).status !== 0) {
      throw new Error(`Update your branch from ${remote}/${branch} before releasing; it is behind or has diverged`);
    }
  }

  for (const [path, contents] of files) writeFileSync(path, contents);
  run("add", "--", ...files.keys());
  run("commit", "-m", `Release ${version}`);
  releaseCommit = true;
  run("tag", "-a", tag, "-m", `Release ${version}`);
  releaseTag = true;
  run("push", "--atomic", remote, `HEAD:refs/heads/${branch}`, `refs/tags/${tag}`);
  console.log(`Pushed ${tag}. GitHub Actions will publish the container and Helm chart.`);
} catch (error) {
  console.error(error.message);
  if (releaseTag) {
    console.error(`The local release commit and ${tag} remain. Retry the atomic push after resolving the error:`);
    console.error(`git push --atomic ${remote} HEAD:refs/heads/${branch} refs/tags/${tag}`);
  } else if (releaseCommit) {
    console.error(`The release commit remains locally; create ${tag} on it and push the branch and tag.`);
  } else {
    console.error("No release was pushed. Any edited version files remain available for review.");
  }
  process.exitCode = 1;
}
