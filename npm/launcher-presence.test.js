"use strict";

const test = require("node:test");
const assert = require("node:assert/strict");
const { spawnSync } = require("node:child_process");
const { chmodSync, copyFileSync, existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } = require("node:fs");
const { tmpdir } = require("node:os");
const { join } = require("node:path");

const LAUNCHER = join(__dirname, "bin", "hanzo.js");

function runWithPathEntry(t, makeEntry, { expectInstall }) {
  const root = mkdtempSync(join(tmpdir(), "hanzo-bin-check-"));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  const bin = join(root, "bin");
  const shadowPath = join(root, "shadow-path");
  const marker = join(root, "install-request.txt");
  mkdirSync(bin);
  mkdirSync(shadowPath);
  copyFileSync(LAUNCHER, join(bin, "hanzo.js"));
  writeFileSync(join(bin, "hanzo"), "#!/bin/sh\nexit 0\n");
  chmodSync(join(bin, "hanzo"), 0o755);
  makeEntry(join(shadowPath, "hanzod"));
  writeFileSync(
    join(root, "install.js"),
    'require("node:fs").writeFileSync(process.env.HANZO_INSTALL_MARKER, process.argv.slice(2).join(","))\n'
  );
  // Isolate PATH so a host-installed `hanzod` on the developer/CI machine
  // cannot mask the test fixture in shadowPath.
  const env = { ...process.env, HANZO_INSTALL_MARKER: marker };
  for (const key of Object.keys(env)) {
    if (key.toLowerCase() === "path") delete env[key];
  }
  env.PATH = shadowPath;
  const result = spawnSync(process.execPath, [join(bin, "hanzo.js"), "chain", "status"], {
    env,
    encoding: "utf8",
  });
  assert.equal(result.error, undefined);
  assert.equal(result.status, 0, result.stderr);
  assert.equal(
    existsSync(marker),
    expectInstall,
    expectInstall
      ? "missing or non-executable companions must trigger lazy install"
      : "an executable native companion on PATH must not trigger lazy install"
  );
  if (expectInstall) {
    assert.equal(readFileSync(marker, "utf8"), "node");
  }
}

test("a non-executable PATH file does not satisfy a native companion", { skip: process.platform === "win32" }, (t) => {
  runWithPathEntry(t, (p) => {
    writeFileSync(p, "not an executable");
    chmodSync(p, 0o644);
  }, { expectInstall: true });
});

test("a PATH directory does not satisfy a native companion", { skip: process.platform === "win32" }, (t) => {
  runWithPathEntry(t, (p) => mkdirSync(p), { expectInstall: true });
});

test("an executable regular file on PATH satisfies a native companion", { skip: process.platform === "win32" }, (t) => {
  runWithPathEntry(t, (p) => {
    writeFileSync(p, "#!/bin/sh\nexit 0\n");
    chmodSync(p, 0o755);
  }, { expectInstall: false });
});
