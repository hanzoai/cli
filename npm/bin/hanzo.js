#!/usr/bin/env node
// Hand the real binary the argv it was given and get out of the way.
//
// exec-and-replace, not spawn-and-wait: a wrapper that survives its child owns
// that child's signals, and `hanzo` is interactive — Ctrl-C has to reach the
// program, not a Node process sitting in front of it. The exit code is the
// binary's for the same reason.
const { spawnSync } = require("node:child_process");
const { existsSync } = require("node:fs");
const { join } = require("node:path");

const bin = join(__dirname, process.platform === "win32" ? "hanzo.exe" : "hanzo");
if (!existsSync(bin)) {
  console.error(
    "hanzo: the binary is missing. `npm install` runs install.js to fetch it;\n" +
      "if that was skipped (--ignore-scripts) run `node " + join(__dirname, "..", "install.js") + "`."
  );
  process.exit(1);
}
const r = spawnSync(bin, process.argv.slice(2), { stdio: "inherit" });
if (r.error) { console.error("hanzo:", r.error.message); process.exit(1); }
process.exit(r.status ?? 0);
