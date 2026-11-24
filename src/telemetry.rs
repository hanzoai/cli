//! CLI usage telemetry — best-effort canonical Events to `/v1/event`.
//!
//! One handle, built once per run, records the dispatched command (verb, duration,
//! outcome) and flushes at process exit. Everything is fail-soft and privacy-clean:
//! the visitor is a per-install [`device_id`](hanzo_event::device_id) (never PII),
//! the bearer is a write-only publishable key (never the user's access token), and
//! an opt-in enables it; an opt-out builds
//! a dead handle that reads no id and touches no network.

use crate::commands::network;
use crate::config::Config;

use crate::Commands;

/// Write-only Hanzo publishable key fallback when the caller is unsigned and
/// has not opted in to telemetry.
const DEFAULT_PUBLISHABLE_KEY: &str = "pk-CmfLA2K6kvsPflrS9DSkt06H_kSoQB_21sjedt6VJdc";

/// Build the telemetry handle for this run. An opt-out or any resolution error
/// yields a disabled handle. The bearer read is the NON-mutating index+vault path
/// NEVER uses the user's access token — only the publishable key.
pub fn build(config: &Config) -> hanzo_event::Telemetry {
    // Check both env var and config setting (config takes precedence when explicitly set)
    let env_opted_in = hanzo_event::opted_in();
    let config_opted_in = config.code.telemetry.unwrap_or(false);
    if !env_opted_in && !config_opted_in {
        return hanzo_event::Telemetry::disabled();
    }
    let api = network::active(config).api;
    let dir = dirs::data_local_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("hanzo");
    let distinct_id = hanzo_event::device_id(&dir);
    // NEVER use the user's access token — only the publishable key
    let bearer = std::env::var(hanzo_event::KEY_ENV)
        .ok()
        .filter(|s| !s.trim().is_empty())
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
        Commands::Down => "down",
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
        Commands::Telemetry { .. } => "telemetry",
    }
}
