"use strict";

const test = require("node:test");
const assert = require("node:assert/strict");
const { execFileSync, spawnSync } = require("node:child_process");
const { createHash } = require("node:crypto");
const {
  chmodSync,
  copyFileSync,
  existsSync,
  mkdirSync,
  mkdtempSync,
  readFileSync,
  rmSync,
  writeFileSync,
} = require("node:fs");
const { tmpdir } = require("node:os");
const { delimiter, dirname, join } = require("node:path");
const { extractArchive, resolveTarExecutable } = require("./install.js");

const INSTALLER = join(__dirname, "install.js");
const PACKAGE = join(__dirname, "package.json");
const WINDOWS_SYSTEM_TAR = process.env.SystemRoot
  ? join(process.env.SystemRoot, "System32", "tar.exe")
  : null;
const WINDOWS_GIT_TAR = process.platform === "win32"
  ? join(process.env.ProgramFiles || "C:\\Program Files", "Git", "usr", "bin", "tar.exe")
  : null;

function setEnvironmentKey(env, name, value) {
  for (const key of Object.keys(env)) {
    if (key.toLowerCase() === name.toLowerCase()) delete env[key];
  }
  env[name] = value;
}

function createCase(t, { validArchive = true } = {}) {
  const root = mkdtempSync(join(tmpdir(), "hanzo issue 41 café "));
  t.after(() => rmSync(root, { recursive: true, force: true }));

  const packageDir = join(root, "package");
  const sourceDir = join(root, "fixture source");
  mkdirSync(packageDir);
  mkdirSync(sourceDir);
  copyFileSync(INSTALLER, join(packageDir, "install.js"));
  copyFileSync(PACKAGE, join(packageDir, "package.json"));

  const target = process.platform === "win32" ? "windows-amd64" : "linux-amd64";
  const binary = process.platform === "win32" ? "hanzo.exe" : "hanzo";
  const assetName = `hanzo-${target}.tar.gz`;
  const archive = join(root, assetName);

  if (validArchive) {
    writeFileSync(join(sourceDir, binary), "local fixture only\n");
    const archiveTar = process.platform === "win32" ? WINDOWS_SYSTEM_TAR : "tar";
    execFileSync(archiveTar, ["-czf", archive, "-C", sourceDir, binary]);
  } else {
    writeFileSync(archive, "not a tar archive\n");
  }

  const sha256 = createHash("sha256").update(readFileSync(archive)).digest("hex");
  const preload = join(root, "local-fetch.js");
  writeFileSync(
    preload,
    [
      '"use strict";',
      'const { readFileSync } = require("node:fs");',
      'const { Readable } = require("node:stream");',
      "global.fetch = async (url) => {",
      "  if (String(url).endsWith(\".sha256\")) {",
      "    return {",
      "      ok: true,",
      "      text: async () => `${process.env.HANZO_TEST_SHA}  ${process.env.HANZO_TEST_ASSET}\\n`,",
      "    };",
      "  }",
      "  return { ok: true, body: Readable.from([readFileSync(process.env.HANZO_TEST_ARCHIVE)]) };",
      "};",
      "",
    ].join("\n")
  );

  return {
    root,
    packageDir,
    archive,
    assetName,
    sha256,
    binary,
    preload,
    installedBinary: join(packageDir, "bin", binary),
  };
}

function runInstaller(context, { expectedSha = context.sha256, pathPrefix, systemRoot } = {}) {
  const env = { ...process.env };
  setEnvironmentKey(env, "HANZO_TEST_ARCHIVE", context.archive);
  setEnvironmentKey(env, "HANZO_TEST_ASSET", context.assetName);
  setEnvironmentKey(env, "HANZO_TEST_SHA", expectedSha);

  if (pathPrefix) {
    const currentPath = Object.entries(process.env).find(
      ([key]) => key.toLowerCase() === "path"
    )?.[1] || "";
    setEnvironmentKey(env, "PATH", `${pathPrefix}${delimiter}${currentPath}`);
  }
  if (systemRoot !== undefined) setEnvironmentKey(env, "SystemRoot", systemRoot);

  return spawnSync(process.execPath, ["--require", context.preload, join(context.packageDir, "install.js")], {
    cwd: context.root,
    env,
    encoding: "utf8",
    timeout: 30_000,
  });
}

test(
  "Windows uses the system tar with a drive-letter, spaces, Unicode, and a hostile PATH tar",
  { skip: process.platform !== "win32" },
  (t) => {
    const context = createCase(t);
    assert.match(context.root, /^[A-Za-z]:\\/);
    assert.match(context.root, / /);
    assert.match(context.root, /café/);

    const hostileBin = join(context.root, "hostile PATH");
    mkdirSync(hostileBin);
    copyFileSync(process.execPath, join(hostileBin, "tar.exe"));

    const result = runInstaller(context, { pathPrefix: hostileBin });
    assert.equal(result.status, 0, result.stderr);
    assert.equal(readFileSync(context.installedBinary, "utf8"), "local fixture only\n");
  }
);

test(
  "Windows succeeds with Git GNU tar first on PATH",
  { skip: process.platform !== "win32" || !existsSync(WINDOWS_GIT_TAR) },
  (t) => {
    const context = createCase(t);
    const version = execFileSync(WINDOWS_GIT_TAR, ["--version"], { encoding: "utf8" });
    assert.match(version, /GNU tar/);

    const result = runInstaller(context, { pathPrefix: dirname(WINDOWS_GIT_TAR) });
    assert.equal(result.status, 0, result.stderr);
    assert.equal(readFileSync(context.installedBinary, "utf8"), "local fixture only\n");
  }
);

test("Windows rejects an absent SystemRoot", () => {
  assert.throws(
    () => resolveTarExecutable("win32", ""),
    /SystemRoot is not set/
  );
});

test("Windows rejects a missing system tar", (t) => {
  const context = createCase(t);
  const emptyRoot = join(context.root, "empty Windows root");
  mkdirSync(emptyRoot);
  assert.throws(
    () => resolveTarExecutable("win32", emptyRoot),
    /Windows system tar not found/
  );
});

test("Windows propagates an invalid system tar failure", (t) => {
  const context = createCase(t);
  const invalidRoot = join(context.root, "invalid Windows root");
  const system32 = join(invalidRoot, "System32");
  mkdirSync(system32, { recursive: true });
  writeFileSync(join(system32, "tar.exe"), "not an executable\n");

  assert.throws(() => extractArchive(context.archive, join(context.root, "invalid output"), {
    platform: "win32",
    systemRoot: invalidRoot,
  }));
});

test("an extraction failure is propagated", (t) => {
  const context = createCase(t, { validArchive: false });
  const result = runInstaller(context);
  assert.notEqual(result.status, 0);
  assert.equal(existsSync(context.installedBinary), false);
});

test("a checksum mismatch stops before extraction", (t) => {
  const context = createCase(t);
  const result = runInstaller(context, { expectedSha: "0".repeat(64) });
  assert.notEqual(result.status, 0);
  assert.match(result.stderr, /checksum mismatch/);
  assert.equal(existsSync(join(context.packageDir, "bin")), false);
});

test("Linux keeps using tar from PATH", { skip: process.platform !== "linux" }, (t) => {
  const context = createCase(t);
  const wrapperDir = join(context.root, "PATH wrapper");
  const marker = join(context.root, "path-tar-invoked");
  mkdirSync(wrapperDir);
  writeFileSync(
    join(wrapperDir, "tar"),
    '#!/bin/sh\nprintf invoked > "$HANZO_TEST_TAR_MARKER"\nexec "$HANZO_TEST_REAL_TAR" "$@"\n'
  );
  chmodSync(join(wrapperDir, "tar"), 0o755);

  const realTar = execFileSync("sh", ["-c", "command -v tar"], { encoding: "utf8" }).trim();
  const env = { ...process.env };
  setEnvironmentKey(env, "HANZO_TEST_TAR_MARKER", marker);
  setEnvironmentKey(env, "HANZO_TEST_REAL_TAR", realTar);

  const currentPath = Object.entries(env).find(([key]) => key.toLowerCase() === "path")?.[1] || "";
  setEnvironmentKey(env, "PATH", `${wrapperDir}${delimiter}${currentPath}`);
  setEnvironmentKey(env, "HANZO_TEST_ARCHIVE", context.archive);
  setEnvironmentKey(env, "HANZO_TEST_ASSET", context.assetName);
  setEnvironmentKey(env, "HANZO_TEST_SHA", context.sha256);

  const result = spawnSync(
    process.execPath,
    ["--require", context.preload, join(context.packageDir, "install.js")],
    { cwd: context.root, env, encoding: "utf8", timeout: 30_000 }
  );
  assert.equal(result.status, 0, result.stderr);
  assert.equal(readFileSync(marker, "utf8"), "invoked");
  assert.equal(readFileSync(context.installedBinary, "utf8"), "local fixture only\n");
});
