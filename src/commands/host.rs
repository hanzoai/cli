//! `hanzo host` — the LOCAL cloud, so every cloud command works with no server.
//!
//! The local cloud is served by `hanzod`, the one daemon per machine (HIP-1330),
//! which starts the cloud host as its own child on the first request and stops
//! it when idle. This module is a CLIENT of that daemon: every cloud call
//! resolves WHERE through [`origin`], and when the active network points at
//! loopback that resolution guarantees `hanzod` is answering on its ZAP socket,
//! starting it if it is not. Nothing here launches or supervises the host.
//!
//! WHY a unix socket, and WHY ZAP: the daemon serves the SAME routes on two wires
//! — ZAP, the fleet's primary transport, and a loopback HTTP view for third
//! parties who cannot speak it. The CLI is first party, so locally it speaks ZAP
//! ([`crate::zap`]) over the daemon's socket in the user's own state directory
//! (mode 0600): no HTTP grammar, no second serialization, no port. Windows has no
//! unix socket to speak it over, so there the payload is the loopback base URL.

use anyhow::{anyhow, bail, Context, Result};
use base64::Engine;
use colored::*;
use hanzo_client::Method;
use rand::RngCore;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use crate::commands::network;
use crate::config::Config;

/// How long a freshly started daemon gets to answer `/healthz`.
const READY_TIMEOUT: Duration = Duration::from_secs(10);

/// How long hanzod gets to drain its children on SIGTERM.
const STOP_TIMEOUT: Duration = Duration::from_secs(30);

/// How long to wait on a single `/healthz` probe. Short: the question is only
/// "is someone already listening", and the answer is local either way.
const PROBE_TIMEOUT: Duration = Duration::from_millis(500);

/// Number of trailing non-empty lines from `hanzod.log` included in startup errors.
const LOG_TAIL_LINES: usize = 8;

/// Where a cloud call goes, and over which wire.
pub enum Origin {
    /// THE LOCAL HOST — which is the fact every rule below actually asks about,
    /// so it is one variant and the wire is its payload, not a second variant.
    ///
    /// On unix that payload is the host's ZAP socket: ZAP is the fleet's PRIMARY
    /// transport and the CLI is first party, so locally it speaks the native wire —
    /// the HTTP view the host also serves exists for third parties, and we are not
    /// one. Windows has no unix socket to speak it over, so there the payload is
    /// the loopback base URL and the SAME host answers the SAME routes over that
    /// HTTP view. The wire differs; the peer does not.
    #[cfg(unix)]
    Local(PathBuf),
    #[cfg(not(unix))]
    Local(String),
    /// A published `api.*` origin over HTTPS. TLS terminates at the ingress and
    /// zaphttp has no handshake of its own yet, so a remote host is reached the
    /// way third parties reach it. This is the EXCEPTION, kept only until the ZAP
    /// transport carries its own session crypto.
    Http(String),
}

/// The ONE origin resolver for every cloud call. A loopback network resolves to
/// the local host, guaranteed to be listening by the time this returns; anything
/// else is passed through as HTTP — nothing started, nothing probed.
pub async fn origin(cfg: &Config) -> Result<Origin> {
    let api = network::active(cfg).api.trim_end_matches('/').to_string();
    match loopback_addr(&api) {
        Some(addr) => {
            ensure(&addr).await?;
            local_wire(&addr)
        }
        None => Ok(Origin::Http(api)),
    }
}

/// How the local host is reached on this platform — the ONE place the two wires
/// diverge, so nothing downstream has to know which it got.
#[cfg(unix)]
fn local_wire(_addr: &str) -> Result<Origin> {
    Ok(Origin::Local(zap_socket()?))
}

#[cfg(not(unix))]
fn local_wire(addr: &str) -> Result<Origin> {
    Ok(Origin::Local(format!("http://{addr}")))
}

/// The daemon's ZAP socket, `<data>/hanzo/hanzod.zap.sock`. The daemon creates
/// it 0600; the path is the one thing the two sides agree on.
#[cfg(unix)]
pub fn zap_socket() -> Result<PathBuf> {
    Ok(data_dir()?.join("hanzod.zap.sock"))
}

/// The `host:port` to bind when `origin` is loopback, else `None`. The host binds
/// `tcp4`, so this always names the v4 loopback whatever spelling the URL used.
fn loopback_addr(origin: &str) -> Option<String> {
    let url = reqwest::Url::parse(origin).ok()?;
    // An IPv6 host arrives spelled with its brackets, which is not what parses as
    // an address. Everything loopback answers here — all of 127.0.0.0/8 and ::1 —
    // so the test is the address's own, not a list of spellings.
    let host = url
        .host_str()?
        .trim_start_matches('[')
        .trim_end_matches(']');
    let local = host == "localhost"
        || host
            .parse::<std::net::IpAddr>()
            .is_ok_and(|ip| ip.is_loopback());
    local.then(|| format!("127.0.0.1:{}", url.port_or_known_default().unwrap_or(80)))
}

/// True when a host answers `/healthz`. Liveness is the HOST's own route — it
/// answers while every plugin is still cold — so this asks whether the router is
/// up, never whether any subsystem is.
///
/// Probed over the same wire every other local call uses, so "running" always
/// means the same thing as "reachable": on unix that is ZAP over the socket, and
/// a bound TCP port is never what running means; on Windows it is the HTTP view,
/// because that is the only local wire there.
#[cfg(unix)]
async fn healthy(_addr: &str) -> bool {
    let Ok(sock) = zap_socket() else { return false };
    let probe = crate::zap::send(&sock, &Method::GET, "/healthz", "", None);
    matches!(tokio::time::timeout(PROBE_TIMEOUT, probe).await, Ok(Ok(r)) if (200..300).contains(&r.status))
}

#[cfg(not(unix))]
pub(crate) async fn healthy(addr: &str) -> bool {
    use hanzo_client::{Http, Request, Transport};
    let http = Http::default();
    for path in ["/healthz", "/v1/openapi.json"] {
        let url = format!("http://{addr}{path}");
        let probe = http.send(Request::new(Method::GET, url));
        if matches!(tokio::time::timeout(PROBE_TIMEOUT, probe).await, Ok(Ok(r)) if (200..300).contains(&r.status))
        {
            return true;
        }
    }
    false
}

/// Reuse a running daemon, or start one and wait for it to answer.
pub(crate) async fn ensure(addr: &str) -> Result<()> {
    if healthy(addr).await {
        return Ok(()); // already serving — never double-start
    }
    let bin = binary()?;
    if let Some(detail) = is_legacy_chain_binary(&bin) {
        return Err(legacy_chain_error(&bin, &detail));
    }
    let mut child = spawn(&bin)?;
    let log = log_path()?;
    let deadline = Instant::now() + READY_TIMEOUT;
    loop {
        if healthy(addr).await {
            return Ok(());
        }
        if let Some(status) = child.try_wait().context("waiting on hanzod")? {
            return Err(format_start_failure(
                &bin,
                &log,
                &format!("hanzod exited ({status})"),
            ));
        }
        if Instant::now() >= deadline {
            return Err(format_start_failure(
                &bin,
                &log,
                &format!("hanzod did not answer within {}s", READY_TIMEOUT.as_secs()),
            ));
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// Read the last `max_lines` non-empty lines of `path`, or `None` if absent/empty.
fn log_tail(path: &Path, max_lines: usize) -> Option<String> {
    let raw = std::fs::read_to_string(path).ok()?;
    let mut lines: Vec<&str> = raw
        .lines()
        .map(str::trim_end)
        .filter(|l| !l.trim().is_empty())
        .collect();
    if lines.is_empty() {
        return None;
    }
    if lines.len() > max_lines {
        lines.drain(..lines.len() - max_lines);
    }
    Some(lines.join("\n"))
}

/// Detect output from the legacy L1 chain validator binary (split into
/// `hanzo-chain` in `hanzoai/node` v0.2.6+, previously published as `hanzod` v0.2.5).
fn looks_like_legacy_chain_text(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    lower.contains("no validators")
        || lower.contains("--committee")
        || lower.contains("hanzo-chain")
}

/// Parse `<major>.<minor>.<patch>` from the second whitespace token of a
/// `hanzod <ver>` version line.
fn parse_hanzod_semver(line: &str) -> Option<(u64, u64, u64)> {
    let mut parts = line.split_whitespace();
    let name = parts.next()?.to_ascii_lowercase();
    if name != "hanzod" {
        return None;
    }
    let ver = parts.next()?.trim_start_matches('v');
    let mut nums = ver.splitn(3, '.');
    let major = nums.next()?.parse().ok()?;
    let minor = nums.next()?.parse().ok()?;
    let patch = nums
        .next()?
        .split(|c: char| !c.is_ascii_digit())
        .next()?
        .parse()
        .ok()?;
    Some((major, minor, patch))
}

/// Classify `hanzod --version` output. Returns `Some(reason)` when the binary is
/// the old L1 chain validator (or otherwise not the unified `hanzod` daemon).
fn classify_version_output(stdout: &str, stderr: &str, success: bool) -> Option<String> {
    let combined = format!("{stdout}\n{stderr}");
    let first_line = combined
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("");
    if looks_like_legacy_chain_text(&combined) {
        return Some(format!("reported legacy validator output: `{first_line}`"));
    }
    let lower = combined.to_ascii_lowercase();
    let mentions_daemon = lower.contains("hanzod") || lower.contains("daemon");
    if !success {
        if !mentions_daemon {
            return Some(if first_line.is_empty() {
                "`--version` exited non-zero without identifying `hanzod`".into()
            } else {
                format!("`--version` failed: `{first_line}`")
            });
        }
        return None;
    }
    if !mentions_daemon {
        return Some(if first_line.is_empty() {
            "`--version` produced no output identifying `hanzod`".into()
        } else {
            format!("`--version` printed `{first_line}`, not the unified `hanzod` daemon")
        });
    }
    // `hanzod` releases <= 0.2.5 (e.g. Nov-11 GitHub Release v0.2.5) were the
    // standalone L1 validator binary before `hanzoai/node` split `chain/` and `daemon/`.
    if !lower.contains("dev=") && !lower.contains("daemon") {
        if let Some(ver) = parse_hanzod_semver(first_line) {
            if ver < (0, 2, 6) {
                return Some(format!(
                    "`--version` reported `{first_line}` (pre-0.2.6 L1 validator release)"
                ));
            }
        }
    }
    None
}

/// Probe `bin --version` before spawning so a stale chain-validator binary is
/// caught with an actionable diagnostic rather than failing after spawn.
fn is_legacy_chain_binary(bin: &Path) -> Option<String> {
    let out = Command::new(bin)
        .arg("--version")
        .stdin(Stdio::null())
        .output()
        .ok()?;
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    classify_version_output(&stdout, &stderr, out.status.success())
}

fn legacy_chain_error(bin: &Path, detail: &str) -> anyhow::Error {
    anyhow!(
        "installed binary at {} is the legacy L1 chain validator (now `hanzo-chain`), \
         not the unified `hanzod` daemon ({detail}).\n\
         Install the unified `hanzod` daemon with:\n  \
           hanzo upgrade hanzod\n\
         Or build and install it from source (hanzoai/node `daemon/`):\n  \
           cargo install --git https://github.com/hanzoai/node --locked hanzod --root ~/.hanzo",
        bin.display()
    )
}

fn format_start_failure(bin: &Path, log: &Path, headline: &str) -> anyhow::Error {
    match log_tail(log, LOG_TAIL_LINES) {
        Some(tail) if looks_like_legacy_chain_text(&tail) => {
            let indented = tail
                .lines()
                .map(|l| format!("  {l}"))
                .collect::<Vec<_>>()
                .join("\n");
            anyhow!(
                "{}\n\nlast lines of {}:\n{indented}",
                legacy_chain_error(bin, &format!("{headline}; log mentions validator flags")),
                log.display()
            )
        }
        Some(tail) => {
            let indented = tail
                .lines()
                .map(|l| format!("  {l}"))
                .collect::<Vec<_>>()
                .join("\n");
            anyhow!(
                "{headline} — see {}\nlast lines of {}:\n{indented}",
                log.display(),
                log.display()
            )
        }
        None => anyhow!("{headline} — see {}", log.display()),
    }
}

/// Locate `hanzod`: `~/.hanzo/bin/hanzod` (the one binary on the machine), else
/// beside this executable, else `PATH`. Never a checkout: the daemon runs only
/// programs whose digest it has pinned, and the same holds for starting it.
fn binary() -> Result<PathBuf> {
    let home = dirs::home_dir().unwrap_or_default();
    let name = if cfg!(windows) {
        "hanzod.exe"
    } else {
        "hanzod"
    };
    let mut candidates = vec![home.join(".hanzo").join("bin").join(name)];
    if let Some(dir) = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(PathBuf::from))
    {
        candidates.push(dir.join(name));
    }
    for c in &candidates {
        if c.is_file() {
            return Ok(c.clone());
        }
    }
    if let Ok(found) = which::which("hanzod") {
        return Ok(found);
    }
    bail!(
        "hanzod is not installed at {}.\nInstall it with `hanzo upgrade hanzod`.",
        candidates[0].display()
    );
}

/// Start the daemon detached, logging to the data dir.
fn spawn(bin: &Path) -> Result<Child> {
    let log = std::fs::File::create(log_path()?).with_context(|| {
        format!(
            "creating {}",
            log_path()
                .map(|p| p.display().to_string())
                .unwrap_or_default()
        )
    })?;
    let mut cmd = Command::new(bin);
    cmd.stdin(Stdio::null())
        .stdout(Stdio::from(
            log.try_clone()
                .context("duplicating the hanzod log handle")?,
        ))
        .stderr(Stdio::from(log));
    detach(&mut cmd);
    cmd.spawn()
        .with_context(|| format!("starting {}", bin.display()))
}

/// Put the daemon in its own process group so the Ctrl-C that interrupts the CLI
/// does not also kill the daemon it just started.
#[cfg(unix)]
fn detach(cmd: &mut Command) {
    use std::os::unix::process::CommandExt;
    cmd.process_group(0);
}

#[cfg(windows)]
fn detach(cmd: &mut Command) {
    use std::os::windows::process::CommandExt;
    const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
    const DETACHED_PROCESS: u32 = 0x0000_0008;
    cmd.creation_flags(CREATE_NEW_PROCESS_GROUP | DETACHED_PROCESS);
}

#[cfg(all(not(unix), not(windows)))]
fn detach(_cmd: &mut Command) {}

// ---- the at-rest key -----------------------------------------------------------

/// SHA-256 of the fixed development key that pre-release builds of `hanzo up`
/// wrote to `~/.hanzo/cloud/master.key`. An at-rest key belongs to one install,
/// so this one is recognised by its digest and never used.
const SHARED_KEY_SHA256: &str = "cd80b74bd7fc76170d3355339fbe4797ac5ae226aa5f28adff8512b990e48faf";

/// 32 fresh bytes, base64 — the shape the cloud reads its at-rest key in. From
/// the OS generator, never a derivation: a key a second machine could guess is
/// not one.
pub fn key() -> String {
    let mut bytes = [0u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

fn shared(key: &str) -> bool {
    format!("{:x}", Sha256::digest(key.trim().as_bytes())) == SHARED_KEY_SHA256
}

/// Retire the local cloud state pre-release builds of `hanzo up` kept in
/// `~/.hanzo/cloud`: a store sealed under the shared development key, and that
/// key. Nothing reads the directory any more, and only the cloud that wrote a
/// store can re-seal it, so both are removed; the local host seals a fresh
/// store under this install's own key. Runs on every invocation, and once it
/// has run it costs one failed open.
pub fn retire_shared() {
    let Some(home) = dirs::home_dir() else { return };
    let dir = home.join(".hanzo").join("cloud");
    match retire(&dir, shared) {
        Ok(true) => eprintln!(
            "{} removed the local cloud state in {} and its development key; the local host now keeps a key of its own",
            "→".cyan(),
            dir.display()
        ),
        Ok(false) => {}
        Err(e) => eprintln!("{} {e:#}", "warning:".yellow()),
    }
}

/// Remove `dir`'s store and key when `is_shared` recognises the key. The store
/// goes first: the key file is the marker, so a removal that fails part-way is
/// taken up again by the next invocation.
fn retire(dir: &Path, is_shared: fn(&str) -> bool) -> Result<bool> {
    let key = dir.join("master.key");
    match std::fs::read_to_string(&key) {
        Ok(k) if is_shared(&k) => {}
        _ => return Ok(false),
    }
    let data = dir.join("data");
    if data.exists() {
        std::fs::remove_dir_all(&data).with_context(|| format!("removing {}", data.display()))?;
    }
    std::fs::remove_file(&key).with_context(|| format!("removing {}", key.display()))?;
    Ok(true)
}

/// `${XDG_DATA_HOME}/hanzo` — the daemon's socket, pid and log.
pub(crate) fn data_dir() -> Result<PathBuf> {
    let dir = dirs::data_local_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("hanzo");
    std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    Ok(dir)
}

fn log_path() -> Result<PathBuf> {
    Ok(data_dir()?.join("hanzod.log"))
}

/// The pid hanzod recorded at start, if the process is still alive.
pub(crate) fn running_pid() -> Option<i32> {
    let pid: i32 = std::fs::read_to_string(data_dir().ok()?.join("hanzod.pid"))
        .ok()?
        .trim()
        .parse()
        .ok()?;
    alive(pid).then_some(pid)
}

/// Signal 0 asks "may I signal this pid" without sending anything — the standard
/// liveness test, and the only way to tell a live daemon from a stale pidfile.
#[cfg(unix)]
fn alive(pid: i32) -> bool {
    unsafe { libc::kill(pid, 0) == 0 }
}

#[cfg(windows)]
pub(crate) fn win_alive(pid: i32) -> bool {
    if pid <= 0 {
        return false;
    }
    let filter = format!("PID eq {pid}");
    Command::new("tasklist")
        .args(["/FI", &filter, "/NH", "/FO", "CSV"])
        .output()
        .is_ok_and(|o| {
            o.status.success()
                && String::from_utf8_lossy(&o.stdout).contains(&format!(",\"{pid}\""))
        })
}

#[cfg(windows)]
fn alive(pid: i32) -> bool {
    win_alive(pid)
}

#[cfg(all(not(unix), not(windows)))]
fn alive(_pid: i32) -> bool {
    false
}

// ---- `hanzo host <verb>` ------------------------------------------------------

/// `hanzo host start` — bring the local host up (and warm nothing else: its
/// plugins still start on the first request that needs them).
pub async fn start(cfg: &Config) -> Result<()> {
    let (api, addr) = local_origin(cfg)?;
    let existed = healthy(&addr).await;
    origin(cfg).await?;
    let verb = if existed {
        "already running"
    } else {
        "started"
    };
    println!(
        "{} hanzod {} — local cloud at {}",
        "✓".green(),
        verb,
        api.cyan()
    );
    Ok(())
}

/// `hanzo host status` — is it up, where, and as which pid.
pub async fn status(cfg: &Config) -> Result<()> {
    let (api, addr) = local_origin(cfg)?;
    if healthy(&addr).await {
        match running_pid() {
            Some(pid) => println!("{} running at {} (pid {pid})", "●".green(), api.cyan()),
            // Serving, but not by a process this CLI started — a `make run`, a
            // container, whatever. Reported honestly rather than claimed.
            None => println!(
                "{} running at {} (started elsewhere)",
                "●".green(),
                api.cyan()
            ),
        }
        println!("  logs {}", log_path()?.display().to_string().dimmed());
    } else {
        println!("{} not running ({})", "○".dimmed(), api.dimmed());
    }
    Ok(())
}

/// `hanzo host stop` — SIGTERM the host, which drains zip's shutdown hooks and
/// stops every plugin child it started.
pub async fn stop(cfg: &Config) -> Result<()> {
    let (api, addr) = local_origin(cfg)?;
    // Liveness is checked BEFORE the pidfile is trusted. A pid alone is not proof:
    // pids are recycled, so a stale file plus an unlucky wrap would aim SIGTERM at
    // whatever process inherited the number. Nothing serving means nothing to stop,
    // whatever the file says.
    if !healthy(&addr).await {
        let _ = std::fs::remove_file(data_dir()?.join("hanzod.pid"));
        println!("{} not running", "○".dimmed());
        return Ok(());
    }
    let Some(pid) = running_pid() else {
        bail!("{api} is served by a host this CLI did not start — stop it where it was started");
    };
    terminate(pid)?;
    let deadline = Instant::now() + STOP_TIMEOUT;
    while alive(pid) && Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    if alive(pid) {
        bail!(
            "hanzod (pid {pid}) did not exit within {}s",
            STOP_TIMEOUT.as_secs()
        );
    }
    let _ = std::fs::remove_file(data_dir()?.join("hanzod.pid"));
    println!("{} hanzod stopped", "✓".green());
    Ok(())
}

/// SIGTERM, never SIGKILL: the host's whole shutdown contract is that the signal
/// reaches its children, and SIGKILL is precisely what leaves them orphaned.
#[cfg(unix)]
fn terminate(pid: i32) -> Result<()> {
    if unsafe { libc::kill(pid, libc::SIGTERM) } != 0 {
        bail!("signalling pid {pid}: {}", std::io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(windows)]
fn terminate(pid: i32) -> Result<()> {
    let status = Command::new("taskkill")
        .args(["/PID", &pid.to_string(), "/T", "/F"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .with_context(|| format!("terminating local cloud host pid {pid}"))?;
    if !status.success() && alive(pid) {
        bail!("taskkill failed for pid {pid} ({status})");
    }
    Ok(())
}

#[cfg(windows)]
pub(crate) fn stop_recorded_pid() -> Result<Option<i32>> {
    let Some(pid) = running_pid() else {
        return Ok(None);
    };
    terminate(pid)?;
    let _ = std::fs::remove_file(data_dir()?.join("hanzod.pid"));
    Ok(Some(pid))
}

#[cfg(all(not(unix), not(windows)))]
fn terminate(_pid: i32) -> Result<()> {
    bail!("the local cloud host is a unix daemon")
}

/// The active network's api origin, refused unless it is local — `hanzo host` is
/// about the LOCAL host, and silently operating on `mainnet` would be a trap.
/// Returns the origin as written AND the `host:port` it resolves to: the first is
/// what a human is shown, the second is what a probe dials, and deriving the
/// second twice is how they drift.
fn local_origin(cfg: &Config) -> Result<(String, String)> {
    let api = network::active(cfg).api.trim_end_matches('/').to_string();
    let Some(addr) = loopback_addr(&api) else {
        bail!(
            "the active network points at {api}, which is not local — \
             run `hanzo network use local` to work against a local cloud host"
        );
    };
    Ok((api, addr))
}

// ---- `hanzo host serve` -------------------------------------------------------

/// Resolve the ONE cloud binary (`HANZO_CLOUD_BIN`, then PATH), shared with
/// `hanzo chain up --with-cloud` — so "where is cloud?" is answered in exactly
/// one place.
pub fn resolve_cloud_bin() -> Option<PathBuf> {
    crate::commands::launch::resolve("HANZO_CLOUD_BIN", &["hanzo-cloud", "cloud"])
}

/// `hanzo host serve [service] [-- args…]` — the Hanzo Cloud API in the
/// FOREGROUND: the whole API (`cloud`, the default) or one subsystem alone
/// (iam | kms | gateway | storage | pubsub). The service name is the Go
/// binary's own subcommand, and the binary is the authority on which names it
/// serves — an unknown one is its error, not a guess here. We never BUILD it
/// (CI/CD does). This is what `hanzo up` used to do; `up` now boots k3s, and
/// the old spelling forwards here for one release.
pub async fn serve(service: String, passthrough: Vec<String>) -> Result<()> {
    let bin = resolve_cloud_bin().ok_or_else(|| {
        anyhow!(
            "cloud binary not found. Set HANZO_CLOUD_BIN=/path/to/hanzo-cloud or put \
             `hanzo-cloud` on PATH (we do not build it here — CI/CD does)."
        )
    })?;
    if service == "cloud" {
        println!("{} running the Hanzo Cloud API", "→".cyan());
    } else {
        println!(
            "{} running the {} service",
            "→".cyan(),
            service.cyan().bold()
        );
    }
    let mut argv = vec![service];
    argv.extend(passthrough);
    crate::commands::launch::exec(&bin, &argv)
}

/// Tell hanzod where the cloud is when something other than its own child serves
/// it (`hanzo up`'s k3s VM publishes the cloud on a private loopback port).
/// `None` withdraws the registration, and hanzod goes back to starting its own
/// host on demand. Unix only: the registration rides the daemon's socket.
#[cfg(unix)]
pub async fn register_upstream(addr: Option<&str>) -> Result<()> {
    ensure("").await?;
    let sock = zap_socket()?;
    let (method, body) = match addr {
        Some(a) => (Method::PUT, Some(serde_json::json!({ "addr": a }))),
        None => (Method::DELETE, None),
    };
    let r = crate::zap::send(&sock, &method, "/v1/hanzod/upstream", "", body.as_ref()).await?;
    if !(200..300).contains(&r.status) {
        bail!(
            "hanzod refused the cloud upstream ({}): {}",
            r.status,
            r.body
        );
    }
    Ok(())
}

#[cfg(not(unix))]
pub async fn register_upstream(_addr: Option<&str>) -> Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loopback_is_recognised_by_every_spelling() {
        assert_eq!(
            loopback_addr("http://localhost:3690").as_deref(),
            Some("127.0.0.1:3690")
        );
        assert_eq!(
            loopback_addr("http://127.0.0.1:3690").as_deref(),
            Some("127.0.0.1:3690")
        );
        // The host binds tcp4, so a v6 spelling still resolves to the v4 loopback.
        assert_eq!(
            loopback_addr("http://[::1]:3690").as_deref(),
            Some("127.0.0.1:3690")
        );
    }

    #[test]
    fn a_default_port_is_filled_in() {
        assert_eq!(
            loopback_addr("http://localhost").as_deref(),
            Some("127.0.0.1:80")
        );
    }

    fn scratch(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "hanzo-host-{tag}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// An install holding a key of its own is left alone; one holding the
    /// shared key loses its store and key, store first, exactly once.
    #[test]
    fn only_a_shared_install_is_retired() {
        let dir = scratch("retire");
        std::fs::create_dir_all(dir.join("data").join("kms")).unwrap();
        std::fs::write(dir.join("data").join("kms").join("org.db"), b"sealed").unwrap();
        std::fs::write(dir.join("master.key"), key()).unwrap();

        assert!(!retire(&dir, shared).unwrap());
        assert!(dir.join("master.key").exists() && dir.join("data").exists());

        assert!(retire(&dir, |_| true).unwrap());
        assert!(!dir.join("master.key").exists());
        assert!(!dir.join("data").exists());
        assert!(!retire(&dir, |_| true).unwrap(), "nothing left to retire");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn remote_origins_start_nothing() {
        // The predicate that gates every spawn: a remote origin must never match,
        // or `hanzo` against mainnet would try to boot a cloud on the laptop.
        assert_eq!(loopback_addr("https://api.hanzo.ai"), None);
        assert_eq!(loopback_addr("https://localhost.attacker.example"), None);
        assert_eq!(loopback_addr("not a url"), None);
    }

    #[test]
    fn classifies_unified_vs_legacy_chain_version_output() {
        // Unified daemon builds (0.2.6+ and 0.5.0+) are accepted.
        assert_eq!(
            classify_version_output(
                "hanzod 0.5.0 (dev=hanzo-gen harness=hanzo-gen/0.2.1 chain=steward engine=managed)\n",
                "",
                true,
            ),
            None
        );
        assert_eq!(classify_version_output("hanzod 0.2.7\n", "", true), None);
        assert_eq!(
            classify_version_output("hanzod daemon 0.3.0\n", "", true),
            None
        );

        // Stale v0.2.5 release (the old L1 validator) is flagged.
        let v025 = classify_version_output("hanzod 0.2.5\n", "", true)
            .expect("v0.2.5 must be detected as legacy validator");
        assert!(v025.contains("0.2.5"), "{v025}");

        // Explicit hanzo-chain or --committee errors are flagged.
        let chain = classify_version_output("hanzo-chain 0.2.6\n", "", true)
            .expect("hanzo-chain must be detected");
        assert!(chain.contains("hanzo-chain"), "{chain}");

        let committee = classify_version_output("", "no validators: --committee <file>\n", false)
            .expect("--committee error must be detected");
        assert!(committee.contains("--committee"), "{committee}");

        // Unrelated binary that mentions neither hanzod nor daemon is flagged.
        let other = classify_version_output("some-other-bin 1.0.0\n", "", true)
            .expect("unrelated binary must be flagged");
        assert!(other.contains("some-other-bin"), "{other}");
    }

    #[test]
    fn start_failure_surfaces_legacy_chain_fix_and_log_tail() {
        let dir = scratch("start-fail");
        let bin = dir.join("hanzod");
        let log = dir.join("hanzod.log");

        // Legacy chain validator signature produces actionable upgrade/build fix.
        std::fs::write(&log, "hanzod: no validators: --committee <file>\n").unwrap();
        let err = format_start_failure(&bin, &log, "hanzod exited (exit status: 1)").to_string();
        assert!(err.contains("legacy L1 chain validator"), "{err}");
        assert!(err.contains("hanzo-chain"), "{err}");
        assert!(err.contains("hanzo upgrade hanzod"), "{err}");
        assert!(err.contains("hanzoai/node"), "{err}");
        assert!(err.contains("--committee"), "{err}");

        // Non-validator error includes only the last 8 non-empty log lines.
        let lines = (1..=12)
            .map(|i| format!("line-{i}\n\n"))
            .collect::<String>();
        std::fs::write(&log, lines).unwrap();
        let err = format_start_failure(&bin, &log, "hanzod exited (exit status: 2)").to_string();
        assert!(!err.contains("legacy L1 chain validator"), "{err}");
        assert!(!err.contains("line-4"), "{err}");
        assert!(err.contains("line-5"), "{err}");
        assert!(err.contains("line-12"), "{err}");

        // Missing log falls back cleanly without panicking.
        std::fs::remove_file(&log).unwrap();
        let err = format_start_failure(&bin, &log, "hanzod did not answer within 10s").to_string();
        assert!(err.contains("hanzod did not answer within 10s"), "{err}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn legacy_chain_binary_probe_rejects_fake_validator_script() {
        use std::os::unix::fs::PermissionsExt;
        let dir = scratch("probe");
        let fake_validator = dir.join("hanzod-old");
        std::fs::write(
            &fake_validator,
            "#!/bin/sh\necho 'no validators: --committee <file>' >&2\nexit 1\n",
        )
        .unwrap();
        std::fs::set_permissions(&fake_validator, std::fs::Permissions::from_mode(0o755)).unwrap();
        let reason = is_legacy_chain_binary(&fake_validator)
            .expect("validator script must be flagged before spawn");
        assert!(reason.contains("--committee"), "{reason}");

        let fake_daemon = dir.join("hanzod-new");
        std::fs::write(
            &fake_daemon,
            "#!/bin/sh\necho 'hanzod 0.5.0 (dev=hanzo-gen harness=hanzo-gen/0.2.1 chain=steward engine=managed)'\n",
        )
        .unwrap();
        std::fs::set_permissions(&fake_daemon, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(is_legacy_chain_binary(&fake_daemon), None);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
