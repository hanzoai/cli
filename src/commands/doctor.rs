//! Read-only installation checks; release verification downloads into a temporary directory.
use anyhow::{bail, Context, Result};
use hanzo_client::Method;
use sha2::{Digest, Sha256};
use std::{io::Read, path::Path, time::Duration};
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
}
