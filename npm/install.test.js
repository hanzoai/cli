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
  readdirSync,
  readlinkSync,
  rmSync,
  symlinkSync,
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

function createCase(t, { validArchive = true, archiveMember, archiveKind = "file", archiveExtras = [] } = {}) {
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
    const member = archiveMember || binary;
    if (archiveKind === "directory") mkdirSync(join(sourceDir, member));
    else if (archiveKind === "symlink") symlinkSync("target-outside", join(sourceDir, member));
    else writeFileSync(join(sourceDir, member), archiveKind === "empty" ? "" : "local fixture only\n");
    for (const extra of archiveExtras) writeFileSync(join(sourceDir, extra), "sidecar fixture only\n");
    const archiveTar = process.platform === "win32" ? WINDOWS_SYSTEM_TAR : "tar";
    execFileSync(archiveTar, ["-czf", archive, "-C", sourceDir, member, ...archiveExtras]);
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
      '    if (process.env.HANZO_TEST_RAW_SHA !== undefined) return { ok: true, text: async () => process.env.HANZO_TEST_RAW_SHA };',
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

function runInstaller(context, { expectedSha = context.sha256, rawSha, pathPrefix, systemRoot, args = [] } = {}) {
  const env = { ...process.env };
  setEnvironmentKey(env, "HANZO_TEST_ARCHIVE", context.archive);
  setEnvironmentKey(env, "HANZO_TEST_ASSET", context.assetName);
  setEnvironmentKey(env, "HANZO_TEST_SHA", expectedSha);
  if (rawSha !== undefined) setEnvironmentKey(env, "HANZO_TEST_RAW_SHA", rawSha);
  else delete env.HANZO_TEST_RAW_SHA;

  if (pathPrefix) {
    const currentPath = Object.entries(process.env).find(
      ([key]) => key.toLowerCase() === "path"
    )?.[1] || "";
    setEnvironmentKey(env, "PATH", `${pathPrefix}${delimiter}${currentPath}`);
  }
  if (systemRoot !== undefined) setEnvironmentKey(env, "SystemRoot", systemRoot);

  return spawnSync(process.execPath, ["--require", context.preload, join(context.packageDir, "install.js"), ...args], {
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

test("replaces dangling native aliases and binary symlinks instead of writing through them", { skip: process.platform === "win32" }, (t) => {
  const context = createCase(t);
  const bin = join(context.packageDir, "bin");
  mkdirSync(bin);
  const alias = join(bin, "hanzo-cli");
  const staleAliasTarget = join(bin, "previous-hanzo");
  const staleBinaryTarget = join(bin, "previous-binary-target");
  symlinkSync("previous-hanzo", alias);
  symlinkSync("previous-binary-target", context.installedBinary);

  const result = runInstaller(context);
  assert.equal(result.status, 0, result.stderr);
  assert.equal(readlinkSync(alias), "hanzo", "alias should point at the currently installed binary");
  assert.equal(readFileSync(context.installedBinary, "utf8"), "local fixture only\n");
  assert.equal(existsSync(staleAliasTarget), false, "installer must not follow a dangling alias to create a stale file");
  assert.equal(existsSync(staleBinaryTarget), false, "installer must not follow a dangling binary symlink");
});

test("rejects an archive that extracts successfully but lacks the expected native binary", (t) => {
  const context = createCase(t, { archiveMember: "README.txt" });
  const result = runInstaller(context);
  assert.notEqual(result.status, 0, "an archive missing the binary must not be considered installed");
  assert.match(result.stderr, /expected binary|missing native binary/i);
  assert.equal(existsSync(context.installedBinary), false);
});

test("rejects directory, empty-file, and symlink placeholders instead of claiming a native binary was installed", (t) => {
  const kinds = process.platform === "win32" ? ["directory", "empty"] : ["directory", "empty", "symlink"];
  for (const archiveKind of kinds) {
    const context = createCase(t, { archiveKind });
    const result = runInstaller(context);
    assert.notEqual(result.status, 0, `${archiveKind} must not install successfully`);
    assert.match(result.stderr, /missing native binary/i);
  }
});

test("a valid replacement keeps additional archive files and clears staging", (t) => {
  const context = createCase(t, { archiveExtras: ["README.txt"] });
  mkdirSync(join(context.packageDir, "bin"));
  writeFileSync(context.installedBinary, "previous working binary\n");
  const result = runInstaller(context);
  assert.equal(result.status, 0, result.stderr);
  assert.equal(readFileSync(context.installedBinary, "utf8"), "local fixture only\n");
  assert.equal(readFileSync(join(context.packageDir, "bin", "README.txt"), "utf8"), "sidecar fixture only\n");
  assert.equal(readdirSync(join(context.packageDir, "bin")).some((name) => name.startsWith(".hanzo-staging-")), false);
});

test("a stale installed binary cannot mask a missing binary in a new release", (t) => {
  const context = createCase(t, { archiveMember: "README.txt" });
  mkdirSync(join(context.packageDir, "bin"));
  writeFileSync(context.installedBinary, "previous working binary\n");
  const result = runInstaller(context);
  assert.notEqual(result.status, 0, "missing release binary must fail despite an old installation");
  assert.match(result.stderr, /missing native binary/i);
  assert.equal(readFileSync(context.installedBinary, "utf8"), "previous working binary\n", "failed updates preserve working installed binaries");
});

test("a failed extraction removes the downloaded archive", (t) => {
  const context = createCase(t, { validArchive: false });
  const result = runInstaller(context);
  assert.notEqual(result.status, 0);
  assert.equal(existsSync(context.installedBinary), false);
  const leftovers = readdirSync(context.packageDir).filter(
    (name) => name.startsWith(`${result.pid}-`) && name.endsWith(".tar.gz")
  );
  assert.deepEqual(leftovers, [], "failed extraction must not leave the downloaded archive behind");
});

test("an empty or malformed checksum file refuses installation rather than skipping verification", (t) => {
  for (const rawSha of ["", "   \n", "not-a-sha256\n", "deadbeef  hanzo.tar.gz\n"]) {
    const context = createCase(t);
    const result = runInstaller(context, { rawSha });
    assert.notEqual(result.status, 0, `raw checksum ${JSON.stringify(rawSha)} must fail`);
    assert.match(result.stderr, /invalid checksum/i);
    assert.equal(existsSync(context.installedBinary), false);
  }
});

test("an optional companion fails closed on an empty checksum response", (t) => {
  const context = createCase(t);
  const result = runInstaller(context, { rawSha: "", args: ["gen"] });
  assert.notEqual(result.status, 0);
  assert.match(result.stderr, /invalid checksum/i);
});

test("uppercase SHA-256 checksum digests verify successfully", (t) => {
  const context = createCase(t);
  const result = runInstaller(context, { expectedSha: context.sha256.toUpperCase() });
  assert.equal(result.status, 0, result.stderr);
  assert.equal(readFileSync(context.installedBinary, "utf8"), "local fixture only\n");
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
