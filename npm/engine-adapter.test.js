"use strict";

const test = require("node:test");
const assert = require("node:assert/strict");
const { spawnSync } = require("node:child_process");
const { chmodSync, mkdirSync, mkdtempSync, rmSync, writeFileSync } = require("node:fs");
const { tmpdir } = require("node:os");
const { join } = require("node:path");
const { ENGINE_ADAPTER } = require("./install.js");

function run(t, args) {
  const root = mkdtempSync(join(tmpdir(), "hanzo-engine-args-"));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  const wrapper = join(root, "hanzo-engine");
  writeFileSync(wrapper, ENGINE_ADAPTER, { mode: 0o755 });
  writeFileSync(join(root, "hanzoai"), "#!/bin/sh\nprintf '%s\\n' \"$@\"\n");
  chmodSync(join(root, "hanzoai"), 0o755);
  return spawnSync(wrapper, args, { encoding: "utf8", timeout: 1000, maxBuffer: 65536 });
}

test("adapter rejects missing model/port/overlay values without looping", { skip: process.platform === "win32" }, (t) => {
  for (const args of [["serve", "-m"], ["serve", "--model"], ["serve", "-m", "demo", "-p"], ["serve", "-m", "demo", "--overlay"]]) {
    const result = run(t, args);
    assert.equal(result.error, undefined, `should return an argument error, not hang: ${args}`);
    assert.equal(result.status, 2, `should reject ${args}`);
  }
});

test("adapter rejects an option in place of a required value", { skip: process.platform === "win32" }, (t) => {
  for (const args of [
    ["serve", "-m", "-p", "5544"],
    ["serve", "--model", "--verbose"],
    ["serve", "-m", "demo", "-p", "--overlay", "some-file"],
    ["serve", "-m", "demo", "--overlay", "--verbose"],
  ]) {
    const result = run(t, args);
    assert.equal(result.error, undefined, `must return an argument error: ${args}`);
    assert.equal(result.status, 2, `another option cannot be used as the value for ${args}`);
    assert.match(result.stderr, /requires a value/);
  }
});

test("adapter preserves a valid model and explicit port", { skip: process.platform === "win32" }, (t) => {
  const result = run(t, ["serve", "-m", "example-model", "-p", "5544"]);
  assert.equal(result.error, undefined);
  assert.equal(result.status, 0, result.stderr);
  assert.equal(result.stdout, "--port\n5544\nrun\n-m\nexample-model\n");
});
