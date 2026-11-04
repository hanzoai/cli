#!/usr/bin/env node
// Fetch the binary for THIS platform from the GitHub release for THIS version,
// and lazily fetch companion stack binaries (`dev`, `hanzo-mcp`, `gen`, `zt`,
// `hanzo-engine`, `hanzo-vm`, `hanzod`) on demand when invoked with tool names.
//
// Why a downloader rather than five optionalDependencies (which is what
// @hanzo/dev does): the release matrix already publishes
// `hanzo-<platform>.tar.gz` assets, and pointing at those means one artifact,
// built once, verified once. Five npm packages would be a second copy of the
// same bytes under a second set of version numbers, and they drift — that is
// how a `latest` ends up serving last week's binary for one platform only.
//
// By default (`postinstall` with no args), ONLY `hanzo` (+ `hanzo-cli` alias)
// is downloaded so `npm install -g hanzo` stays fast and lightweight. Companion
// binaries (`dev`, `mcp`, `engine`, `node`, `zt`, `vm`, `gen`) are fetched
// lazily on first use by `bin/hanzo.js` (or eagerly with `HANZO_NPM_EAGER=1` /
// `node install.js --all`).
const { createWriteStream, mkdirSync, chmodSync, copyFileSync, existsSync, renameSync, symlinkSync, unlinkSync } = require("node:fs");
const { createHash } = require("node:crypto");
const { rm } = require("node:fs/promises");
const { join } = require("node:path");
const { execFileSync } = require("node:child_process");
const { pipeline } = require("node:stream/promises");

const VERSION = require("./package.json").version;
const REPO = "hanzoai/cli";

const TOOLS = {
  hanzo:  { repo: "hanzoai/cli",    bin: "hanzo",        alias: "hanzo-cli", versioned: true },
  mcp:    { repo: "hanzoai/mcp",    bin: "hanzo-mcp",    alias: "mcp" },
  dev:    { repo: "hanzoai/dev",    bin: "dev",          alias: "hanzo-dev" },
  gen:    { repo: "hanzoai/gen",    bin: "gen",          alias: "hanzo-gen" },
  zt:     { repo: "hanzozt/zt",     bin: "zt",           alias: "hanzo-zet" },
  engine: { repo: "hanzoai/engine", bin: "hanzo-engine", alias: "hanzoai",   kind: "engine" },
  vm:     { repo: "hanzoai/vm",     bin: "hanzo-vm",     alias: "",          kind: "vm" },
  node:   { repo: "hanzoai/node",   bin: "hanzod",       alias: "hanzo-node" },
};

const TARGETS = {
  "darwin-arm64": "darwin-arm64",
  "darwin-x64": "darwin-amd64",
  "linux-arm64": "linux-arm64",
  "linux-x64": "linux-amd64",
  "win32-x64": "windows-amd64",
};

const ENGINE_TARGETS = {
  "darwin-arm64": "macos-arm64",
  "darwin-x64": "macos-amd64",
  "linux-arm64": "linux-arm64",
  "linux-x64": "linux-amd64",
};

const VM_TARGETS = {
  "darwin-arm64": "darwin-aarch64",
  "linux-arm64": "linux-aarch64",
  "linux-x64": "linux-x86_64",
};

async function get(url) {
  const res = await fetch(url, { redirect: "follow" });
  if (!res.ok) throw new Error(`${res.status} ${res.statusText} for ${url}`);
  return res;
}

function linkAlias(dest, binName, aliasName, ext) {
  if (!aliasName) return;
  const src = join(dest, `${binName}${ext}`);
  const dst = join(dest, `${aliasName}${ext}`);
  if (!existsSync(src)) return;
  try {
    if (existsSync(dst)) unlinkSync(dst);
    symlinkSync(`${binName}${ext}`, dst);
  } catch {
    copyFileSync(src, dst);
    if (process.platform !== "win32") chmodSync(dst, 0o755);
  }
}

async function downloadAndExtract({ assetUrl, sha256Url, assetName, dest }) {
  const tmp = join(__dirname, `${process.pid}-${assetName}`);
  let want = null;
  if (sha256Url) {
    want = (await (await get(sha256Url)).text()).trim().split(/\s+/)[0];
  }

  const res = await get(assetUrl);
  const hash = createHash("sha256");
  const file = createWriteStream(tmp);
  await pipeline(res.body, async function* (src) {
    for await (const chunk of src) { hash.update(chunk); yield chunk; }
  }, file);

  const got = hash.digest("hex");
  if (want && got !== want) {
    await rm(tmp, { force: true });
    throw new Error(`checksum mismatch for ${assetName}\n  expected ${want}\n  got      ${got}`);
  }

  mkdirSync(dest, { recursive: true });
  execFileSync("tar", ["-xzf", tmp, "-C", dest]);
  await rm(tmp, { force: true });
}

async function fetchStandardTool({ base, bin, alias, target, dest, ext }) {
  const asset = `${bin}-${target}.tar.gz`;
  await downloadAndExtract({
    assetUrl: `${base}/${asset}`,
    sha256Url: `${base}/${asset}.sha256`,
    assetName: asset,
    dest,
  });
  const out = join(dest, `${bin}${ext}`);
  if (existsSync(out) && process.platform !== "win32") chmodSync(out, 0o755);
  linkAlias(dest, bin, alias, ext);
}

async function fetchEngine({ key, dest, ext }) {
  const engineTarget = ENGINE_TARGETS[key];
  if (!engineTarget) {
    throw new Error(`no prebuilt hanzo-engine binary for ${key}; install via https://github.com/hanzoai/engine`);
  }
  const asset = `hanzoai-${engineTarget}.tar.gz`;
  let tag = "v1.7.92";
  let sha256Url = null;
  try {
    const res = await fetch("https://api.github.com/repos/hanzoai/engine/releases?per_page=15", {
      headers: { "User-Agent": "@hanzo/cli" },
    });
    if (res.ok) {
      const releases = await res.json();
      const match = releases.find(
        (r) => Array.isArray(r.assets) && r.assets.some((a) => a.name === asset)
      );
      if (match) {
        tag = match.tag_name;
        if (match.assets.some((a) => a.name === `${asset}.sha256`)) {
          sha256Url = `https://github.com/hanzoai/engine/releases/download/${tag}/${asset}.sha256`;
        }
      }
    }
  } catch {
    // Fall back to known release tag with prebuilt assets
  }

  const base = `https://github.com/hanzoai/engine/releases/download/${tag}`;
  await downloadAndExtract({
    assetUrl: `${base}/${asset}`,
    sha256Url,
    assetName: asset,
    dest,
  });

  const extracted = join(dest, `hanzoai${ext}`);
  const out = join(dest, `hanzo-engine${ext}`);
  if (existsSync(extracted)) {
    if (existsSync(out)) unlinkSync(out);
    renameSync(extracted, out);
  }
  if (existsSync(out) && process.platform !== "win32") chmodSync(out, 0o755);
  linkAlias(dest, "hanzo-engine", "hanzoai", ext);
}

async function fetchVm({ key, dest, ext }) {
  const vmTarget = VM_TARGETS[key];
  if (!vmTarget) {
    throw new Error(`no prebuilt hanzo-vm binary for ${key}`);
  }
  let tag = "v2.0.2";
  try {
    const res = await fetch("https://api.github.com/repos/hanzoai/vm/releases/latest", {
      headers: { "User-Agent": "@hanzo/cli" },
    });
    if (res.ok) {
      const rel = await res.json();
      if (rel && rel.tag_name) tag = rel.tag_name;
    }
  } catch {
    // Fall back to v2.0.2
  }
  const asset = `hanzo-vm-${tag}-${vmTarget}.tar.gz`;
  const base = `https://github.com/hanzoai/vm/releases/download/${tag}`;
  await downloadAndExtract({
    assetUrl: `${base}/${asset}`,
    sha256Url: `${base}/${asset}.sha256`,
    assetName: asset,
    dest,
  });
  const out = join(dest, `hanzo-vm${ext}`);
  if (existsSync(out) && process.platform !== "win32") chmodSync(out, 0o755);
}

async function installOne(name, { key, target, dest, ext, optional }) {
  const tool = TOOLS[name];
  if (!tool) return;
  try {
    if (tool.kind === "engine") {
      await fetchEngine({ key, dest, ext });
    } else if (tool.kind === "vm") {
      await fetchVm({ key, dest, ext });
    } else {
      const primaryBase = tool.versioned
        ? `https://github.com/${tool.repo}/releases/download/v${VERSION}`
        : `https://github.com/${tool.repo}/releases/latest/download`;
      try {
        await fetchStandardTool({
          base: primaryBase,
          bin: tool.bin,
          alias: tool.alias,
          target,
          dest,
          ext,
        });
      } catch (err) {
        if (tool.versioned && String(err.message || err).startsWith("404 ")) {
          await fetchStandardTool({
            base: `https://github.com/${tool.repo}/releases/latest/download`,
            bin: tool.bin,
            alias: tool.alias,
            target,
            dest,
            ext,
          });
        } else {
          throw err;
        }
      }
    }
    const label = tool.versioned ? `${tool.bin} ${VERSION}` : tool.bin;
    console.error(`@hanzo/cli: installed ${label} (${target})`);
  } catch (err) {
    if (!optional || String(err.message || err).includes("checksum mismatch")) throw err;
    console.warn(`@hanzo/cli: optional companion ${tool.bin} skipped (${err.message})`);
  }
}

async function main() {
  const key = `${process.platform}-${process.arch}`;
  const target = TARGETS[key];
  if (!target) {
    console.warn(`@hanzo/cli: no prebuilt binary for ${key}; skipping download.`);
    return;
  }

  const dest = join(__dirname, "bin");
  const ext = process.platform === "win32" ? ".exe" : "";
  const args = process.argv.slice(2);

  if (args.length > 0 && !args.includes("--all")) {
    for (const arg of args) {
      const name = arg.replace(/^hanzo-/, "");
      const toolKey = TOOLS[arg] ? arg : TOOLS[name] ? name : null;
      if (toolKey) {
        await installOne(toolKey, { key, target, dest, ext, optional: toolKey === "node" || toolKey === "gen" });
      }
    }
    return;
  }

  // Default `postinstall`: lightweight install of `hanzo` (+ `hanzo-cli` alias) only.
  // All companion binaries (`dev`, `hanzo-mcp`, `hanzo-engine`, `hanzo-vm`, `zt`, `gen`)
  // are lazily downloaded on first use by `bin/hanzo.js`.
  await installOne("hanzo", { key, target, dest, ext, optional: false });

  if (process.env.HANZO_NPM_EAGER === "1" || args.includes("--all")) {
    for (const name of ["mcp", "dev", "gen", "zt"]) {
      await installOne(name, { key, target, dest, ext, optional: true });
    }
  }
}

main().catch((err) => {
  console.error(`@hanzo/cli: ${err.message}`);
  process.exit(1);
});
