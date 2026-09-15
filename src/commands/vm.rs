//! `hanzo vm <args…>` — the native microVM CLI (hanzoai/vm), run verbatim — and
//! the one way this CLI drives it: install, checkpoints, and the stdio wire that
//! `hanzo up` and `hanzo build` both boot their vms through.
//!
//! One resolver, no reimplementation: `hanzo-vm` on PATH, else the place its
//! installer puts it (`~/.local/bin/hanzo-vm`) — and when neither exists, or the
//! found binary is older than [`VM_VERSION`], the CLI installs that pinned
//! release itself. `hanzo up` boots its k3s VM through the same
//! [`resolve_or_install`], so "where is the vm binary?" is answered in exactly
//! one place and a clean machine needs no separate install step.
//!
//! The pin is deliberate: the CLI controls which vm it spawns, never "latest at
//! runtime", so the same CLI build always boots the same vm. The release asset
//! is `hanzo-vm-v<VER>-<platform>.tar.gz` with a `.sha256` sidecar (the exact
//! names hanzoai/vm's install.sh and assets.rs use); the download is refused on
//! a digest mismatch. On macOS the fresh binary is ad-hoc signed with the
//! Virtualization.framework entitlement — unsigned, the kernel SIGKILLs it.

use crate::commands::launch;
use anyhow::{anyhow, bail, Context, Result};
use base64::Engine;
use colored::*;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::time::{Duration, Instant};
use vm_measure::{attest, Log};

/// The hanzoai/vm release this CLI installs and spawns.
pub(crate) const VM_VERSION: &str = "2.0.2";

/// The Virtualization.framework entitlement (hanzoai/vm's `vm.entitlements`),
/// vendored so signing needs no second download.
#[cfg(target_os = "macos")]
const ENTITLEMENTS: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>com.apple.security.virtualization</key>
    <true/>
</dict>
</plist>
"#;

/// Locate an existing `hanzo-vm`: PATH first, then the installer's default.
fn resolve() -> Option<PathBuf> {
    which::which("hanzo-vm").ok().or_else(|| {
        let p = dirs::home_dir()?.join(".local/bin/hanzo-vm");
        p.is_file().then_some(p)
    })
}

/// Locate `hanzo-vm`, installing the pinned release when it is absent or older
/// than [`VM_VERSION`]. The ONE entry point — `hanzo vm` and `hanzo up` both
/// come through here, so a bare machine bootstraps instead of erroring.
pub(crate) async fn resolve_or_install() -> Result<PathBuf> {
    if let Some(bin) = resolve() {
        match binary_version(&bin) {
            Some(found) if !older(&found, VM_VERSION) => return Ok(bin),
            Some(found) => eprintln!(
                "hanzo-vm {found} at {} is older than the pinned v{VM_VERSION} — updating",
                bin.display()
            ),
            // `--version` failed: a broken install (on macOS typically an
            // unsigned binary the kernel kills). Reinstalling is the repair.
            None => eprintln!(
                "hanzo-vm at {} does not answer --version — reinstalling",
                bin.display()
            ),
        }
    }
    install().await
}

/// `hanzo vm <args…>` — exec the binary with the args verbatim. A passthrough is
/// transparent: the child owns the terminal and its exit is our exit, exactly
/// the [`launch`] contract.
pub async fn run(args: Vec<String>) -> Result<()> {
    let bin = resolve_or_install().await?;
    launch::exec(&bin, &args)
}

// ---- the bootstrap ------------------------------------------------------------

/// Download the pinned release, verify its sha256 sidecar, extract `hanzo-vm`
/// to `~/.local/bin`, and (macOS) sign it for Virtualization.framework.
async fn install() -> Result<PathBuf> {
    let platform = platform(std::env::consts::OS, std::env::consts::ARCH).ok_or_else(|| {
        anyhow!(
            "no hanzo-vm build for {}-{} — it supports macOS arm64 and Linux x86_64/aarch64",
            std::env::consts::OS,
            std::env::consts::ARCH
        )
    })?;
    let asset = format!("hanzo-vm-v{VM_VERSION}-{platform}.tar.gz");
    let url = format!("https://github.com/hanzoai/vm/releases/download/v{VM_VERSION}/{asset}");
    eprintln!("installing hanzo-vm v{VM_VERSION} ({platform}) → ~/.local/bin/hanzo-vm …");

    let http = reqwest::Client::new();
    let tarball = fetch(&http, &url).await?;
    let sidecar = String::from_utf8(fetch(&http, &format!("{url}.sha256")).await?)
        .context("the .sha256 sidecar is not utf-8")?;
    verify_sha256(&tarball, &sidecar, &asset)?;

    let dir = dirs::home_dir()
        .context("no home directory")?
        .join(".local/bin");
    std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    let bin = dir.join("hanzo-vm");
    extract(&tarball, "hanzo-vm", &bin)?;
    #[cfg(target_os = "macos")]
    codesign(&bin)?;
    Ok(bin)
}

/// One GET, whole body, non-2xx is an error (a release asset either exists in
/// full or the install is off).
pub(crate) async fn fetch(http: &reqwest::Client, url: &str) -> Result<Vec<u8>> {
    let resp = http
        .get(url)
        .send()
        .await
        .with_context(|| format!("fetching {url}"))?;
    if !resp.status().is_success() {
        bail!("{url}: HTTP {}", resp.status());
    }
    Ok(resp
        .bytes()
        .await
        .with_context(|| format!("reading {url}"))?
        .to_vec())
}

/// Unpack the tarball entry whose file name is `name` to `dest`, atomically
/// (temp file in the same directory, then rename) and executable.
pub(crate) fn extract(tar_gz: &[u8], name: &str, dest: &Path) -> Result<()> {
    let mut archive = tar::Archive::new(flate2::read::GzDecoder::new(tar_gz));
    for entry in archive.entries().context("reading the release tarball")? {
        let mut entry = entry.context("reading a tarball entry")?;
        if entry.path().context("tarball entry path")?.file_name() != Some(name.as_ref()) {
            continue;
        }
        let tmp = dest.with_extension("tmp");
        entry
            .unpack(&tmp)
            .with_context(|| format!("unpacking to {}", tmp.display()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755))?;
        }
        std::fs::rename(&tmp, dest).with_context(|| format!("installing {}", dest.display()))?;
        return Ok(());
    }
    bail!("the release tarball has no {name} binary");
}

/// Ad-hoc sign with the virtualization entitlement. Without it the kernel
/// SIGKILLs the binary the moment it maps Virtualization.framework.
#[cfg(target_os = "macos")]
fn codesign(bin: &Path) -> Result<()> {
    let mut ent = tempfile::NamedTempFile::new().context("creating a temp entitlements file")?;
    std::io::Write::write_all(&mut ent, ENTITLEMENTS.as_bytes())?;
    let out = std::process::Command::new("codesign")
        .args(["--entitlements"])
        .arg(ent.path())
        .args(["--force", "-s", "-"])
        .arg(bin)
        .output()
        .context("running codesign")?;
    if !out.status.success() {
        bail!(
            "codesign failed on {} — hanzo-vm needs the virtualization entitlement to run:\n{}",
            bin.display(),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(())
}

// ---- booting one ----------------------------------------------------------------

/// The argv a driver boots a vm with: the stdio wire, the network, the shape, a
/// `-p host:guest` per forward, and the checkpoint the disk starts from.
pub(crate) fn run_args(
    cpus: u32,
    memory_mb: u64,
    disk_mb: u64,
    forwards: &[(u16, u16)],
    from: &str,
) -> Vec<String> {
    let mut args: Vec<String> = ["run", "--stdio", "--allow-net"].map(String::from).to_vec();
    for (flag, value) in [
        ("--cpus", cpus as u64),
        ("--memory", memory_mb),
        ("--disk-size", disk_mb),
    ] {
        args.extend([flag.to_string(), value.to_string()]);
    }
    for (host, guest) in forwards {
        args.extend(["-p".to_string(), format!("{host}:{guest}")]);
    }
    args.extend(["--from".to_string(), from.to_string()]);
    args
}

/// Create checkpoint `name` when the store lacks it — in the FOREGROUND, with
/// inherited stdio, so its one-time download is visible rather than a silent
/// minute.
pub(crate) fn ensure_checkpoint(bin: &Path, name: &str, install: &str) -> Result<()> {
    let out = Command::new(bin)
        .args(["checkpoint", "list"])
        .output()
        .with_context(|| format!("running {} checkpoint list", bin.display()))?;
    if has_checkpoint(&String::from_utf8_lossy(&out.stdout), name) {
        return Ok(());
    }
    println!("{} creating the {name} checkpoint (once)…", "→".cyan());
    let status = Command::new(bin)
        .args(checkpoint_args(name, install))
        .status()
        .with_context(|| format!("running {} checkpoint create", bin.display()))?;
    if !status.success() {
        bail!("checkpoint create failed ({status})");
    }
    Ok(())
}

/// The one-time checkpoint: `install` run in the base image, the disk saved as
/// `name`.
pub(crate) fn checkpoint_args(name: &str, install: &str) -> Vec<String> {
    [
        "checkpoint",
        "create",
        name,
        "--allow-net",
        "--",
        "sh",
        "-c",
        install,
    ]
    .map(String::from)
    .to_vec()
}

/// Whether `hanzo-vm checkpoint list` names ours (the first column of a row).
fn has_checkpoint(listing: &str, name: &str) -> bool {
    listing
        .lines()
        .filter_map(|l| l.split_whitespace().next())
        .any(|first| first == name)
}

// ---- the processes ----------------------------------------------------------------

/// Signal 0 — the standard liveness test, and the only way to tell a live
/// process from a stale pid.
#[cfg(unix)]
pub(crate) fn alive(pid: i32) -> bool {
    unsafe { libc::kill(pid, 0) == 0 }
}

#[cfg(not(unix))]
pub(crate) fn alive(_pid: i32) -> bool {
    false
}

/// Signal a process and everything it started. Every vm this CLI starts is a
/// process-group leader, so `-pid` reaches the group; the direct signal
/// follows in case it is not one. `ESRCH` from either is the answer
/// "already gone", which is the outcome we wanted.
///
/// The group is what makes this complete on x86-64, where the vm runs
/// cloud-hypervisor as a child of its own: signalling only `hanzo-vm` would
/// leave the hypervisor holding the guest's memory and its ports.
#[cfg(unix)]
pub(crate) fn signal(pid: i32, sig: i32) {
    unsafe {
        libc::kill(-pid, sig);
        libc::kill(pid, sig);
    }
}

// ---- the stdio wire -------------------------------------------------------------

/// The `hanzo-vm --stdio` peer: JSON-lines JSON-RPC 2.0 on the child's
/// stdin/stdout (`vm-cli/src/stdio.rs`). Spoken BLOCKING — a driver holds one vm
/// and has nothing else to do while it waits.
pub(crate) struct Rpc {
    pub(crate) child: Child,
    /// Closing this is the designed stop: the guest sees EOF and shuts down.
    /// An `Option` so [`Drop`] can close it while the child is still held.
    stdin: Option<ChildStdin>,
    out: BufReader<ChildStdout>,
    next: u64,
    /// Where the vm's stderr goes, named when it stops mid-conversation.
    log: PathBuf,
}

impl Rpc {
    /// Spawn the vm with its stderr appended to `log`; stdout is the protocol.
    pub(crate) fn start(bin: &Path, args: &[String], log: &Path) -> Result<Rpc> {
        let stderr = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(log)
            .with_context(|| format!("opening {}", log.display()))?;
        let mut cmd = Command::new(bin);
        cmd.args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::from(stderr));
        // Its own process group, so a later `stop` reaches whatever the vm
        // started — on x86-64 that is a cloud-hypervisor process holding the
        // guest's memory and its ports.
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            cmd.process_group(0);
        }
        let mut child = cmd
            .spawn()
            .with_context(|| format!("starting {}", bin.display()))?;
        let stdin = child.stdin.take().expect("piped stdin");
        let out = BufReader::new(child.stdout.take().expect("piped stdout"));
        Ok(Rpc {
            child,
            stdin: Some(stdin),
            out,
            next: 0,
            log: log.to_path_buf(),
        })
    }

    /// One protocol line. EOF is the vm being gone — its own last words are in
    /// the log, so say where to look rather than guessing why.
    fn read_line(&mut self) -> Result<Value> {
        let mut line = String::new();
        loop {
            line.clear();
            if self.out.read_line(&mut line)? == 0 {
                bail!(
                    "hanzo-vm exited mid-conversation — see {}",
                    self.log.display()
                );
            }
            let t = line.trim();
            if t.is_empty() {
                continue;
            }
            if let Ok(v) = serde_json::from_str::<Value>(t) {
                return Ok(v);
            }
        }
    }

    /// Block until the guest is up, and come back with what was booted.
    ///
    /// The vm sends its launch measurement before it says `ready` — that
    /// ordering is the point: the register is over the images the hypervisor
    /// was handed, taken before the guest could touch anything. A vm that
    /// reports no measurement is refused rather than run unmeasured.
    pub(crate) fn wait_ready(&mut self) -> Result<Log> {
        let mut launch = None;
        loop {
            let line = self.read_line()?;
            match line.get("method").and_then(Value::as_str) {
                Some("measurement") => {
                    let params = line
                        .get("params")
                        .ok_or_else(|| anyhow!("the vm sent a measurement with no log"))?;
                    launch = Some(
                        serde_json::from_value(params.clone())
                            .context("reading the vm's launch measurement")?,
                    );
                }
                Some("ready") => {
                    return launch.ok_or_else(|| {
                        anyhow!("this hanzo-vm booted without reporting a measurement")
                    })
                }
                _ => {}
            }
        }
    }

    /// One call: request out, notifications skipped, this id's result back. An
    /// `error` member is our error, never a silent null.
    fn call(&mut self, method: &str, params: Value) -> Result<Value> {
        self.next += 1;
        let id = self.next;
        let req = json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params });
        let stdin = self
            .stdin
            .as_mut()
            .ok_or_else(|| anyhow!("the vm's stdin is closed"))?;
        writeln!(stdin, "{req}").context("writing to hanzo-vm")?;
        stdin.flush().context("flushing to hanzo-vm")?;
        loop {
            let v = self.read_line()?;
            if v.get("id").and_then(Value::as_u64) != Some(id) {
                continue; // a notification (spawned k3s narrating), or nothing of ours
            }
            if let Some(e) = v.get("error") {
                bail!("{method}: {e}");
            }
            return Ok(v.get("result").cloned().unwrap_or(Value::Null));
        }
    }

    /// Run to completion in the guest: (stdout, stderr, exit code).
    pub(crate) fn exec(&mut self, argv: &[&str]) -> Result<(String, String, i64)> {
        let r = self.call("exec", json!({ "argv": argv }))?;
        let s = |k: &str| {
            r.get(k)
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string()
        };
        let code = r.get("exit_code").and_then(Value::as_i64).unwrap_or(-1);
        Ok((s("stdout"), s("stderr"), code))
    }

    /// Start a long-lived guest process; its output arrives as notifications,
    /// which [`Rpc::call`] skips past.
    pub(crate) fn spawn(&mut self, argv: &[&str]) -> Result<String> {
        let r = self.call("spawn", json!({ "argv": argv }))?;
        r.get("pid")
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| anyhow!("spawn answered without a pid: {r}"))
    }

    /// Write a guest file (the wire carries it base64), creating its directory.
    pub(crate) fn write_file(&mut self, path: &str, content: &str) -> Result<()> {
        let dir = path.rsplit_once('/').map(|(d, _)| d).unwrap_or("/");
        self.call("mkdir", json!({ "path": dir, "recursive": true }))?;
        let content = base64::engine::general_purpose::STANDARD.encode(content);
        self.call("write_file", json!({ "path": path, "content": content }))?;
        Ok(())
    }

    /// Ask the guest's platform for a report over `bind`. On hardware with
    /// neither `/dev/sev-guest` nor `/dev/tdx_guest` the answer is `none`,
    /// which the document records as the fact it is.
    pub(crate) fn attest(&mut self, bind: &str) -> Result<attest::Status> {
        let r = self.call("attest", json!({ "bind": bind }))?;
        serde_json::from_value(r).context("reading the guest's attestation status")
    }

    /// Read a guest file (the wire carries it base64).
    pub(crate) fn read_file(&mut self, path: &str) -> Result<Vec<u8>> {
        let r = self.call("read_file", json!({ "path": path }))?;
        let content = r
            .get("content")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("read_file answered without content: {r}"))?;
        base64::engine::general_purpose::STANDARD
            .decode(content)
            .context("decode read_file content")
    }

    pub(crate) fn exited(&mut self) -> Option<std::process::ExitStatus> {
        self.child.try_wait().ok().flatten()
    }

    /// Whether the child is finished within `patience`, reaping it if so. A
    /// child we killed is a zombie until we collect it, and `kill(pid, 0)`
    /// calls a zombie alive — so liveness for our OWN child is `try_wait`,
    /// never a signal.
    fn gone(&mut self, patience: Duration) -> bool {
        let deadline = Instant::now() + patience;
        loop {
            if self.exited().is_some() {
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

/// The vm does not outlive the conversation. Closing its stdin is the designed
/// stop and drop does that by itself — but only `hanzo-vm` is listening, and on
/// x86-64 it holds a cloud-hypervisor of its own. So the group is signalled and
/// the child is reaped here, where no error path can skip it.
///
/// Reaping through [`Child::wait`] rather than by polling for liveness is the
/// distinction that matters for a process we own: a killed child is a zombie
/// until its parent collects it, and `kill(pid, 0)` says a zombie is alive. The
/// parent is us.
impl Drop for Rpc {
    fn drop(&mut self) {
        // The leash, dropped: the guest sees EOF and shuts its disk down
        // cleanly. Everything below is for a vm that does not take the hint.
        self.stdin.take();
        if self.gone(Duration::from_secs(2)) {
            return;
        }
        #[cfg(unix)]
        {
            let pid = self.child.id() as i32;
            signal(pid, libc::SIGTERM);
            if self.gone(Duration::from_secs(3)) {
                return;
            }
            signal(pid, libc::SIGKILL);
        }
        let _ = self.child.wait();
    }
}

/// Test peers for the wire, shared by every module that drives one.
#[cfg(test)]
pub(crate) mod wire {
    use super::*;

    /// A fake peer speaking the real protocol, driven by a script.
    pub(crate) fn peer(dir: &Path, script: &str) -> Rpc {
        Rpc::start(
            Path::new("sh"),
            &["-c".into(), script.into()],
            &dir.join("log"),
        )
        .unwrap()
    }

    /// One `printf` of a protocol line, single-quoted for `sh`.
    pub(crate) fn line(json: &str) -> String {
        format!("printf '%s\\n' '{json}'; ")
    }

    /// A launch log as the vm would report one.
    pub(crate) fn launched() -> Log {
        let mut log = Log::default();
        log.text("kernel", "K");
        log.text("cmdline", "root=/dev/vda rw");
        log
    }
}

// ---- pure helpers (unit-tested) -----------------------------------------------

/// The release platform string, exactly install.sh's spelling. `None` when no
/// build exists for the host.
fn platform(os: &str, arch: &str) -> Option<&'static str> {
    match (os, arch) {
        ("macos", "aarch64") => Some("darwin-aarch64"),
        ("linux", "x86_64") => Some("linux-x86_64"),
        ("linux", "aarch64") => Some("linux-aarch64"),
        _ => None,
    }
}

/// `hanzo-vm --version` → its semver, from the last token of the first line
/// (`hanzo-vm 2.0.0`). `None` when the binary will not run or prints no version.
fn binary_version(bin: &Path) -> Option<String> {
    let out = std::process::Command::new(bin)
        .arg("--version")
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let first = String::from_utf8_lossy(&out.stdout);
    let token = first.lines().next()?.split_whitespace().last()?;
    let v = token.trim_start_matches('v');
    parse_semver(v).map(|_| v.to_string())
}

fn parse_semver(v: &str) -> Option<(u64, u64, u64)> {
    let mut it = v.splitn(3, '.');
    let major = it.next()?.parse().ok()?;
    let minor = it.next()?.parse().ok()?;
    // Tolerate a suffix (`2.0.0-rc1`): the numeric prefix orders it.
    let patch = it
        .next()?
        .split(|c: char| !c.is_ascii_digit())
        .next()?
        .parse()
        .ok()?;
    Some((major, minor, patch))
}

/// Is `found` strictly older than `pin`? Unparseable input counts as older —
/// a version we cannot read is not one we trust to boot.
fn older(found: &str, pin: &str) -> bool {
    match (parse_semver(found), parse_semver(pin)) {
        (Some(f), Some(p)) => f < p,
        _ => true,
    }
}

/// Compare the tarball against its `.sha256` sidecar (`<hex>  <filename>`).
/// A mismatch refuses the install — never run what we cannot verify.
pub(crate) fn verify_sha256(bytes: &[u8], sidecar: &str, asset: &str) -> Result<()> {
    let want = sidecar
        .split_whitespace()
        .next()
        .with_context(|| format!("{asset}.sha256 is empty"))?
        .to_ascii_lowercase();
    let got = hex(&Sha256::digest(bytes));
    if got != want {
        bail!(
            "sha256 mismatch for {asset}: expected {want}, downloaded {got} — refusing to install"
        );
    }
    Ok(())
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::wire::*;
    use super::*;

    #[test]
    fn platform_strings_match_the_release_assets() {
        assert_eq!(platform("macos", "aarch64"), Some("darwin-aarch64"));
        assert_eq!(platform("linux", "x86_64"), Some("linux-x86_64"));
        assert_eq!(platform("linux", "aarch64"), Some("linux-aarch64"));
        assert_eq!(platform("macos", "x86_64"), None);
        assert_eq!(platform("windows", "x86_64"), None);
    }

    #[test]
    fn version_ordering_drives_the_update() {
        // A vm older than the pin is replaced — 2.0.0 has no measurement to
        // report, and `hanzo up` refuses a vm that reports none.
        assert!(older("2.0.0", VM_VERSION));
        // 2.0.1's `run --stdio` could outlive its closed stdin, holding the
        // forwarded ports; closing stdin is how every driver here stops a vm.
        assert!(older("2.0.1", VM_VERSION));
        assert!(older("0.1.3", "2.0.0"));
        assert!(older("1.9.9", "2.0.0"));
        assert!(!older("2.0.0", "2.0.0"));
        assert!(!older("2.0.1", "2.0.0"));
        assert!(!older("10.0.0", "2.0.0"));
        // Unreadable is older: reinstall rather than trust it.
        assert!(older("garbage", "2.0.0"));
        assert!(older("", "2.0.0"));
        // A suffixed patch still orders by its numeric prefix.
        assert!(older("2.0.0-rc1", "2.0.1"));
    }

    #[test]
    fn sha256_sidecar_accepts_the_true_digest() {
        let body = b"the vm tarball";
        let sidecar = format!("{}  asset.tar.gz\n", hex(&Sha256::digest(body)));
        verify_sha256(body, &sidecar, "asset.tar.gz").unwrap();
    }

    #[test]
    fn sha256_sidecar_refuses_a_tampered_download() {
        let sidecar = format!(
            "{}  asset.tar.gz\n",
            hex(&Sha256::digest(b"the vm tarball"))
        );
        let err = verify_sha256(b"tampered bytes", &sidecar, "asset.tar.gz").unwrap_err();
        assert!(err.to_string().contains("sha256 mismatch"), "{err}");
    }

    /// A crafted tar.gz (no network): verify → extract → executable file lands.
    #[test]
    fn extract_installs_the_hanzo_vm_entry() {
        let mut tar = tar::Builder::new(Vec::new());
        let body = b"#!/bin/sh\necho hanzo-vm test\n";
        let mut hdr = tar::Header::new_gnu();
        hdr.set_size(body.len() as u64);
        hdr.set_mode(0o755);
        hdr.set_cksum();
        tar.append_data(&mut hdr, "hanzo-vm", &body[..]).unwrap();
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        std::io::Write::write_all(&mut gz, &tar.into_inner().unwrap()).unwrap();
        let tarball = gz.finish().unwrap();

        let sidecar = format!("{}  x.tar.gz", hex(&Sha256::digest(&tarball)));
        verify_sha256(&tarball, &sidecar, "x.tar.gz").unwrap();

        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("hanzo-vm");
        extract(&tarball, "hanzo-vm", &dest).unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), body);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&dest).unwrap().permissions().mode() & 0o777,
                0o755
            );
        }
    }

    #[test]
    fn extract_refuses_a_tarball_without_the_binary() {
        let mut tar = tar::Builder::new(Vec::new());
        let mut hdr = tar::Header::new_gnu();
        hdr.set_size(2);
        hdr.set_mode(0o644);
        hdr.set_cksum();
        tar.append_data(&mut hdr, "README", &b"hi"[..]).unwrap();
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        std::io::Write::write_all(&mut gz, &tar.into_inner().unwrap()).unwrap();
        let tarball = gz.finish().unwrap();

        let dir = tempfile::tempdir().unwrap();
        let err = extract(&tarball, "hanzo-vm", &dir.path().join("hanzo-vm")).unwrap_err();
        assert!(err.to_string().contains("no hanzo-vm binary"), "{err}");
    }

    /// `checkpoint list` is parsed by its first column; the header is not a
    /// checkpoint and prose ("No checkpoints found.") is not one either.
    #[test]
    fn the_checkpoint_listing_is_read_by_name() {
        let listing = "NAME                       SIZE CREATED\nk3s                      512 MB 2h ago\nbuild                    128 MB 1d ago\n";
        assert!(has_checkpoint(listing, "k3s"));
        assert!(has_checkpoint(listing, "build"));
        assert!(!has_checkpoint(listing, "k3"));
        assert!(!has_checkpoint("", "k3s"));
        assert!(!has_checkpoint("No checkpoints found.\n", "k3s"));
    }

    /// The vm does not outlive its conversation, by any path. Dropping the Rpc
    /// closes the leash and collects the child — and takes the group with it,
    /// so a hypervisor started underneath goes too.
    #[cfg(unix)]
    #[test]
    fn dropping_the_conversation_stops_the_vm() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("grandchild");
        // A peer that ignores EOF and holds a child of its own: SIGTERM to the
        // group is the only thing that ends it.
        let script = format!(
            "trap '' HUP; sleep 60 & echo $! > {}; sleep 60",
            file.display()
        );
        let rpc = peer(dir.path(), &script);
        let pid = rpc.child.id() as i32;

        let deadline = Instant::now() + Duration::from_secs(10);
        let grandchild = loop {
            if let Some(p) = std::fs::read_to_string(&file)
                .ok()
                .and_then(|s| s.trim().parse::<i32>().ok())
                .filter(|p| alive(*p))
            {
                break p;
            }
            assert!(Instant::now() < deadline, "the grandchild never started");
            std::thread::sleep(Duration::from_millis(50));
        };

        drop(rpc);
        assert!(!alive(pid), "the vm survived the drop");
        let deadline = Instant::now() + Duration::from_secs(5);
        while alive(grandchild) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(50));
        }
        assert!(!alive(grandchild), "the grandchild survived the drop");
    }

    /// The RPC client against a fake peer: the measurement before `ready`,
    /// notifications skipped, results matched by id, errors surfaced.
    #[test]
    fn the_rpc_client_speaks_the_stdio_protocol() {
        let dir = tempfile::tempdir().unwrap();
        let measurement = json!({"jsonrpc": "2.0", "method": "measurement", "params": launched()});
        let script = format!(
            "{}{}{}{}{}{}{}",
            line(&measurement.to_string()),
            line(r#"{"jsonrpc":"2.0","method":"ready"}"#),
            "read line; ",
            line(
                r#"{"jsonrpc":"2.0","method":"output","params":{"pid":"p1","stream":"stdout","data":""}}"#
            ),
            line(r#"{"jsonrpc":"2.0","id":1,"result":{"stdout":"ok","stderr":"","exit_code":0}}"#),
            "read line; ",
            line(r#"{"jsonrpc":"2.0","id":2,"error":{"code":-32000,"message":"exec failed"}}"#),
        );
        let mut rpc = peer(dir.path(), &script);
        assert_eq!(rpc.wait_ready().unwrap(), launched());

        let (out, err, code) = rpc.exec(&["true"]).unwrap();
        assert_eq!((out.as_str(), err.as_str(), code), ("ok", "", 0));

        let e = rpc.exec(&["false"]).unwrap_err();
        assert!(e.to_string().contains("exec failed"), "{e}");

        // The peer is done; the next read is an honest EOF error, not a hang.
        assert!(rpc.read_line().is_err());
        let _ = rpc.child.wait();
    }

    /// A vm that says `ready` without saying what it booted is refused. An
    /// unmeasured cluster is not one this can file a document about, and
    /// filing nothing quietly would be worse than not starting.
    #[test]
    fn a_vm_that_reports_no_measurement_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let mut rpc = peer(dir.path(), &line(r#"{"jsonrpc":"2.0","method":"ready"}"#));
        let e = rpc.wait_ready().unwrap_err();
        assert!(
            e.to_string().contains("without reporting a measurement"),
            "{e}"
        );
        let _ = rpc.child.wait();
    }
}
