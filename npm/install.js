#!/usr/bin/env node
// Fetch the binary for THIS platform from the GitHub release for THIS version.
//
// Why a downloader rather than five optionalDependencies (which is what
// @hanzo/dev does): the release matrix already publishes
// `hanzo-<platform>.tar.gz` assets, and pointing at those means one artifact,
// built once, verified once. Five npm packages would be a second copy of the
// same bytes under a second set of version numbers, and they drift — that is
// how a `latest` ends up serving last week's binary for one platform only.
//
// The download is VERIFIED against the .sha256 the release publishes beside
// each asset. This script runs on every `npm install` and then that binary is
// executed, so it is a supply chain: an unverified download here is arbitrary
// code execution on every machine that installs us.
const { createWriteStream, mkdirSync, chmodSync, existsSync } = require("node:fs");
const { createHash } = require("node:crypto");
const { readFile, rm } = require("node:fs/promises");
const { join } = require("node:path");
const { execFileSync } = require("node:child_process");
const { pipeline } = require("node:stream/promises");

const VERSION = require("./package.json").version;
const REPO = "hanzoai/cli";

// The asset ids the release matrix actually emits (release-matrix.yml).
// windows-arm64 and linux-*-musl are deliberately absent: a name here that the
// matrix does not build is an install that fails at the 404 instead of at a
// sentence a human can read.
const TARGETS = {
  "darwin-arm64": "darwin-arm64",
  "darwin-x64": "darwin-amd64",
  "linux-arm64": "linux-arm64",
  "linux-x64": "linux-amd64",
  "win32-x64": "windows-amd64",
};

async function get(url) {
  const res = await fetch(url, { redirect: "follow" });
  if (!res.ok) throw new Error(`${res.status} ${res.statusText} for ${url}`);
  return res;
}

async function main() {
  const key = `${process.platform}-${process.arch}`;
  const target = TARGETS[key];
  if (!target) {
    // Not an error. A platform we do not build for should not fail an install
    // that may not even use this package; `hanzo` will say so if it is run.
    console.warn(`@hanzo/cli: no prebuilt binary for ${key}; skipping download.`);
    return;
  }

  const base = `https://github.com/${REPO}/releases/download/v${VERSION}`;
  const asset = `hanzo-${target}.tar.gz`;
  const tmp = join(__dirname, asset);
  const dest = join(__dirname, "bin");

  const want = (await (await get(`${base}/${asset}.sha256`)).text()).trim().split(/\s+/)[0];

  const res = await get(`${base}/${asset}`);
  const hash = createHash("sha256");
  const file = createWriteStream(tmp);
  await pipeline(res.body, async function* (src) {
    for await (const chunk of src) { hash.update(chunk); yield chunk; }
  }, file);

  const got = hash.digest("hex");
  if (got !== want) {
    await rm(tmp, { force: true });
    throw new Error(`checksum mismatch for ${asset}\n  expected ${want}\n  got      ${got}`);
  }

  mkdirSync(dest, { recursive: true });
  execFileSync("tar", ["-xzf", tmp, "-C", dest]);
  await rm(tmp, { force: true });

  const bin = join(dest, process.platform === "win32" ? "hanzo.exe" : "hanzo");
  if (existsSync(bin) && process.platform !== "win32") chmodSync(bin, 0o755);
  console.log(`@hanzo/cli: installed hanzo ${VERSION} (${target})`);
}

main().catch((err) => {
  console.error(`@hanzo/cli: ${err.message}`);
  process.exit(1);
});
