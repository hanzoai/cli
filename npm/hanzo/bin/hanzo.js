#!/usr/bin/env node
// Hand the real binary the argv it was given and get out of the way.
//
// ONE HOP, NOT TWO. Loading `@hanzo/cli/bin/hanzo.js` in-process via `require`
// runs the launcher inside THIS Node process (no extra Node wrapper between the
// terminal and the native binary), while ensuring `cliDir/bin` is prepended to
// `PATH` (handling Windows `Path` case-insensitively) and companion binaries
// (`dev`, `hanzo-mcp`, `hanzo-engine`, `hanzod`, `hanzo-vm`, `zt`) install
// lazily on first use.
import { spawnSync } from "node:child_process";
import { existsSync } from "node:fs";
import { createRequire } from "node:module";
import { delimiter, dirname, join } from "node:path";

const require = createRequire(import.meta.url);

let cliDir;
try {
  cliDir = dirname(require.resolve("@hanzo/cli/package.json"));
} catch {
  console.error(
    "hanzo: @hanzo/cli is not installed. It is a dependency of this package, so\n" +
      "reinstall with `npm install -g hanzo` (or `npm install hanzo` in a project)."
  );
  process.exit(1);
}

const binDir = join(cliDir, "bin");
const pathKey =
  Object.keys(process.env).find((key) => key.toLowerCase() === "path") || "PATH";
const currentPath = process.env[pathKey] || "";
if (!currentPath.split(delimiter).includes(binDir)) {
  process.env[pathKey] = binDir + (currentPath ? delimiter + currentPath : "");
}

const cliLauncher = join(binDir, "hanzo.js");
if (existsSync(cliLauncher)) {
  require(cliLauncher);
} else {
  const bin = join(binDir, process.platform === "win32" ? "hanzo.exe" : "hanzo");
  if (!existsSync(bin)) {
    const installScript = join(cliDir, "install.js");
    if (existsSync(installScript)) {
      spawnSync(process.execPath, [installScript], { stdio: "inherit", env: process.env });
    }
  }
  if (!existsSync(bin)) {
    console.error(
      "hanzo: the binary is missing. @hanzo/cli's install script fetches it;\n" +
        "if that was skipped (--ignore-scripts) run `node " + join(cliDir, "install.js") + "`."
    );
    process.exit(1);
  }
  const { status, signal } = spawnSync(bin, process.argv.slice(2), {
    stdio: "inherit",
    env: process.env,
  });
  if (signal) process.kill(process.pid, signal);
  process.exit(status ?? 1);
}
