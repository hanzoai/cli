#!/usr/bin/env node
// Hand the real binary the argv it was given and get out of the way.
//
// Prepends this `bin/` directory to PATH (handling Windows `Path`
// case-insensitively) so sibling binaries (`dev`, `hanzo-mcp`, `hanzo-engine`,
// `hanzo-vm`, `zt`, `hanzod`) are always discoverable, and lazily fetches any
// missing companion binary on first invocation via `../install.js`.
const { spawnSync } = require("node:child_process");
const { existsSync } = require("node:fs");
const { basename, delimiter, join } = require("node:path");

const ext = process.platform === "win32" ? ".exe" : "";
const installScript = join(__dirname, "..", "install.js");

const env = { ...process.env };
const pathKey =
  Object.keys(env).find((key) => key.toLowerCase() === "path") || "PATH";

env[pathKey] = __dirname + delimiter + (env[pathKey] || "");

const TOOL_BINS = {
  hanzo: "hanzo",
  dev: "dev",
  mcp: "hanzo-mcp",
  gen: "gen",
  zt: "zt",
  engine: "hanzo-engine",
  vm: "hanzo-vm",
  node: "hanzod",
};

function hasTool(tool) {
  const binName = (TOOL_BINS[tool] || tool) + ext;
  for (const dir of (env[pathKey] || "").split(delimiter)) {
    if (dir && existsSync(join(dir, binName))) return true;
  }
  return false;
}

function isHelpOrVersion(args) {
  return args.some((a) => a === "--help" || a === "-h" || a === "--version" || a === "-V" || a === "version" || a === "help");
}

const rawName = basename(process.argv[1] || "hanzo").replace(/\.(js|cmd|ps1|exe)$/i, "");
let argv = process.argv.slice(2);
let targetTool = "hanzo";
const required = [];

if (rawName === "hanzo-dev" || rawName === "dev") {
  targetTool = "dev";
  required.push("dev", "mcp");
} else if (rawName === "hanzo-mcp" || rawName === "mcp") {
  targetTool = "mcp";
  required.push("mcp");
} else if (rawName === "hanzo-engine") {
  targetTool = "engine";
  required.push("engine");
} else if (rawName === "hanzo-node" || rawName === "hanzod") {
  targetTool = "hanzo";
  argv = ["chain", ...(argv.length ? argv : ["up"])];
  required.push("hanzo", "node");
} else {
  required.push("hanzo");
  const sub = argv[0];
  if (sub === "mcp") {
    targetTool = "mcp";
    argv = argv.slice(1);
    required.length = 0;
    required.push("mcp");
  } else if (sub === "node" && ["up", "join", "status", "stop"].includes(argv[1])) {
    argv = ["chain", ...argv.slice(1)];
    if (!isHelpOrVersion(argv)) required.push("node");
  } else if (sub === "engine" && argv[1] === "run") {
    targetTool = "engine";
    argv = argv.slice(1);
    required.length = 0;
    required.push("engine");
  } else if (!isHelpOrVersion(argv)) {
    if (!sub || sub.startsWith("-") || sub === "dev" || sub === "code" || sub === "desktop") {
      required.push("dev", "mcp");
    } else if (sub === "__mcp") {
      required.push("mcp");
    } else if (sub === "engine" && argv[1] === "serve") {
      required.push("engine");
    } else if (sub === "link" || sub === "zt") {
      required.push("zt");
    } else if (sub === "vm" || sub === "up" || sub === "build") {
      required.push("vm");
    } else if (sub === "chain") {
      required.push("node");
    }
  }
}

const missing = required.filter((t) => !hasTool(t));
if (missing.length > 0 && existsSync(installScript) && env.HANZO_NO_LAZY_INSTALL !== "1") {
  spawnSync(process.execPath, [installScript, ...missing], {
    stdio: "inherit",
    env,
  });
}

const binName = (TOOL_BINS[targetTool] || "hanzo") + ext;
const bin = join(__dirname, binName);
if (!existsSync(bin)) {
  console.error(
    `hanzo: the ${TOOL_BINS[targetTool] || "hanzo"} binary is missing. Run \`node ${installScript} ${targetTool}\` to fetch it.`
  );
  process.exit(1);
}

const r = spawnSync(bin, argv, {
  stdio: "inherit",
  env,
});
if (r.error) { console.error("hanzo:", r.error.message); process.exit(1); }
process.exit(r.status ?? 0);
