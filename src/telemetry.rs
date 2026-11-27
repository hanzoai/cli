//! CLI usage telemetry — best-effort canonical Events to `/v1/event`.
//!
//! One handle, built once per run, records the dispatched command (verb, duration,
//! outcome) and flushes at process exit. Everything is fail-soft and privacy-clean:
//! the visitor is a per-install [`device_id`](hanzo_event::device_id) (never PII),
//! the bearer is the active identity's own token when signed in (the server derives
//! the tenant from it, so an event only lands in your own org), and an opt-out builds
//! a dead handle that reads no id and touches no network.

use crate::commands::network;
use crate::config::Config;
use crate::iam::paths::DEFAULT_BRAND;
use crate::iam::store;
use crate::Commands;

/// Write-only Hanzo publishable key fallback when the caller is unsigned and
/// has not opted out of telemetry (`HANZO_TELEMETRY=0` / `DO_NOT_TRACK`).
const DEFAULT_PUBLISHABLE_KEY: &str = "pk-CmfLA2K6kvsPflrS9DSkt06H_kSoQB_21sjedt6VJdc";

/// Build the telemetry handle for this run. An opt-out or any resolution error
/// yields a disabled handle. The bearer read is the NON-mutating index+vault path
/// (`store::active` then `store::token_for`), so telemetry never migrates or moves
/// the active identity as a side effect.
pub fn build(config: &Config) -> hanzo_event::Telemetry {
    if hanzo_event::opted_out() {
        return hanzo_event::Telemetry::disabled();
    }
    let api = network::active(config).api;
    let dir = dirs::data_local_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("hanzo");
    let distinct_id = hanzo_event::device_id(&dir);
    let bearer = store::active(config, DEFAULT_BRAND)
        .and_then(|id| store::token_for(config, DEFAULT_BRAND, &id).ok().flatten())
        .map(|tokens| tokens.access_token)
        .or_else(|| {
            std::env::var(hanzo_event::KEY_ENV)
                .ok()
                .filter(|s| !s.trim().is_empty())
        })
        .or_else(|| Some(DEFAULT_PUBLISHABLE_KEY.to_string()));
    hanzo_event::Telemetry::new(hanzo_event::Config {
        product: "cli".to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        api_base: api,
        distinct_id,
        bearer,
    })
}

/// Install a panic hook that forwards unhandled panics (message + file:line,
/// scrubbed of secrets and home directories) to Hanzo's central `/v1/event`
/// collector on a short bounded timeout, then invokes the previous panic hook.
/// No-op when telemetry is disabled or opted out.
pub fn install_panic_hook(telemetry: &hanzo_event::Telemetry) {
    let Some((endpoint, auth, distinct_id, product, version)) = telemetry.crash_target() else {
        return;
    };
    let prev = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        if crate::is_broken_pipe(info.payload()) {
            prev(info);
            return;
        }
        let payload = info
            .payload()
            .downcast_ref::<&str>()
            .map(|s| (*s).to_string())
            .or_else(|| info.payload().downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "panic".to_string());
        let loc = info
            .location()
            .map(|l| format!("{}:{}:{}", l.file(), l.line(), l.column()));
        let t = hanzo_event::Telemetry::new(hanzo_event::Config {
            product: product.clone(),
            version: version.clone(),
            api_base: endpoint.trim_end_matches("/v1/event").to_string(),
            distinct_id: distinct_id.clone(),
            bearer: Some(auth.clone()),
        });
        let ev = t.build_error_event("Panic", &payload, loc.as_deref(), false, None);
        let endpoint = endpoint.clone();
        let auth = auth.clone();
        let _ = std::thread::spawn(move || {
            if let Ok(rt) = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                rt.block_on(async move {
                    if let Ok(client) = reqwest::Client::builder()
                        .timeout(std::time::Duration::from_millis(500))
                        .build()
                    {
                        let _ = client
                            .post(&endpoint)
                            .bearer_auth(&auth)
                            .json(&vec![ev])
                            .send()
                            .await;
                    }
                });
            }
        })
        .join();
        prev(info);
    }));
}

/// The fixed dispatch label for a top-level command — the verb ONLY, never argv,
/// paths, or user input, so the recorded property can carry no PII.
pub fn label(command: &Commands) -> &'static str {
    match command {
        Commands::Auth { .. } => "auth",
        Commands::Bot { .. } => "bot",
        // `hanzo login` and `hanzo auth login` are one command reached two ways,
        // so they report as one label — a split here would read as two products.
        Commands::Login { .. } | Commands::Whoami { .. } => "auth",
        // The coding session reports as `dev` (a bare `hanzo` too, because it IS
        // that command); `code` is the code index.
        Commands::Code => "code",
        Commands::Daemon(..) => "daemon",
        Commands::Dev(..) => "dev",
        Commands::Desktop(..) => "desktop",
        Commands::Sources(..) => "sources",
        Commands::Prompt(..) => "prompt",
        Commands::Config { .. } => "config",
        Commands::Engine { .. } | Commands::Pull { .. } => "engine",
        Commands::Graph { .. } => "graph",
        Commands::Link { .. } => "link",
        Commands::Runner { .. } => "runner",
        Commands::Scan { .. } => "scan",
        Commands::Serve(..) => "serve",
        Commands::Build(..) => "build",
        Commands::Channels(..) => "channels",
        Commands::Connect(..) => "connect",
        Commands::Post(..) => "post",
        Commands::Vm { .. } => "vm",
        Commands::K8s { .. } => "k8s",
        Commands::Up { .. } => "up",
        Commands::Down { .. } => "down",
        Commands::Status { .. } => "status",
        Commands::Monitor(..) => "monitor",
        Commands::Version => "version",
        Commands::Doctor { .. } => "doctor",
        Commands::Report { .. } => "report",
        Commands::Upgrade { .. } => "upgrade",
        Commands::Console => "console",
        Commands::Beat => "beat",
        Commands::Sandbox { .. } | Commands::Ls => "sandbox",
        Commands::Load { .. } => "load",
        Commands::Chain { .. } => "chain",
        Commands::Network { .. } => "network",
        Commands::Host { .. } => "host",
        Commands::Wallet { .. } => "wallet",
        Commands::Share { .. } => "share",
        Commands::Shell { .. } => "shell",
        Commands::Init { .. } => "init",
    }
}

/// Decide whether a failed command should be filed as a `CommandError` exception
/// to `/v1/event` (Sentinel). Every failed run is still recorded as `ok: false`
/// on `command_completed`; this gate prevents expected non-crash outcomes from
/// paging Sentinel as CLI client bugs:
///
/// - Transparent child binary non-zero exits (`launch::ChildExited`: `kubectl`,
///   `helm`, `hanzo-vm`, `buildctl`, …) — the exit code belongs to the child tool.
/// - Upstream HTTP status responses (`product::HttpStatusError`, or a formatted
///   `-> 4xx/5xx:` / `HTTP 4xx/5xx:` status refusal): 4xx are normal user/auth/
///   validation refusals, and 5xx are server/edge faults already alerted server-side.
pub fn should_capture_error(err: &anyhow::Error) -> bool {
    for cause in err.chain() {
        if cause.is::<crate::commands::launch::ChildExited>()
            || cause.is::<crate::commands::product::HttpStatusError>()
        {
            return false;
        }
    }
    !is_http_status_message(&err.to_string())
}

fn is_http_status_message(msg: &str) -> bool {
    for prefix in [" -> ", "HTTP "] {
        let mut rest = msg;
        while let Some(idx) = rest.find(prefix) {
            let after = &rest[idx + prefix.len()..];
            let bytes = after.as_bytes();
            if bytes.len() >= 3
                && matches!(bytes[0], b'4' | b'5')
                && bytes[1].is_ascii_digit()
                && bytes[2].is_ascii_digit()
                && bytes.get(3).is_none_or(|b| !b.is_ascii_digit())
            {
                return true;
            }
            rest = after;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::{launch::ChildExited, product::HttpStatusError};
    use std::path::PathBuf;
    use std::process::Command;

    fn non_zero_status() -> std::process::ExitStatus {
        #[cfg(unix)]
        let mut cmd = {
            let mut c = Command::new("sh");
            c.args(["-c", "exit 1"]);
            c
        };
        #[cfg(windows)]
        let mut cmd = {
            let mut c = Command::new("cmd");
            c.args(["/C", "exit 1"]);
            c
        };
        cmd.status().expect("spawn exit 1")
    }

    #[test]
    fn skips_child_process_non_zero_exits() {
        let err: anyhow::Error = ChildExited {
            bin: PathBuf::from("/usr/local/bin/kubectl"),
            status: non_zero_status(),
        }
        .into();
        assert!(!should_capture_error(&err));
        let wrapped = err.context("kubectl rollout restart deploy/cloud failed");
        assert!(!should_capture_error(&wrapped));
    }

    #[test]
    fn skips_http_4xx_and_5xx_api_responses() {
        for (status, path, detail) in [
            (
                400,
                "/v1/platform/apps",
                r#"{"detail":"a repository in this organization's workspace..."}"#,
            ),
            (
                403,
                "/v1/git/workflows",
                r#"{"code":"forbidden","detail":"forge: ..."}"#,
            ),
            (502, "/v1/platform/apps", "error code: 502"),
            (
                400,
                "/v1/git/repos/cd-clock-test/push",
                r#"{"detail":"invalid body: json..."}"#,
            ),
        ] {
            let err: anyhow::Error = HttpStatusError {
                path: path.to_string(),
                status,
                detail: detail.to_string(),
                hint: None,
            }
            .into();
            assert!(!should_capture_error(&err), "status {status} on {path}");
        }
    }

    #[test]
    fn skips_formatted_http_status_fallback_messages() {
        assert!(!should_capture_error(&anyhow::anyhow!(
            "/v1/platform/apps -> 502: error code: 502"
        )));
        assert!(!should_capture_error(&anyhow::anyhow!(
            "cloud coding: HTTP 403: forbidden"
        )));
    }

    #[test]
    fn keeps_genuine_cli_faults() {
        assert!(should_capture_error(&anyhow::anyhow!(
            "reading config ~/.hanzo/config.toml: invalid TOML syntax"
        )));
        assert!(should_capture_error(&anyhow::anyhow!(
            "connecting to local unix socket: permission denied"
        )));
        assert!(should_capture_error(&anyhow::anyhow!(
            "migrating store schema v4 -> v5 failed"
        )));
    }
}
