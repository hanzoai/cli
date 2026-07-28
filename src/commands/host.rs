//! `hanzo host` — the LOCAL cloud, so every cloud command works with no server.
//!
//! Every cloud call resolves WHERE through [`origin`]. When the active network
//! points at loopback, that resolution also GUARANTEES something is listening
//! there: it reuses a running host, or starts the one `make host` built in a
//! hanzoai/cloud checkout. Nothing links Go.
//!
//! WHY the host and not the 108 app binaries: the host is a ROUTER that mounts
//! each subsystem as its own process, started on the first request that reaches
//! its prefix. Cold is ~15ms, warm ~0.5ms, and an app nobody calls costs a route
//! entry. So pointing the CLI at the host gets the whole surface for the price of
//! the commands actually run.
//!
//! WHY a persistent daemon rather than a child per command: that laziness only
//! pays off warm. Stopping the host between commands would make every call cold
//! and throw the entire benefit away. It outlives the CLI, and `hanzo host stop`
//! ends it — SIGTERM, which zip drains LIFO into every child it started.
//!
//! WHY a unix socket, and WHY ZAP: the host serves the SAME routes on two wires —
//! ZAP, the fleet's primary transport, and HTTP, a secondary view for third
//! parties who cannot speak it. The CLI is first party, so locally it speaks ZAP
//! ([`crate::zap`]) over a unix socket in the user's own state directory: no
//! HTTP grammar, no second serialization, no port on any interface. The host is
//! still told to bind loopback TCP because `cmd/host` always listens on both, but
//! nothing here dials it — a bound port is never what "running" means to us.

use anyhow::{bail, Context, Result};
use colored::*;
use reqwest::Method;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use crate::commands::network;
use crate::config::Config;

/// How long a freshly started host gets to answer `/healthz`. It boots its eager
/// subsystems first, so this is seconds, not milliseconds.
const READY_TIMEOUT: Duration = Duration::from_secs(30);

/// How long to wait on a single `/healthz` probe. Short: the question is only
/// "is someone already listening", and the answer is local either way.
const PROBE_TIMEOUT: Duration = Duration::from_millis(500);

/// Where a cloud call goes, and over which wire.
pub enum Origin {
    /// The local host's ZAP socket. ZAP is the fleet's PRIMARY transport and the
    /// CLI is first party, so locally it speaks the native wire — the HTTP view
    /// the host also serves exists for third parties, and we are not one.
    LocalZap(PathBuf),
    /// A published `api.*` origin over HTTPS. TLS terminates at the ingress and
    /// zaphttp has no handshake of its own yet, so a remote host is reached the
    /// way third parties reach it. This is the EXCEPTION, kept only until the ZAP
    /// transport carries its own session crypto.
    Http(String),
}

/// The ONE origin resolver for every cloud call. A loopback network resolves to
/// the local ZAP socket, with a host guaranteed to be listening on it by the time
/// this returns; anything else is passed through as HTTP — nothing started,
/// nothing probed.
pub async fn origin(cfg: &Config) -> Result<Origin> {
    let api = network::active(cfg).api.trim_end_matches('/').to_string();
    match loopback_addr(&api) {
        Some(addr) => {
            ensure(&addr).await?;
            Ok(Origin::LocalZap(zap_socket()?))
        }
        None => Ok(Origin::Http(api)),
    }
}

/// The local host's ZAP socket. zip reads the wire off the address SHAPE, so a
/// path binds a unix socket — which is why this is a path and not a port.
pub fn zap_socket() -> Result<PathBuf> {
    Ok(state_dir()?.join("host.zap.sock"))
}

/// The `host:port` to bind when `origin` is loopback, else `None`. The host binds
/// `tcp4`, so this always names the v4 loopback whatever spelling the URL used.
fn loopback_addr(origin: &str) -> Option<String> {
    let url = reqwest::Url::parse(origin).ok()?;
    // An IPv6 host arrives spelled with its brackets, which is not what parses as
    // an address. Everything loopback answers here — all of 127.0.0.0/8 and ::1 —
    // so the test is the address's own, not a list of spellings.
    let host = url.host_str()?.trim_start_matches('[').trim_end_matches(']');
    let local = host == "localhost"
        || host.parse::<std::net::IpAddr>().is_ok_and(|ip| ip.is_loopback());
    local.then(|| format!("127.0.0.1:{}", url.port_or_known_default().unwrap_or(80)))
}

/// True when a host answers `/healthz` on the ZAP socket. Liveness is the HOST's
/// own route — it answers while every plugin is still cold — so this asks whether
/// the router is up, never whether any subsystem is. Probed over ZAP like every
/// other local call: the CLI does not speak HTTP to the local host at all, so a
/// bound TCP port is never what "running" means here.
async fn healthy() -> bool {
    let Ok(sock) = zap_socket() else { return false };
    let probe = crate::zap::send(&sock, &Method::GET, "/healthz", "", None);
    matches!(tokio::time::timeout(PROBE_TIMEOUT, probe).await, Ok(Ok((s, _))) if s.is_success())
}

/// Reuse a running host, or start one and wait for it to listen.
async fn ensure(addr: &str) -> Result<()> {
    if healthy().await {
        return Ok(()); // already serving — never double-start
    }
    let bin = binary()?;
    let mut child = spawn(&bin, addr)?;
    let deadline = Instant::now() + READY_TIMEOUT;
    loop {
        if healthy().await {
            return Ok(());
        }
        // A host that died has already written why. Surfacing its exit beats
        // spending the whole timeout to report a generic one.
        if let Some(status) = child.try_wait().context("waiting on the local cloud host")? {
            bail!("local cloud host exited ({status}) — see {}", log_path()?.display());
        }
        if Instant::now() >= deadline {
            bail!(
                "local cloud host did not listen on {addr} within {}s — see {}",
                READY_TIMEOUT.as_secs(),
                log_path()?.display()
            );
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// Locate the host binary: `$HANZO_HOST_BIN`, else `bin/host` in the default
/// hanzoai/cloud checkout. Deliberately NOT `$PATH` — `host` there is the DNS
/// lookup tool, and running that instead would be a baffling failure. The
/// override names the BINARY rather than the checkout because the host resolves
/// its plugins as siblings of its own path, so the directory is the real unit.
fn binary() -> Result<PathBuf> {
    let bin = match std::env::var_os("HANZO_HOST_BIN") {
        Some(p) => PathBuf::from(p),
        None => default_bin(),
    };
    if !bin.is_file() {
        bail!(
            "no local cloud host at {}.\n\
             Build it with `make host` in a hanzoai/cloud checkout, then `make plugin APP=<name>`\n\
             for the subsystems you need — or set HANZO_HOST_BIN to a host binary.",
            bin.display()
        );
    }
    Ok(bin)
}

fn default_bin() -> PathBuf {
    dirs::home_dir().unwrap_or_default().join("work/hanzo/cloud/bin/host")
}

/// Start the host detached, logging to the state dir.
fn spawn(bin: &Path, addr: &str) -> Result<Child> {
    let state = state_dir()?;
    let log = std::fs::File::create(state.join("host.log"))
        .with_context(|| format!("creating {}", state.join("host.log").display()))?;

    let mut cmd = Command::new(bin);
    cmd.arg("-addr")
        .arg(addr)
        // ZAP is the host's other listener and nothing here dials it. zip reads
        // the wire off the address SHAPE, so a path binds a unix socket — which
        // keeps a second port off every interface for a purely local daemon.
        .arg("-zap")
        .arg(state.join("host.zap.sock"))
        // The host defaults to /var/lib/cloud, which a developer cannot write.
        // Local state belongs beside the rest of the CLI's.
        .env("CLOUD_DATA_DIR", state.join("data"))
        .stdin(Stdio::null())
        .stdout(Stdio::from(log.try_clone().context("duplicating the host log handle")?))
        .stderr(Stdio::from(log));
    detach(&mut cmd);

    let child = cmd.spawn().with_context(|| format!("starting {}", bin.display()))?;
    // The pid is how `stop` finds it in a LATER process. Dropping the handle here
    // does not signal the child — std never kills on drop — which is what lets the
    // daemon outlive the command that started it.
    std::fs::write(state.join("host.pid"), child.id().to_string())
        .with_context(|| format!("writing {}", state.join("host.pid").display()))?;
    Ok(child)
}

/// Put the host in its own process group so the Ctrl-C that interrupts the CLI
/// does not also kill the daemon it just started.
#[cfg(unix)]
fn detach(cmd: &mut Command) {
    use std::os::unix::process::CommandExt;
    cmd.process_group(0);
}

#[cfg(not(unix))]
fn detach(_cmd: &mut Command) {}

/// `${XDG_DATA_HOME}/hanzo/host` — the local host's pid, log, socket and data.
fn state_dir() -> Result<PathBuf> {
    let dir = dirs::data_local_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("hanzo")
        .join("host");
    std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    Ok(dir)
}

fn log_path() -> Result<PathBuf> {
    Ok(state_dir()?.join("host.log"))
}

/// The pid recorded by the last [`spawn`], if the process is still alive.
fn running_pid() -> Option<i32> {
    let pid: i32 = std::fs::read_to_string(state_dir().ok()?.join("host.pid")).ok()?.trim().parse().ok()?;
    alive(pid).then_some(pid)
}

/// Signal 0 asks "may I signal this pid" without sending anything — the standard
/// liveness test, and the only way to tell a live daemon from a stale pidfile.
#[cfg(unix)]
fn alive(pid: i32) -> bool {
    unsafe { libc::kill(pid, 0) == 0 }
}

#[cfg(not(unix))]
fn alive(_pid: i32) -> bool {
    false
}

// ---- `hanzo host <verb>` ------------------------------------------------------

/// `hanzo host start` — bring the local host up (and warm nothing else: its
/// plugins still start on the first request that needs them).
pub async fn start(cfg: &Config) -> Result<()> {
    let api = local_origin(cfg)?;
    let existed = healthy().await;
    origin(cfg).await?;
    let verb = if existed { "already running" } else { "started" };
    println!("{} local cloud host {} at {}", "✓".green(), verb, api.cyan());
    Ok(())
}

/// `hanzo host status` — is it up, where, and as which pid.
pub async fn status(cfg: &Config) -> Result<()> {
    let api = local_origin(cfg)?;
    if healthy().await {
        match running_pid() {
            Some(pid) => println!("{} running at {} (pid {pid})", "●".green(), api.cyan()),
            // Serving, but not by a process this CLI started — a `make run`, a
            // container, whatever. Reported honestly rather than claimed.
            None => println!("{} running at {} (started elsewhere)", "●".green(), api.cyan()),
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
    let api = local_origin(cfg)?;
    // Liveness is checked BEFORE the pidfile is trusted. A pid alone is not proof:
    // pids are recycled, so a stale file plus an unlucky wrap would aim SIGTERM at
    // whatever process inherited the number. Nothing serving means nothing to stop,
    // whatever the file says.
    if !healthy().await {
        let _ = std::fs::remove_file(state_dir()?.join("host.pid"));
        println!("{} not running", "○".dimmed());
        return Ok(());
    }
    let Some(pid) = running_pid() else {
        bail!("{api} is served by a host this CLI did not start — stop it where it was started");
    };
    terminate(pid)?;
    let deadline = Instant::now() + READY_TIMEOUT;
    while alive(pid) && Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    if alive(pid) {
        bail!("local cloud host (pid {pid}) did not exit within {}s", READY_TIMEOUT.as_secs());
    }
    let _ = std::fs::remove_file(state_dir()?.join("host.pid"));
    println!("{} local cloud host stopped", "✓".green());
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

#[cfg(not(unix))]
fn terminate(_pid: i32) -> Result<()> {
    bail!("the local cloud host is a unix daemon")
}

/// The active network's api origin, refused unless it is local — `hanzo host` is
/// about the LOCAL host, and silently operating on `mainnet` would be a trap.
fn local_origin(cfg: &Config) -> Result<String> {
    let api = network::active(cfg).api.trim_end_matches('/').to_string();
    if loopback_addr(&api).is_none() {
        bail!(
            "the active network points at {api}, which is not local — \
             run `hanzo network use local` to work against a local cloud host"
        );
    }
    Ok(api)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loopback_is_recognised_by_every_spelling() {
        assert_eq!(loopback_addr("http://localhost:3690").as_deref(), Some("127.0.0.1:3690"));
        assert_eq!(loopback_addr("http://127.0.0.1:3690").as_deref(), Some("127.0.0.1:3690"));
        // The host binds tcp4, so a v6 spelling still resolves to the v4 loopback.
        assert_eq!(loopback_addr("http://[::1]:3690").as_deref(), Some("127.0.0.1:3690"));
    }

    #[test]
    fn a_default_port_is_filled_in() {
        assert_eq!(loopback_addr("http://localhost").as_deref(), Some("127.0.0.1:80"));
    }

    #[test]
    fn remote_origins_start_nothing() {
        // The predicate that gates every spawn: a remote origin must never match,
        // or `hanzo` against mainnet would try to boot a cloud on the laptop.
        assert_eq!(loopback_addr("https://api.hanzo.ai"), None);
        assert_eq!(loopback_addr("https://localhost.attacker.example"), None);
        assert_eq!(loopback_addr("not a url"), None);
    }
}
