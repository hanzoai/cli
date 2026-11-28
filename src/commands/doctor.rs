//! Read-only installation checks; release verification downloads into a temporary directory.
use anyhow::{bail, Context, Result};
use hanzo_client::Method;
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeSet,
    io::{ErrorKind, Read},
    net::{TcpListener, TcpStream, ToSocketAddrs},
    path::{Path, PathBuf},
    time::Duration,
};
use tokio::process::Command;

use crate::{
    commands::product::Seam,
    config::Config,
    iam::{paths::DEFAULT_BRAND, store},
};

const TOOLS: &[(&str, &str, &str, &str)] = &[
    ("hanzo", "hanzo", "hanzoai/cli", ""),
    ("dev", "dev", "hanzoai/dev", ""),
    (
        "hanzo-gen",
        "gen",
        "hanzoai/gen",
        ".github/workflows/release.yml",
    ),
    ("hanzo-mcp", "hanzo-mcp", "hanzoai/mcp", ""),
    ("hanzo-zet", "zt", "hanzozt/zt", ""),
];

async fn output(command: &mut Command, seconds: u64) -> Result<std::process::Output> {
    command.kill_on_drop(true);
    tokio::time::timeout(Duration::from_secs(seconds), command.output())
        .await
        .context("command timed out")?
        .context("could not execute command")
}

fn digest(path: &Path) -> Result<Vec<u8>> {
    let mut file = std::fs::File::open(path)?;
    let mut hash = Sha256::new();
    let mut bytes = [0; 65536];
    loop {
        let n = file.read(&mut bytes)?;
        if n == 0 {
            break;
        }
        hash.update(&bytes[..n]);
    }
    Ok(hash.finalize().to_vec())
}

async fn verify(path: &Path, binary: &str, repo: &str, signer: &str) -> Result<bool> {
    let dir = tempfile::tempdir()?;
    let script = dir.path().join("install.sh");
    std::fs::write(&script, include_str!("../../install.sh"))?;
    let bin = dir.path().join("bin");
    let result = output(
        Command::new("sh")
            .arg(&script)
            .env("HANZO_INSTALL_REPO", repo)
            .env("HANZO_INSTALL_BIN", binary)
            .env("HANZO_INSTALL_ALIAS", "")
            .env("HANZO_INSTALL_PREFIX", &bin)
            .env("HANZO_INSTALL_SIGNER", signer)
            .env("HANZO_INSTALL_STRICT", "1")
            .env_remove("HANZO_VERSION"),
        180,
    )
    .await?;
    if !result.status.success() {
        bail!("release provenance could not be verified; check GitHub CLI, login and release attestations with the strict installer");
    }
    let filename = format!("{binary}{}", std::env::consts::EXE_SUFFIX);
    Ok(digest(path)? == digest(&bin.join(filename))?)
}

pub async fn run(
    cfg: &mut Config,
    verify_releases: bool,
    offline: bool,
    share: bool,
) -> Result<()> {
    println!("Hanzo installation diagnostics");
    let mut failures = 0;
    for &(name, legacy, repo, signer) in TOOLS {
        let path = which::which(name).or_else(|_| which::which(legacy));
        let Ok(path) = path else {
            println!("WARN {name}: not installed; run the Hanzo installer");
            continue;
        };
        match output(Command::new(&path).arg("--version"), 5).await {
            Ok(result) if result.status.success() && !result.stdout.is_empty() => {
                let text = String::from_utf8_lossy(&result.stdout);
                let version = text.lines().next().unwrap_or("no version output");
                println!("OK   {name}: {version} — {}", path.display());
            }
            _ => {
                println!("FAIL {name}: cannot run {} --version", path.display());
                failures += 1;
            }
        }
        if name != legacy {
            if let Ok(other) = which::which(legacy) {
                if std::fs::canonicalize(&other).ok() != std::fs::canonicalize(&path).ok() {
                    println!(
                        "WARN {legacy}: a separate executable at {}; check PATH precedence",
                        other.display()
                    );
                }
            }
        }
        if verify_releases {
            match verify(&path, legacy, repo, signer).await {
                Ok(true) => println!("OK   {name}: matches the checksummed, provenance-verified current release"),
                Ok(false) => println!("WARN {name}: differs from the current release; it may be an older version or a local build"),
                Err(err) => { println!("FAIL {name}: {err}"); failures += 1; }
            }
        }
    }
    if !verify_releases {
        println!("INFO Release integrity not checked; use hanzo doctor --verify (downloads current releases)");
    }

    if let Ok(mcp_bin) = crate::commands::mcp::resolve_binary() {
        if let Ok(result) = output(Command::new(&mcp_bin).args(["doctor", "--json"]), 8).await {
            if let Ok(report) = serde_json::from_slice::<serde_json::Value>(&result.stdout) {
                for line in summarize_mcp_doctor(&report) {
                    println!("{line}");
                }
            }
        }
    }

    for line in check_machine_link_alignment(cfg) {
        println!("{line}");
    }

    if let Ok(local_diag) = crate::commands::lifecycle::doctor_checks(cfg).await {
        println!("Local 3-layer lifecycle diagnostics");
        for check in &local_diag.checks {
            let tag = match check.status.to_ascii_lowercase().as_str() {
                "ok" | "healthy" => "OK  ",
                "warn" | "warning" => "WARN",
                "fail" | "failed" | "error" => {
                    failures += 1;
                    "FAIL"
                }
                _ => "INFO",
            };
            println!(
                "{tag} [{}] {}: {}",
                check.layer, check.name, check.detail
            );
        }
        if let Some(summary) = local_diag.summary.as_deref().filter(|s| !s.is_empty()) {
            println!("INFO Lifecycle summary: {summary}");
        }
    }
    let runtime = crate::commands::up::diagnose::checks().await;
    if !runtime.is_empty() {
        println!("Local runtime: resources, image and configuration");
        for check in &runtime {
            let tag = match check.status {
                "ok" => "OK  ",
                "warn" => "WARN",
                "fail" => {
                    failures += 1;
                    "FAIL"
                }
                _ => "INFO",
            };
            println!("{tag} {}: {}", check.name, check.detail);
        }
    }

    for line in run_local_host_diagnostics().await {
        if line.status == "FAIL" {
            failures += 1;
        }
        println!("{}", line.format_line());
    }

    if offline {
        println!("INFO Authentication not checked in offline mode");
    } else {
        let checked = tokio::time::timeout(Duration::from_secs(15), async {
            let seam = Seam::open(cfg, crate::iam::paths::DEFAULT_BRAND).await?;
            seam.send(Method::GET, "/v1/usage/summary", &[], None).await
        })
        .await;
        match checked {
            Ok(Ok(reply))
                if reply.status == 200
                    && reply
                        .body
                        .pointer("/scope/org")
                        .and_then(serde_json::Value::as_str)
                        .is_some_and(|org| !org.is_empty()) =>
            {
                println!("OK   Authentication: API accepted the active account")
            }
            Ok(Ok(reply)) => println!(
                "WARN Authentication: API returned {}; check hanzo auth show",
                reply.status
            ),
            Ok(Err(_)) => println!(
                "WARN Authentication unavailable; check hanzo auth show or hanzo auth login"
            ),
            Err(_) => println!("WARN Authentication check timed out"),
        }
    }
    if let Ok(program) = which::which("nvidia-smi") {
        match output(
            Command::new(program)
                .args(["--query-gpu=name,driver_version", "--format=csv,noheader"]),
            5,
        )
        .await
        {
            Ok(result) if result.status.success() => println!(
                "OK   NVIDIA GPU: {}",
                String::from_utf8_lossy(&result.stdout).trim()
            ),
            _ => println!(
                "WARN NVIDIA driver probe failed; cloud coding does not require a local GPU"
            ),
        }
    } else if which::which("rocminfo").is_ok() {
        match output(&mut Command::new("rocminfo"), 5).await {
            Ok(result) if result.status.success() => println!("OK   ROCm runtime responds"),
            _ => println!("WARN ROCm runtime probe failed"),
        }
    } else {
        println!("INFO No NVIDIA/ROCm runtime detected; local GPU capability is unverified and cloud coding remains available");
    }
    if share {
        share_report(&[]).await?;
    }
    if failures != 0 {
        bail!("{failures} installation checks failed");
    }
    Ok(())
}

pub async fn share_report(args: &[String]) -> Result<()> {
    let dev_bin = which::which("dev").context(
        "the Hanzo agent engine is not installed; run: curl -fsSL https://hanzo.sh | sh",
    )?;
    let status = Command::new(dev_bin)
        .arg("report")
        .args(args)
        .status()
        .await
        .context("failed to run `dev report`")?;
    if !status.success() {
        bail!("`dev report` exited with status {status}");
    }
    Ok(())
}

/// Format a concise summary of `hanzo-mcp doctor --json` covering native computer
/// control, MCP client wiring, and ZAP/browser extension connectivity.
pub(crate) fn summarize_mcp_doctor(report: &serde_json::Value) -> Vec<String> {
    let mut lines = Vec::new();
    if let Some(comp) = report.get("computer") {
        let native = comp
            .get("native_available")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false);
        let ax = comp.get("accessibility_granted").and_then(serde_json::Value::as_bool);
        let screen = comp
            .get("screen_recording_granted")
            .and_then(serde_json::Value::as_bool);
        let detail = comp
            .get("detail")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("unknown");
        let tag = if native && ax != Some(false) && screen != Some(false) {
            "OK  "
        } else {
            "WARN"
        };
        lines.push(format!("{tag} Computer Control: {detail}"));
    }
    if let Some(clients) = report.get("clients").and_then(serde_json::Value::as_array) {
        let total = clients.len();
        let registered = clients
            .iter()
            .filter(|c| c.get("registered").and_then(serde_json::Value::as_bool) == Some(true))
            .count();
        let computer_ready = clients
            .iter()
            .filter(|c| c.get("computer_enabled").and_then(serde_json::Value::as_bool) == Some(true))
            .count();
        let missing_computer: Vec<&str> = clients
            .iter()
            .filter(|c| {
                c.get("registered").and_then(serde_json::Value::as_bool) == Some(true)
                    && c.get("computer_enabled").and_then(serde_json::Value::as_bool) == Some(false)
            })
            .filter_map(|c| c.get("client").and_then(serde_json::Value::as_str))
            .collect();
        if !missing_computer.is_empty() {
            lines.push(format!(
                "WARN MCP Clients: {registered}/{total} registered, but {} lack native computer control — run `hanzo mcp install all`",
                missing_computer.join(", ")
            ));
        } else if registered == 0 {
            lines.push("WARN MCP Clients: none registered — run `hanzo mcp install all`".into());
        } else {
            lines.push(format!(
                "OK   MCP Clients: {registered}/{total} registered ({computer_ready} with native computer control)"
            ));
        }
    }
    if let Some(ext) = report.get("extension") {
        let connected = ext
            .get("connected")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false);
        if connected {
            let tabs = ext
                .get("tab_count")
                .and_then(serde_json::Value::as_u64)
                .map(|n| format!("{n} tabs"))
                .unwrap_or_else(|| "connected".into());
            lines.push(format!("OK   Browser Extension (ZAP): {tabs}"));
        } else {
            let hint = ext
                .pointer("/diagnostic/hint")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("open Chrome with the Hanzo Browser Extension or run `hanzo mcp doctor --fix`");
            lines.push(format!("INFO Browser Extension (ZAP): not connected — {hint}"));
        }
    }
    lines
}

/// Detect token expiry or org/auth drift between active CLI identity and local
/// background machine-link services (`beat` / `hanzod`), so a cross-org miss or
/// expired beat credential surfaces explicitly instead of looking like a deleted machine.
fn check_machine_link_alignment(cfg: &Config) -> Vec<String> {
    let active_id = store::active(cfg, DEFAULT_BRAND);
    let active_org = cfg
        .org
        .as_deref()
        .map(str::to_string)
        .or_else(|| active_id.as_ref().map(|id| id.owner.clone()));

    let mut lines = Vec::new();
    if let Some(ref id) = active_id {
        if let Ok(Some(tok)) = store::token_for(cfg, DEFAULT_BRAND, id) {
            if store::spent(&tok.access_token) {
                lines.push(format!(
                    "WARN Machine Link: active identity {id} has an expired access token — run `hanzo auth login` and restart beat/hanzod"
                ));
            }
        }
    }

    let Some(home) = dirs::home_dir() else {
        return lines;
    };
    let log_candidates = [
        home.join("Library/Logs/Hanzo/beat.err.log"),
        home.join("Library/Logs/Hanzo/beat.log"),
        home.join("Library/Logs/Hanzo/hanzod.err.log"),
        home.join(".hanzo/logs/beat.err.log"),
        home.join(".hanzo/logs/hanzod.err.log"),
    ];
    let mut combined_tail = String::new();
    for path in &log_candidates {
        if let Ok(raw) = std::fs::read_to_string(path) {
            let start = raw.len().saturating_sub(4096);
            combined_tail.push_str(&raw[start..]);
            combined_tail.push('\n');
        }
    }
    lines.extend(diagnose_beat_log(
        active_id.as_ref().map(|id| id.to_string()).as_deref(),
        active_org.as_deref(),
        &combined_tail,
    ));
    lines
}

pub(crate) fn diagnose_beat_log(
    active_identity: Option<&str>,
    active_org: Option<&str>,
    log_tail: &str,
) -> Vec<String> {
    let mut out = Vec::new();
    if log_tail.trim().is_empty() {
        return out;
    }
    // Look for "could not refresh the <owner>/<name> credential" or "401" / "Unauthorized"
    let mut beat_identity: Option<String> = None;
    let mut saw_auth_failure = false;
    for line in log_tail.lines().rev() {
        let lower = line.to_lowercase();
        if lower.contains("401")
            || lower.contains("unauthorized")
            || lower.contains("could not refresh the ")
            || lower.contains("session has ended")
        {
            saw_auth_failure = true;
        }
        if beat_identity.is_none() {
            if let Some(idx) = line.find("could not refresh the ") {
                let rest = &line[idx + "could not refresh the ".len()..];
                if let Some(tok) = rest.split_whitespace().next() {
                    if tok.contains('/') {
                        beat_identity = Some(tok.to_string());
                    }
                }
            }
        }
    }
    if let (Some(beat_id), Some(active_org)) = (beat_identity.as_deref(), active_org) {
        if let Some((beat_org, _)) = beat_id.split_once('/') {
            if beat_org != active_org {
                out.push(format!(
                    "WARN Machine Link: background beat is running under `{beat_id}` (org `{beat_org}`), which differs from active org `{active_org}` ({}) — machines only appear in the org they beat into; sign in to `{active_org}` and restart beat/hanzod",
                    active_identity.unwrap_or(active_org),
                ));
                return out;
            }
        }
    }
    if saw_auth_failure {
        #[cfg(target_os = "macos")]
        let restart = "launchctl kickstart -k gui/$(id -u)/com.ai.hanzo.beat gui/$(id -u)/ai.hanzo.hanzod";
        #[cfg(not(target_os = "macos"))]
        let restart = "restart your hanzo beat / hanzod service";
        out.push(format!(
            "WARN Machine Link: recent beat/hanzod logs show auth failure (401 / refresh error) — run `hanzo auth login` then `{restart}`"
        ));
    }
    out
}

pub(crate) const DEFAULT_LOCAL_PORTS: &[(u16, &str)] = &[
    (8080, "Hanzo Cloud"),
    (9090, "Metrics / Prometheus"),
    (1234, "Hanzo Engine"),
    (8000, "IAM / Gateway"),
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DiagLine {
    pub(crate) status: &'static str,
    pub(crate) label: String,
    pub(crate) detail: String,
}

impl DiagLine {
    fn new(status: &'static str, label: impl Into<String>, detail: impl Into<String>) -> Self {
        Self {
            status,
            label: label.into(),
            detail: detail.into(),
        }
    }

    pub(crate) fn format_line(&self) -> String {
        format!("{:<4} {}: {}", self.status, self.label, self.detail)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SocketCandidate {
    pub(crate) label: String,
    pub(crate) socket_path: PathBuf,
    pub(crate) pid_path: Option<PathBuf>,
}

/// Run the local host checks for kubeconfigs, default local port collisions, and
/// orphaned daemon/VM runtime sockets.
async fn run_local_host_diagnostics() -> Vec<DiagLine> {
    let mut out = Vec::new();
    for path in default_kubeconfig_paths() {
        if let Some(line) = diagnose_kubeconfig_path(&path) {
            out.push(line);
        }
    }

    let mut active_ports = BTreeSet::new();
    if crate::commands::lifecycle::resources::local_engine_healthy().await {
        active_ports.insert(1234);
    }
    if let Some(kube) = crate::commands::lifecycle::resources::local_kube_client() {
        if kube
            .cloud_replicas()
            .await
            .ok()
            .flatten()
            .is_some_and(|n| n > 0)
        {
            active_ports.insert(8080);
        }
    }
    out.extend(diagnose_port_collisions(DEFAULT_LOCAL_PORTS, &active_ports));
    out.extend(diagnose_runtime_sockets(&default_socket_candidates()));
    out
}

fn default_kubeconfig_paths() -> Vec<PathBuf> {
    let mut raw = Vec::new();
    if let Some(home) = dirs::home_dir() {
        raw.push(home.join(".kube").join("hanzo.yaml"));
    }
    if let Ok(kc) = crate::commands::up::kubeconfig_path() {
        raw.push(kc);
    }
    let mut seen = BTreeSet::new();
    let mut unique = Vec::new();
    for path in raw {
        let key = std::fs::canonicalize(&path).unwrap_or_else(|_| path.clone());
        if seen.insert(key) {
            unique.push(path);
        }
    }
    unique
}

fn default_socket_candidates() -> Vec<SocketCandidate> {
    let mut out = Vec::new();
    if let Ok(data) = crate::commands::host::data_dir() {
        out.push(SocketCandidate {
            label: "hanzod".into(),
            socket_path: data.join("hanzod.zap.sock"),
            pid_path: Some(data.join("hanzod.pid")),
        });
    }
    if let Some(home) = dirs::home_dir() {
        let up_dir = home.join(".hanzo").join("up");
        out.push(SocketCandidate {
            label: "hanzo-up VM".into(),
            socket_path: up_dir.join("vm.sock"),
            pid_path: Some(up_dir.join("vm.pid")),
        });
        out.push(SocketCandidate {
            label: "hanzo-up supervisor".into(),
            socket_path: up_dir.join("supervisor.sock"),
            pid_path: Some(up_dir.join("supervisor.pid")),
        });
        let vm_dir = home.join(".hanzo").join("vm");
        out.push(SocketCandidate {
            label: "hanzo-vm".into(),
            socket_path: vm_dir.join("hanzo-vm.sock"),
            pid_path: Some(vm_dir.join("hanzo-vm.pid")),
        });
    }
    out
}

fn parse_http_host_port(server: &str) -> Option<(String, u16)> {
    let (default_port, rest) = if let Some(s) = server.strip_prefix("https://") {
        (443u16, s)
    } else if let Some(s) = server.strip_prefix("http://") {
        (80u16, s)
    } else {
        return None;
    };
    let authority = rest.split('/').next().unwrap_or("").trim();
    if authority.is_empty() {
        return None;
    }
    if let Some(bracketed) = authority.strip_prefix('[') {
        let (host, tail) = bracketed.split_once(']')?;
        if host.is_empty() {
            return None;
        }
        let port = if let Some(p) = tail.strip_prefix(':') {
            p.parse::<u16>().ok()?
        } else if tail.is_empty() {
            default_port
        } else {
            return None;
        };
        return Some((host.to_string(), port));
    }
    if let Some((host, port_str)) = authority.rsplit_once(':') {
        if host.is_empty() {
            return None;
        }
        let port = port_str.parse::<u16>().ok()?;
        Some((host.to_string(), port))
    } else {
        Some((authority.to_string(), default_port))
    }
}

/// Validate a kubeconfig file when present on disk and detect malformed YAML or
/// unreachable loopback endpoints.
pub(crate) fn diagnose_kubeconfig_path(path: &Path) -> Option<DiagLine> {
    if !path.exists() {
        return None;
    }
    let label = format!("Kubeconfig ({})", path.display());
    let raw = match std::fs::read_to_string(path) {
        Ok(s) if !s.trim().is_empty() => s,
        Ok(_) => {
            return Some(DiagLine::new(
                "FAIL",
                label,
                format!(
                    "malformed kubeconfig (empty file) — regenerate with: rm -f {} && hanzo start",
                    path.display()
                ),
            ));
        }
        Err(e) => {
            return Some(DiagLine::new(
                "FAIL",
                label,
                format!(
                    "unreadable kubeconfig ({e}) — regenerate with: rm -f {} && hanzo start",
                    path.display()
                ),
            ));
        }
    };

    let doc: serde_norway::Value = match serde_norway::from_str(&raw) {
        Ok(v) => v,
        Err(e) => {
            return Some(DiagLine::new(
                "FAIL",
                label,
                format!(
                    "malformed YAML ({e}) — regenerate with: rm -f {} && hanzo start",
                    path.display()
                ),
            ));
        }
    };

    let has_contexts = doc
        .get("contexts")
        .and_then(|v| v.as_sequence())
        .is_some_and(|s| !s.is_empty());
    let has_users = doc
        .get("users")
        .and_then(|v| v.as_sequence())
        .is_some_and(|s| !s.is_empty());
    let server = doc
        .get("clusters")
        .and_then(|v| v.as_sequence())
        .and_then(|s| s.first())
        .and_then(|c| c.get("cluster"))
        .and_then(|c| c.get("server"))
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty());

    let (Some(server), true, true) = (server, has_contexts, has_users) else {
        return Some(DiagLine::new(
            "FAIL",
            label,
            format!(
                "malformed kubeconfig (missing clusters/contexts/users/server) — regenerate with: rm -f {} && hanzo start",
                path.display()
            ),
        ));
    };

    let Some((host, port)) = parse_http_host_port(server) else {
        return Some(DiagLine::new(
            "FAIL",
            label,
            format!(
                "malformed cluster server URL `{server}` — regenerate with: rm -f {} && hanzo start",
                path.display()
            ),
        ));
    };

    let is_loopback = matches!(host.as_str(), "127.0.0.1" | "localhost" | "::1");
    if is_loopback {
        let reachable = (host.as_str(), port)
            .to_socket_addrs()
            .ok()
            .is_some_and(|addrs| {
                addrs
                    .into_iter()
                    .any(|addr| TcpStream::connect_timeout(&addr, Duration::from_millis(250)).is_ok())
            });
        if !reachable {
            return Some(DiagLine::new(
                "WARN",
                label,
                format!(
                    "stale cluster endpoint {server} is unreachable — refresh with: hanzo start (or remove stale file: rm -f {})",
                    path.display()
                ),
            ));
        }
    }

    Some(DiagLine::new(
        "OK  ",
        label,
        format!("valid cluster endpoint {server}"),
    ))
}

/// Probe default local ports on `127.0.0.1` unless owned by an already-active
/// Hanzo service, printing an exact platform inspection command on conflicts.
pub(crate) fn diagnose_port_collisions(
    ports: &[(u16, &str)],
    active_hanzo_ports: &BTreeSet<u16>,
) -> Vec<DiagLine> {
    let mut out = Vec::new();
    for &(port, service) in ports {
        if active_hanzo_ports.contains(&port) {
            continue;
        }
        if let Err(err) = TcpListener::bind(("127.0.0.1", port)) {
            if err.kind() == ErrorKind::AddrInUse {
                #[cfg(unix)]
                let inspect = format!(
                    "lsof -nP -iTCP:{port} -sTCP:LISTEN (or free with: kill $(lsof -tiTCP:{port} -sTCP:LISTEN))"
                );
                #[cfg(windows)]
                let inspect = format!("netstat -ano | findstr :{port}");
                #[cfg(all(not(unix), not(windows)))]
                let inspect = format!("ss -ltnp | grep :{port}");
                out.push(DiagLine::new(
                    "WARN",
                    format!("Port {port} ({service})"),
                    format!(
                        "local port 127.0.0.1:{port} is already in use — inspect/free with: {inspect}"
                    ),
                ));
            }
        }
    }
    if out.is_empty() {
        let port_list = ports
            .iter()
            .map(|(p, _)| p.to_string())
            .collect::<Vec<_>>()
            .join(", ");
        out.push(DiagLine::new(
            "OK  ",
            "Local Ports",
            format!("default ports {port_list} are free of conflicts"),
        ));
    }
    out
}

fn read_pid_file(path: &Path) -> Option<i32> {
    std::fs::read_to_string(path).ok()?.trim().parse().ok()
}

/// Detect leftover daemon/VM runtime sockets whose listener or companion pidfile
/// is no longer alive.
pub(crate) fn diagnose_runtime_sockets(candidates: &[SocketCandidate]) -> Vec<DiagLine> {
    let mut out = Vec::new();
    for cand in candidates {
        if std::fs::symlink_metadata(&cand.socket_path).is_err() {
            continue;
        }
        #[cfg(unix)]
        let sock_live = std::os::unix::net::UnixStream::connect(&cand.socket_path).is_ok();
        #[cfg(not(unix))]
        let sock_live = false;

        let pid_live = cand
            .pid_path
            .as_deref()
            .and_then(read_pid_file)
            .is_some_and(crate::commands::vm::alive);

        let label = format!("Runtime Socket ({})", cand.label);
        if sock_live || pid_live {
            out.push(DiagLine::new(
                "OK  ",
                label,
                format!("active at {}", cand.socket_path.display()),
            ));
        } else {
            out.push(DiagLine::new(
                "WARN",
                label,
                format!(
                    "orphaned socket at {} — remove stale socket with: rm -f {} && hanzo start",
                    cand.socket_path.display(),
                    cand.socket_path.display()
                ),
            ));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn integrity_comparison_reads_bytes_not_filenames() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a");
        let b = dir.path().join("b");
        std::fs::write(&a, b"release").unwrap();
        std::fs::write(&b, b"release").unwrap();
        assert_eq!(digest(&a).unwrap(), digest(&b).unwrap());
        std::fs::write(&b, b"changed").unwrap();
        assert_ne!(digest(&a).unwrap(), digest(&b).unwrap());
    }

    #[test]
    fn summarizes_mcp_doctor_report_and_flags_missing_computer() {
        let report = json!({
            "computer": {
                "native_available": true,
                "accessibility_granted": true,
                "screen_recording_granted": true,
                "detail": "native Quartz ready"
            },
            "clients": [
                {"client": "hanzo", "registered": true, "computer_enabled": true},
                {"client": "cursor", "registered": true, "computer_enabled": false}
            ],
            "extension": { "connected": true, "tab_count": 4 }
        });
        let lines = summarize_mcp_doctor(&report);
        assert!(lines.iter().any(|l| l.starts_with("OK   Computer Control:")));
        assert!(lines.iter().any(|l| l.contains("cursor lack native computer control")));
        assert!(lines.iter().any(|l| l.contains("4 tabs")));
    }

    #[test]
    fn detects_cross_org_beat_drift_and_401_auth_failure() {
        let log = "warn: could not refresh the admin/z credential (503) — sending expired token\n401 Unauthorized";
        let drift = diagnose_beat_log(Some("hanzo/z"), Some("hanzo"), log);
        assert_eq!(drift.len(), 1);
        assert!(drift[0].contains("org `admin`"));
        assert!(drift[0].contains("active org `hanzo`"));

        let same_org = diagnose_beat_log(Some("hanzo/z"), Some("hanzo"), "HTTP 401 Unauthorized");
        assert_eq!(same_org.len(), 1);
        assert!(same_org[0].contains("auth failure"));
    }

    #[test]
    fn kubeconfig_validation_distinguishes_malformed_stale_and_reachable() {
        let dir = tempfile::tempdir().unwrap();

        let missing = dir.path().join("missing.yaml");
        assert!(diagnose_kubeconfig_path(&missing).is_none());

        let broken = dir.path().join("broken.yaml");
        std::fs::write(&broken, "clusters: [not_a_cluster\n").unwrap();
        let line_broken = diagnose_kubeconfig_path(&broken).unwrap();
        assert_eq!(line_broken.status, "FAIL");
        assert!(line_broken.detail.contains("rm -f"));
        assert!(line_broken.detail.contains("hanzo start"));

        let incomplete = dir.path().join("incomplete.yaml");
        std::fs::write(
            &incomplete,
            "clusters:\n  - cluster:\n      server: https://127.0.0.1:6443\ncontexts: []\nusers: []\n",
        )
        .unwrap();
        let line_incomplete = diagnose_kubeconfig_path(&incomplete).unwrap();
        assert_eq!(line_incomplete.status, "FAIL");
        assert!(line_incomplete.detail.contains("missing"));

        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let live_port = listener.local_addr().unwrap().port();
        let healthy = dir.path().join("healthy.yaml");
        std::fs::write(
            &healthy,
            format!(
                "clusters:\n  - name: hanzo\n    cluster:\n      server: https://127.0.0.1:{live_port}\ncontexts:\n  - name: hanzo\nusers:\n  - name: hanzo\n"
            ),
        )
        .unwrap();
        let line_ok = diagnose_kubeconfig_path(&healthy).unwrap();
        assert_eq!(line_ok.status, "OK  ");
        assert!(line_ok.detail.contains(&live_port.to_string()));

        drop(listener);
        let stale = dir.path().join("stale.yaml");
        std::fs::write(
            &stale,
            format!(
                "clusters:\n  - name: hanzo\n    cluster:\n      server: https://127.0.0.1:{live_port}\ncontexts:\n  - name: hanzo\nusers:\n  - name: hanzo\n"
            ),
        )
        .unwrap();
        let line_stale = diagnose_kubeconfig_path(&stale).unwrap();
        assert_eq!(line_stale.status, "WARN");
        assert!(line_stale.detail.contains("unreachable"));
        assert!(line_stale.detail.contains("hanzo start"));
    }

    #[test]
    fn port_collision_check_reports_unowned_busy_ports_and_skips_active_hanzo_ports() {
        let busy = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let busy_port = busy.local_addr().unwrap().port();

        let free_probe = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let free_port = free_probe.local_addr().unwrap().port();
        drop(free_probe);

        let unowned = diagnose_port_collisions(
            &[(busy_port, "Test Service"), (free_port, "Free Service")],
            &BTreeSet::new(),
        );
        assert_eq!(unowned.len(), 1);
        assert_eq!(unowned[0].status, "WARN");
        assert!(unowned[0].detail.contains(&busy_port.to_string()));
        assert!(
            unowned[0].detail.contains("lsof")
                || unowned[0].detail.contains("netstat")
                || unowned[0].detail.contains("ss ")
        );

        let mut owned_set = BTreeSet::new();
        owned_set.insert(busy_port);
        let owned = diagnose_port_collisions(&[(busy_port, "Test Service")], &owned_set);
        assert_eq!(owned.len(), 1);
        assert_eq!(owned[0].status, "OK  ");
    }

    #[test]
    fn runtime_socket_check_detects_orphaned_vs_live_sockets() {
        let dir = tempfile::tempdir().unwrap();
        let absent = SocketCandidate {
            label: "absent".into(),
            socket_path: dir.path().join("absent.sock"),
            pid_path: None,
        };
        assert!(diagnose_runtime_sockets(&[absent]).is_empty());

        let stale_sock = dir.path().join("hanzod.zap.sock");
        let stale_pid = dir.path().join("hanzod.pid");
        std::fs::write(&stale_sock, b"").unwrap();
        std::fs::write(&stale_pid, "99999999").unwrap();
        let stale_out = diagnose_runtime_sockets(&[SocketCandidate {
            label: "hanzod".into(),
            socket_path: stale_sock.clone(),
            pid_path: Some(stale_pid),
        }]);
        assert_eq!(stale_out.len(), 1);
        assert_eq!(stale_out[0].status, "WARN");
        assert!(stale_out[0].detail.contains("orphaned socket"));
        assert!(stale_out[0].detail.contains("rm -f"));

        let live_sock = dir.path().join("live.sock");
        let live_pid = dir.path().join("live.pid");
        std::fs::write(&live_sock, b"").unwrap();
        std::fs::write(&live_pid, std::process::id().to_string()).unwrap();
        let live_out = diagnose_runtime_sockets(&[SocketCandidate {
            label: "live-daemon".into(),
            socket_path: live_sock,
            pid_path: Some(live_pid),
        }]);
        assert_eq!(live_out.len(), 1);
        assert_eq!(live_out[0].status, "OK  ");
    }
}
