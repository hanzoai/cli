"use strict";

const test = require("node:test");
const assert = require("node:assert/strict");
const { spawnSync } = require("node:child_process");
const { chmodSync, copyFileSync, existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } = require("node:fs");
const { tmpdir } = require("node:os");
const { delimiter, join } = require("node:path");

const LAUNCHER = join(__dirname, "bin", "hanzo.js");

function runWithPathEntry(t, makeEntry) {
  const root = mkdtempSync(join(tmpdir(), "hanzo-bin-check-"));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  const bin = join(root, "bin");
  const path = join(root, "shadow-path");
  const marker = join(root, "install-request.txt");
  mkdirSync(bin);
  mkdirSync(path);
  copyFileSync(LAUNCHER, join(bin, "hanzo.js"));
  writeFileSync(join(bin, "hanzo"), "#!/bin/sh\nexit 0\n");
  chmodSync(join(bin, "hanzo"), 0o755);
  makeEntry(join(path, "hanzod"));
  writeFileSync(
    join(root, "install.js"),
    'require("node:fs").writeFileSync(process.env.HANZO_INSTALL_MARKER, process.argv.slice(2).join(","))\n'
  );
  const result = spawnSync(process.execPath, [join(bin, "hanzo.js"), "chain", "status"], {
    env: {
      ...process.env,
      PATH: `${path}${delimiter}${process.env.PATH || ""}`,
      HANZO_INSTALL_MARKER: marker,
    },
    encoding: "utf8",
  });
  assert.equal(result.error, undefined);
  assert.equal(result.status, 0, result.stderr);
  assert.equal(existsSync(marker), true, "missing companions must trigger lazy install");
  assert.equal(readFileSync(marker, "utf8"), "node");
}

test("a non-executable PATH file does not satisfy a native companion", { skip: process.platform === "win32" }, (t) => {
  runWithPathEntry(t, (path) => {
    writeFileSync(path, "not an executable");
    chmodSync(path, 0o644);
  });
});

test("a PATH directory does not satisfy a native companion", { skip: process.platform === "win32" }, (t) => {
  runWithPathEntry(t, (path) => mkdirSync(path));
});
