"use strict";

const test = require("node:test");
const assert = require("node:assert/strict");
const { spawnSync } = require("node:child_process");
const { chmodSync, copyFileSync, mkdirSync, mkdtempSync, rmSync, writeFileSync } = require("node:fs");
const { constants: osConstants, tmpdir } = require("node:os");
const { join } = require("node:path");

const LAUNCHER = join(__dirname, "bin", "hanzo.js");

function runFakeNative(t, script) {
  const root = mkdtempSync(join(tmpdir(), "hanzo-launcher-signal-"));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  const binDir = join(root, "bin");
  mkdirSync(binDir);
  copyFileSync(LAUNCHER, join(binDir, "hanzo.js"));
  const native = join(binDir, "hanzo");
  writeFileSync(native, `#!/bin/sh\n${script}\n`);
  chmodSync(native, 0o755);
  return spawnSync(process.execPath, [join(binDir, "hanzo.js")], {
    env: { ...process.env, HANZO_NO_LAZY_INSTALL: "1" },
    encoding: "utf8",
    timeout: 5_000,
  });
}

test("launcher preserves native exit codes", { skip: process.platform === "win32" }, (t) => {
  const result = runFakeNative(t, "exit 17");
  assert.equal(result.error, undefined);
  assert.equal(result.status, 17);
  assert.equal(result.signal, null);
});

test("launcher propagates native termination signals", { skip: process.platform === "win32" }, (t) => {
  const result = runFakeNative(t, "kill -TERM $$");
  assert.equal(result.error, undefined);
  assert.equal(result.status, null);
  assert.equal(result.signal, "SIGTERM");
});


test("launcher never starts the Node inspector for a native SIGUSR1", { skip: process.platform === "win32" }, (t) => {
  const result = runFakeNative(t, "kill -USR1 $$");
  assert.equal(result.error, undefined, result.stderr);
  assert.equal(result.signal, null);
  assert.equal(result.status, 128 + osConstants.signals.SIGUSR1);
  assert.doesNotMatch(result.stderr ?? "", /Debugger listening|Debugger attached/i);
});
