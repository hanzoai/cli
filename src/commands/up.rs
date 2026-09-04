//! `hanzo up` — a local Kubernetes: k3s in a Hanzo microVM.
//!
//! Bare `hanzo up` boots k3s inside a `hanzo-vm` microVM and hands back a
//! kubeconfig. The VM lives exactly as long as its supervisor — a daemonized
//! re-exec of this binary (`up supervise`, hidden) that holds `hanzo-vm run
//! --stdio` as a child and speaks its JSON-RPC over that stdio: exec k3s, poll
//! the node Ready, read the kubeconfig out of the guest. The vm's stdin is the
//! supervisor's leash — the supervisor dying closes it, the guest sees EOF and
//! stops — so `up down` is one SIGTERM.
//!
//! First boot creates a `k3s` disk checkpoint (downloads the binary once, in
//! the foreground so the download is visible); every later boot starts from it.
//!
//! What ran here before — the local cloud API — is `hanzo host serve` now;
//! `hanzo up <service>` forwards there for one release.

use anyhow::{anyhow, bail, Context, Result};
use base64::Engine;
use colored::*;
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::time::{Duration, Instant};

use crate::commands::{host, net, vm};
use crate::config::Config;

/// The VM's shape — one value through every layer, so the supervisor boots
/// exactly what the caller asked for.
pub struct Boot {
    pub cpus: u32,
    pub memory_mb: u64,
    pub disk_mb: u64,
}

/// The k3s API port, forwarded host→guest one-to-one.
const K3S_PORT: u16 = 6443;
/// The disk checkpoint every boot starts from.
const CHECKPOINT: &str = "k3s";
/// How long the guest gets to report a Ready node.
const READY_TIMEOUT: Duration = Duration::from_secs(180);
/// How long the foreground waits on the supervisor to reach `ready`.
const UP_TIMEOUT: Duration = Duration::from_secs(300);

// ---- state on disk -----------------------------------------------------------

/// `~/.hanzo/up` — supervisor pid, state and log.
fn up_dir() -> Result<PathBuf> {
    let d = dirs::home_dir()
        .ok_or_else(|| anyhow!("no home directory"))?
        .join(".hanzo")
        .join("up");
    std::fs::create_dir_all(&d).with_context(|| format!("creating {}", d.display()))?;
    Ok(d)
}

fn kubeconfig_path() -> Result<PathBuf> {
    Ok(dirs::home_dir()
        .ok_or_else(|| anyhow!("no home directory"))?
        .join(".kube")
        .join("hanzo.yaml"))
}

fn write_pid(dir: &Path, pid: u32) -> Result<()> {
    let f = dir.join("pid");
    std::fs::write(&f, pid.to_string()).with_context(|| format!("writing {}", f.display()))
}

fn read_pid(dir: &Path) -> Option<i32> {
    std::fs::read_to_string(dir.join("pid")).ok()?.trim().parse().ok()
}

fn clear_pid(dir: &Path) {
    let _ = std::fs::remove_file(dir.join("pid"));
}

fn write_vm_pid(dir: &Path, pid: u32) -> Result<()> {
    let f = dir.join("vm.pid");
    std::fs::write(&f, pid.to_string()).with_context(|| format!("writing {}", f.display()))
}

fn read_vm_pid(dir: &Path) -> Option<i32> {
    std::fs::read_to_string(dir.join("vm.pid")).ok()?.trim().parse().ok()
}

fn clear_vm_pid(dir: &Path) {
    let _ = std::fs::remove_file(dir.join("vm.pid"));
}

/// Signal 0 — the standard liveness test, and the only way to tell a live
/// supervisor from a stale pidfile.
#[cfg(unix)]
fn alive(pid: i32) -> bool {
    unsafe { libc::kill(pid, 0) == 0 }
}

#[cfg(not(unix))]
fn alive(_pid: i32) -> bool {
    false
}

#[cfg(unix)]
fn terminate(pid: i32) -> Result<()> {
    if unsafe { libc::kill(pid, libc::SIGTERM) } != 0 {
        bail!("signalling pid {pid}: {}", std::io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(not(unix))]
fn terminate(_pid: i32) -> Result<()> {
    bail!("the k3s supervisor is a unix daemon")
}

#[cfg(target_os = "linux")]
fn reap_stale_vms() {
    if let Ok(entries) = std::fs::read_dir("/proc") {
        let my_pid = std::process::id() as i32;
        let mut pids_to_kill = Vec::new();
        for entry in entries.flatten() {
            let name = entry.file_name();
            if let Some(pid) = name.to_str().and_then(|s| s.parse::<i32>().ok()) {
                if pid == my_pid {
                    continue;
                }
                let cmdline_path = format!("/proc/{pid}/cmdline");
                if let Ok(cmdline) = std::fs::read_to_string(&cmdline_path) {
                    let is_ch = cmdline.contains("cloud-hypervisor") && cmdline.contains("hanzo-vm");
                    let is_vm = cmdline.contains("hanzo-vm") && cmdline.contains("run");
                    if is_ch || is_vm {
                        pids_to_kill.push(pid);
                    }
                }
            }
        }
        for pid in &pids_to_kill {
            unsafe { libc::kill(*pid, libc::SIGTERM); }
        }
        if !pids_to_kill.is_empty() {
            std::thread::sleep(Duration::from_millis(200));
            for pid in pids_to_kill {
                if alive(pid) {
                    unsafe { libc::kill(pid, libc::SIGKILL); }
                }
            }
        }
    }
    for base in &["/dev/shm/gotmp", "/dev/shm"] {
        if let Ok(entries) = std::fs::read_dir(base) {
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().to_string();
                if name.starts_with("hanzo-vm") {
                    let _ = std::fs::remove_dir_all(entry.path());
                }
            }
        }
    }
}

#[cfg(not(target_os = "linux"))]
fn reap_stale_vms() {}

/// The supervisor's phase, written where the foreground (and `status`) can read
/// it: `boot` → `k3s` → `ready` → `down (…)`, or `error: …`. Best-effort — a
/// phase we cannot record is not a reason to stop booting.
fn write_state(dir: &Path, s: &str) {
    let _ = std::fs::write(dir.join("state"), s);
}

fn read_state(dir: &Path) -> Option<String> {
    std::fs::read_to_string(dir.join("state")).ok().map(|s| s.trim().to_string())
}

// ---- what the vm is asked to do ----------------------------------------------

/// The argv `hanzo-vm` boots the k3s VM with. `--stdio` is the supervisor's
/// wire; the port forward is what makes 127.0.0.1:6443 the API on the host.
fn run_args(boot: &Boot) -> Vec<String> {
    [
        "run",
        "--stdio",
        "--allow-net",
        "--cpus",
        &boot.cpus.to_string(),
        "--memory",
        &boot.memory_mb.to_string(),
        "--disk-size",
        &boot.disk_mb.to_string(),
        "-p",
        &format!("{K3S_PORT}:{K3S_PORT}"),
        "--from",
        CHECKPOINT,
    ]
    .iter()
    .map(|s| s.to_string())
    .collect()
}

/// Which k3s release asset this host's architecture boots. The guest runs the
/// host's architecture — the vm does not emulate.
fn k3s_asset() -> &'static str {
    if cfg!(target_arch = "aarch64") {
        "k3s-arm64"
    } else {
        "k3s"
    }
}

fn install_cmd() -> String {
    format!(
        "curl -Lo /usr/local/bin/k3s \
         https://github.com/k3s-io/k3s/releases/latest/download/{} \
         && chmod +x /usr/local/bin/k3s",
        k3s_asset()
    )
}

/// The one-time checkpoint: download k3s into the base image, save the disk.
fn checkpoint_args() -> Vec<String> {
    [
        "checkpoint",
        "create",
        CHECKPOINT,
        "--allow-net",
        "--",
        "sh",
        "-c",
        &install_cmd(),
    ]
    .iter()
    .map(|s| s.to_string())
    .collect()
}

/// Whether `hanzo-vm checkpoint list` names ours (the first column of a row).
fn has_checkpoint(listing: &str, name: &str) -> bool {
    listing
        .lines()
        .filter_map(|l| l.split_whitespace().next())
        .any(|first| first == name)
}

/// Whether `k3s kubectl get nodes --no-headers` reports a Ready node. The
/// status column is a comma-joined condition list, so `Ready` is matched as a
/// member, never as a substring — `NotReady` must not read as ready.
fn node_ready(out: &str) -> bool {
    out.lines().any(|l| {
        l.split_whitespace()
            .nth(1)
            .is_some_and(|status| status.split(',').any(|c| c == "Ready"))
    })
}

/// Point the guest's kubeconfig at the forwarded port. The guest writes its own
/// idea of an address; the forward is OURS, so every `server:` line is mapped
/// to it explicitly.
fn rewrite_server(yaml: &str) -> String {
    let mut out: String = yaml
        .lines()
        .map(|l| match l.find("server:") {
            Some(i) if l[..i].chars().all(|c| c == ' ') => {
                format!("{}server: https://127.0.0.1:{K3S_PORT}", &l[..i])
            }
            _ => l.to_string(),
        })
        .collect::<Vec<_>>()
        .join("\n");
    out.push('\n');
    out
}

/// Write the kubeconfig owner-only (0600): it carries the cluster's keys.
fn write_kubeconfig(path: &Path, yaml: &str) -> Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    }
    std::fs::write(path, yaml).with_context(|| format!("writing {}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
            .with_context(|| format!("chmod 600 {}", path.display()))?;
    }
    Ok(())
}

// ---- the vm's stdio JSON-RPC, spoken from the supervisor ----------------------

/// The `hanzo-vm --stdio` peer: JSON-lines JSON-RPC 2.0 on the child's
/// stdin/stdout (`vm-cli/src/stdio.rs`). Spoken BLOCKING — the supervisor is a
/// dedicated process with nothing else to do.
struct Rpc {
    child: Child,
    stdin: ChildStdin,
    out: BufReader<ChildStdout>,
    log: PathBuf,
    next: u64,
}

impl Rpc {
    /// Spawn the vm with its stderr appended to `log`; stdout is the protocol.
    fn start(bin: &Path, args: &[String], log: &Path) -> Result<Rpc> {
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
        #[cfg(target_os = "linux")]
        unsafe {
            use std::os::unix::process::CommandExt;
            cmd.pre_exec(|| {
                if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let mut child = cmd.spawn()
            .with_context(|| format!("starting {}", bin.display()))?;
        let stdin = child.stdin.take().expect("piped stdin");
        let out = BufReader::new(child.stdout.take().expect("piped stdout"));
        Ok(Rpc { child, stdin, out, log: log.to_path_buf(), next: 0 })
    }

    fn rpc_child_id(&self) -> u32 {
        self.child.id()
    }
}

impl Drop for Rpc {
    fn drop(&mut self) {
        let _ = self.child.kill();
    }
}

impl Rpc {

    /// One protocol line. EOF is the vm being gone — its own last words are in
    /// the log, so say where to look rather than guessing why.
    fn read_line(&mut self) -> Result<Value> {
        let mut line = String::new();
        loop {
            line.clear();
            if self.out.read_line(&mut line)? == 0 {
                let exit_info = match self.child.try_wait() {
                    Ok(Some(status)) => format!(" (exit status {status})"),
                    _ => String::new(),
                };
                let last_log = std::fs::read_to_string(&self.log)
                    .unwrap_or_default()
                    .lines()
                    .rev()
                    .take(5)
                    .collect::<Vec<_>>()
                    .into_iter()
                    .rev()
                    .collect::<Vec<_>>()
                    .join("\n");
                if !last_log.is_empty() {
                    bail!("hanzo-vm exited mid-conversation{exit_info}:\n{last_log}\nsee {}", self.log.display());
                } else {
                    bail!("hanzo-vm exited mid-conversation{exit_info} — see {}", self.log.display());
                }
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

    /// Block until the guest's `ready` notification.
    fn wait_ready(&mut self) -> Result<()> {
        loop {
            if self.read_line()?.get("method").and_then(Value::as_str) == Some("ready") {
                return Ok(());
            }
        }
    }

    /// One call: request out, notifications skipped, this id's result back. An
    /// `error` member is our error, never a silent null.
    fn call(&mut self, method: &str, params: Value) -> Result<Value> {
        self.next += 1;
        let id = self.next;
        let req = json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params });
        writeln!(self.stdin, "{req}").context("writing to hanzo-vm")?;
        self.stdin.flush().context("flushing to hanzo-vm")?;
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
    fn exec(&mut self, argv: &[&str]) -> Result<(String, String, i64)> {
        let r = self.call("exec", json!({ "argv": argv }))?;
        let s = |k: &str| r.get(k).and_then(Value::as_str).unwrap_or_default().to_string();
        let code = r.get("exit_code").and_then(Value::as_i64).unwrap_or(-1);
        Ok((s("stdout"), s("stderr"), code))
    }

    /// Start a long-lived guest process; its output arrives as notifications,
    /// which [`Rpc::call`] skips past.
    fn spawn(&mut self, argv: &[&str]) -> Result<String> {
        let r = self.call("spawn", json!({ "argv": argv }))?;
        r.get("pid")
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| anyhow!("spawn answered without a pid: {r}"))
    }

    /// Read a guest file (the wire carries it base64).
    fn read_file(&mut self, path: &str) -> Result<Vec<u8>> {
        let r = self.call("read_file", json!({ "path": path }))?;
        let content = r
            .get("content")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("read_file answered without content: {r}"))?;
        base64::engine::general_purpose::STANDARD
            .decode(content)
            .context("decode read_file content")
    }

    fn exited(&mut self) -> Option<std::process::ExitStatus> {
        self.child.try_wait().ok().flatten()
    }
}

// ---- the supervisor -----------------------------------------------------------

/// `hanzo up supervise` (hidden): the daemon `hanzo up` leaves behind. Owns the
/// vm for its whole life; this process dying — `up down`, a crash, a logout —
/// closes the vm's stdin, and EOF is how the guest stops.
pub async fn supervise(boot: Boot) -> Result<()> {
    let dir = up_dir()?;
    write_pid(&dir, std::process::id())?;
    // The same one resolver `hanzo up` used — by now the binary exists, but a
    // supervisor started by hand on a bare box bootstraps identically.
    let bin = vm::resolve_or_install().await?;
    let out = drive(&dir, &boot, &bin);
    if let Err(e) = &out {
        write_state(&dir, &format!("error: {e:#}"));
    }
    clear_pid(&dir);
    clear_vm_pid(&dir);
    out
}

/// Boot → k3s → Ready → kubeconfig → hold. Every phase lands in the state file
/// so the foreground (and `up status`) reads facts, not hope.
fn drive(dir: &Path, boot: &Boot, bin: &Path) -> Result<()> {
    write_state(dir, "boot");
    let log = dir.join("supervisor.log");
    let mut rpc = Rpc::start(bin, &run_args(boot), &log)?;
    let _ = write_vm_pid(dir, rpc.rpc_child_id());
    rpc.wait_ready()?;

    write_state(dir, "k3s");
    rpc.spawn(&["k3s", "server", "--disable", "traefik", "--disable", "metrics-server"])?;
    let deadline = Instant::now() + READY_TIMEOUT;
    loop {
        // A failing poll is k3s not answering YET — unless the vm itself is
        // gone, which no amount of waiting repairs.
        match rpc.exec(&["k3s", "kubectl", "get", "nodes", "--no-headers"]) {
            Ok((out, _, 0)) if node_ready(&out) => break,
            Ok(_) => {}
            Err(e) if rpc.exited().is_some() => return Err(e),
            Err(_) => {}
        }
        if Instant::now() >= deadline {
            bail!("k3s reported no Ready node within {}s", READY_TIMEOUT.as_secs());
        }
        std::thread::sleep(Duration::from_secs(3));
    }

    let yaml = String::from_utf8(rpc.read_file("/etc/rancher/k3s/k3s.yaml")?)
        .context("the guest kubeconfig is not utf-8")?;
    write_kubeconfig(&kubeconfig_path()?, &rewrite_server(&yaml))?;
    write_state(dir, "ready");

    // Hold the vm for as long as we live; its exit ends the watch either way.
    let status = rpc.child.wait().context("waiting on hanzo-vm")?;
    write_state(dir, &format!("down ({status})"));
    Ok(())
}

// ---- `hanzo up` and friends ---------------------------------------------------

/// Bare `hanzo up`: ensure the checkpoint, leave a supervisor behind, wait for
/// `ready`, print the one line to paste.
pub async fn up(cfg: &mut Config, boot: Boot, link: Option<String>) -> Result<()> {
    let dir = up_dir()?;
    if let Some(pid) = read_pid(&dir).filter(|p| alive(*p)) {
        let state = read_state(&dir).unwrap_or_else(|| "unknown".into());
        println!("{} already running (supervisor pid {pid}, {state})", "●".green());
        kubeconfig_hint()?;
        return finish_link(cfg, link).await;
    }
    if let Some(vmpid) = read_vm_pid(&dir).filter(|p| alive(*p)) {
        let _ = terminate(vmpid);
        std::thread::sleep(Duration::from_millis(300));
        if alive(vmpid) {
            #[cfg(unix)]
            unsafe { libc::kill(vmpid, libc::SIGKILL); }
        }
        clear_vm_pid(&dir);
    }
    reap_stale_vms();

    let bin = vm::resolve_or_install().await?;
    ensure_checkpoint(&bin)?;

    write_state(&dir, "starting");
    spawn_supervisor(&dir, &boot)?;
    wait_ready_state(&dir)?;
    println!("{} k3s is up — API at https://127.0.0.1:{K3S_PORT}", "✓".green());
    kubeconfig_hint()?;
    finish_link(cfg, link).await
}

fn kubeconfig_hint() -> Result<()> {
    println!("  export KUBECONFIG={}", kubeconfig_path()?.display());
    Ok(())
}

/// Create the `k3s` checkpoint when the store lacks it — in the FOREGROUND,
/// with inherited stdio, so the one-time k3s download is visible rather than a
/// silent minute.
fn ensure_checkpoint(bin: &Path) -> Result<()> {
    let out = Command::new(bin)
        .args(["checkpoint", "list"])
        .output()
        .with_context(|| format!("running {} checkpoint list", bin.display()))?;
    if has_checkpoint(&String::from_utf8_lossy(&out.stdout), CHECKPOINT) {
        return Ok(());
    }
    println!("{} creating the {CHECKPOINT} checkpoint (downloads k3s once)…", "→".cyan());
    let status = Command::new(bin)
        .args(checkpoint_args())
        .status()
        .with_context(|| format!("running {} checkpoint create", bin.display()))?;
    if !status.success() {
        bail!("checkpoint create failed ({status})");
    }
    Ok(())
}

/// Leave the supervisor behind: our own binary, re-run as the hidden
/// `up supervise`, detached into its own process group with its stdio on the
/// log — so it survives this command and Ctrl-C never reaches it.
fn spawn_supervisor(dir: &Path, boot: &Boot) -> Result<u32> {
    let exe = std::env::current_exe().context("resolving our own binary")?;
    let log = std::fs::File::create(dir.join("supervisor.log"))
        .with_context(|| format!("creating {}", dir.join("supervisor.log").display()))?;
    let mut cmd = Command::new(exe);
    cmd.args([
        "up",
        "--cpus",
        &boot.cpus.to_string(),
        "--memory",
        &boot.memory_mb.to_string(),
        "--disk-size",
        &boot.disk_mb.to_string(),
        "supervise",
    ])
    .stdin(Stdio::null())
    .stdout(Stdio::from(log.try_clone().context("duplicating the log handle")?))
    .stderr(Stdio::from(log));
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    let child = cmd.spawn().context("starting the k3s supervisor")?;
    Ok(child.id())
}

/// Watch the state file until the supervisor says `ready` — or says why not.
fn wait_ready_state(dir: &Path) -> Result<()> {
    let log = dir.join("supervisor.log");
    let deadline = Instant::now() + UP_TIMEOUT;
    loop {
        match read_state(dir).as_deref() {
            Some("ready") => return Ok(()),
            Some(s) if s.starts_with("error") || s.starts_with("down") => {
                bail!("{s} — see {}", log.display())
            }
            _ => {}
        }
        if Instant::now() >= deadline {
            bail!("k3s did not come up within {}s — see {}", UP_TIMEOUT.as_secs(), log.display());
        }
        std::thread::sleep(Duration::from_millis(500));
    }
}

/// `--link <cluster>`: mint the cluster's place on the org network — an
/// identity for this host and a service for the API. The guest half (enrolling
/// INSIDE the vm) needs a `zt` binary today's guest image does not carry, so
/// the minting is real and the enrollment is handed over, out loud.
async fn finish_link(cfg: &mut Config, link: Option<String>) -> Result<()> {
    let Some(cluster) = link else { return Ok(()) };
    let host_name = format!("k8s-{cluster}-host");
    let jwt = net::join(cfg, Some(host_name.clone()), vec![host_name.clone()]).await?;
    let dns = net::publish(cfg, format!("k8s-{cluster}"), format!("127.0.0.1:{K3S_PORT}")).await?;
    println!();
    println!("{}", "identity and service are minted; enrollment is manual for now:".bold());
    println!("  enroll this machine   zt edge enroll --jwt {}", jwt.display());
    println!(
        "  host the API          bind {dns} → 127.0.0.1:{K3S_PORT} as {host_name} (zt tunnel host)"
    );
    bail!(
        "not implemented: guest enrollment — the identity and service above exist; \
         finish with the steps printed"
    )
}

/// `hanzo up status` — the supervisor and the node, honestly separated: the
/// pidfile answers for the first, the kubeconfig (via kubectl) for the second.
pub async fn status() -> Result<()> {
    let dir = up_dir()?;
    let Some(pid) = read_pid(&dir).filter(|p| alive(*p)) else {
        println!("{} not running", "○".dimmed());
        return Ok(());
    };
    let state = read_state(&dir).unwrap_or_else(|| "unknown".into());
    println!("{} supervisor running (pid {pid}, {state})", "●".green());
    println!("  logs {}", dir.join("supervisor.log").display().to_string().dimmed());
    let kc = kubeconfig_path()?;
    if !kc.exists() {
        println!("  no kubeconfig yet ({})", kc.display());
        return Ok(());
    }
    match which::which("kubectl") {
        Ok(kubectl) => {
            let _ = Command::new(kubectl)
                .arg("--kubeconfig")
                .arg(&kc)
                .args(["get", "nodes"])
                .status();
        }
        Err(_) => println!("  kubectl not on PATH — KUBECONFIG={}", kc.display()),
    }
    Ok(())
}

/// `hanzo down` / `hanzo up down` — SIGTERM the supervisor; the vm's stdin closes with it and
/// the guest stops on the EOF. Cleans up the VM and supervisor.
pub fn down() -> Result<()> {
    let dir = up_dir()?;
    let sup_pid = read_pid(&dir).filter(|p| alive(*p));
    let vm_pid = read_vm_pid(&dir).filter(|p| alive(*p));

    if sup_pid.is_none() && vm_pid.is_none() {
        reap_stale_vms();
        clear_pid(&dir);
        clear_vm_pid(&dir);
        write_state(&dir, "down");
        println!("{} not running", "○".dimmed());
        return Ok(());
    }

    if let Some(pid) = sup_pid {
        #[cfg(unix)]
        unsafe {
            // Signal the entire process group (negative pid)
            let _ = libc::kill(-pid, libc::SIGTERM);
            let _ = libc::kill(pid, libc::SIGTERM);
        }
        let deadline = Instant::now() + Duration::from_secs(5);
        while alive(pid) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(100));
        }
        if alive(pid) {
            #[cfg(unix)]
            unsafe {
                let _ = libc::kill(-pid, libc::SIGKILL);
                let _ = libc::kill(pid, libc::SIGKILL);
            }
        }
    }

    if let Some(pid) = vm_pid {
        let _ = terminate(pid);
        let deadline = Instant::now() + Duration::from_secs(3);
        while alive(pid) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(100));
        }
        if alive(pid) {
            #[cfg(unix)]
            unsafe { libc::kill(pid, libc::SIGKILL); }
        }
    }

    reap_stale_vms();
    clear_pid(&dir);
    clear_vm_pid(&dir);
    write_state(&dir, "down");
    println!("{} down", "✓".green());
    Ok(())
}

/// The old `hanzo up [service]` spelling — split into the service and its tail.
fn service_argv(argv: Vec<String>) -> (String, Vec<String>) {
    let mut it = argv.into_iter();
    let service = it.next().unwrap_or_else(|| "cloud".into());
    let mut rest: Vec<String> = it.collect();
    if rest.first().map(String::as_str) == Some("--") {
        rest.remove(0);
    }
    (service, rest)
}

/// The old `hanzo up <service>` — forwards to `hanzo host serve` for one
/// release, saying so.
pub async fn deprecated_service(argv: Vec<String>) -> Result<()> {
    let (service, rest) = service_argv(argv);
    crate::warn(&format!(
        "`hanzo up {service}` is now `hanzo host serve {service}`; \
         this forwarding goes away next release"
    ));
    host::serve(service, rest).await
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The exact argv the k3s VM boots with — the stdio wire, the API forward,
    /// the checkpoint.
    #[test]
    fn the_vm_is_booted_with_the_stdio_wire_and_the_forward() {
        let boot = Boot { cpus: 4, memory_mb: 4096, disk_mb: 16384 };
        assert_eq!(
            run_args(&boot),
            [
                "run", "--stdio", "--allow-net", "--cpus", "4", "--memory", "4096",
                "--disk-size", "16384", "-p", "6443:6443", "--from", "k3s",
            ]
        );
    }

    /// The checkpoint downloads THIS architecture's k3s and marks it runnable.
    #[test]
    fn the_checkpoint_installs_k3s_for_this_architecture() {
        let args = checkpoint_args();
        assert_eq!(&args[..5], ["checkpoint", "create", "k3s", "--allow-net", "--"]);
        let cmd = args.last().unwrap();
        assert!(cmd.contains("k3s-io/k3s/releases/latest/download"), "{cmd}");
        assert!(cmd.contains(k3s_asset()), "{cmd}");
        assert!(cmd.contains("chmod +x /usr/local/bin/k3s"), "{cmd}");
        if cfg!(target_arch = "aarch64") {
            assert_eq!(k3s_asset(), "k3s-arm64");
        } else {
            assert_eq!(k3s_asset(), "k3s");
        }
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

    /// Ready is a MEMBER of the status column, never a substring: `NotReady`
    /// must not count, `Ready,SchedulingDisabled` must.
    #[test]
    fn a_node_is_ready_when_its_status_says_so() {
        assert!(node_ready("k3s-node   Ready    control-plane   30s   v1.30.0\n"));
        assert!(node_ready("n1   Ready,SchedulingDisabled   worker   1m   v1.30.0\n"));
        assert!(!node_ready("k3s-node   NotReady   control-plane   5s   v1.30.0\n"));
        assert!(!node_ready(""));
    }

    /// Every `server:` line is pointed at the forward; nothing else moves.
    #[test]
    fn the_kubeconfig_is_pointed_at_the_forwarded_port() {
        let yaml = "apiVersion: v1\nclusters:\n- cluster:\n    server: https://10.0.2.15:6443\n    certificate-authority-data: AAA\n";
        let out = rewrite_server(yaml);
        assert!(out.contains("    server: https://127.0.0.1:6443\n"), "{out}");
        assert!(!out.contains("10.0.2.15"), "{out}");
        assert!(out.contains("certificate-authority-data: AAA"), "{out}");
        // A comment naming server: elsewhere in the line is not an address.
        assert_eq!(rewrite_server("# the server: line\n"), "# the server: line\n");
    }

    /// The kubeconfig carries the cluster's keys: filed owner-only.
    #[test]
    fn the_kubeconfig_is_filed_owner_only() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".kube").join("hanzo.yaml");
        write_kubeconfig(&path, "apiVersion: v1\n").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "apiVersion: v1\n");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600, "mode {mode:o}");
        }
    }

    /// The pidfile lifecycle, proven on a real child (`sleep`): recorded, seen
    /// alive, terminated, seen gone, cleared.
    #[cfg(unix)]
    #[test]
    fn the_pidfile_follows_a_real_process() {
        let dir = tempfile::tempdir().unwrap();
        let mut child = Command::new("sleep").arg("30").spawn().unwrap();
        write_pid(dir.path(), child.id()).unwrap();

        let pid = read_pid(dir.path()).expect("pid reads back");
        assert_eq!(pid as u32, child.id());
        assert!(alive(pid));

        terminate(pid).unwrap();
        child.wait().unwrap(); // reap, so liveness is about the pid, not a zombie
        assert!(!alive(pid));

        clear_pid(dir.path());
        assert_eq!(read_pid(dir.path()), None);
    }

    /// The state file phases round-trip; the ready-watcher believes `ready`,
    /// reports an error, and refuses to wait on a `down`.
    #[test]
    fn the_state_file_carries_the_phase() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(read_state(dir.path()), None);
        write_state(dir.path(), "boot");
        assert_eq!(read_state(dir.path()).as_deref(), Some("boot"));

        write_state(dir.path(), "ready");
        wait_ready_state(dir.path()).unwrap();

        write_state(dir.path(), "error: no assets");
        let err = wait_ready_state(dir.path()).unwrap_err();
        assert!(err.to_string().contains("no assets"), "{err}");

        write_state(dir.path(), "down (exit status: 0)");
        assert!(wait_ready_state(dir.path()).is_err());
    }

    /// The RPC client against a fake peer speaking the real protocol: `ready`
    /// first, notifications skipped, results matched by id, errors surfaced.
    #[test]
    fn the_rpc_client_speaks_the_stdio_protocol() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("log");
        let script = concat!(
            r#"printf '%s\n' '{"jsonrpc":"2.0","method":"ready"}'; "#,
            r#"read line; "#,
            r#"printf '%s\n' '{"jsonrpc":"2.0","method":"output","params":{"pid":"p1","stream":"stdout","data":""}}'; "#,
            r#"printf '%s\n' '{"jsonrpc":"2.0","id":1,"result":{"stdout":"ok","stderr":"","exit_code":0}}'; "#,
            r#"read line; "#,
            r#"printf '%s\n' '{"jsonrpc":"2.0","id":2,"error":{"code":-32000,"message":"exec failed"}}'"#,
        );
        let mut rpc =
            Rpc::start(Path::new("sh"), &["-c".into(), script.into()], &log).unwrap();
        rpc.wait_ready().unwrap();

        let (out, err, code) = rpc.exec(&["true"]).unwrap();
        assert_eq!((out.as_str(), err.as_str(), code), ("ok", "", 0));

        let e = rpc.exec(&["false"]).unwrap_err();
        assert!(e.to_string().contains("exec failed"), "{e}");

        // The peer is done; the next read is an honest EOF error, not a hang.
        assert!(rpc.read_line().is_err());
        let _ = rpc.child.wait();
    }

    /// The old spelling splits into service + tail, with clap's `--` shed.
    #[test]
    fn the_old_up_spelling_splits_into_service_and_tail() {
        let v = |s: &[&str]| s.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        assert_eq!(service_argv(v(&["iam"])), ("iam".into(), vec![]));
        assert_eq!(
            service_argv(v(&["cloud", "--", "--port", "1"])),
            ("cloud".into(), v(&["--port", "1"]))
        );
        assert_eq!(service_argv(vec![]), ("cloud".into(), vec![]));
    }
}
