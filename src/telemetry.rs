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
        .map(|tokens| tokens.access_token);
    hanzo_event::Telemetry::new(hanzo_event::Config {
        product: "cli".to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        api_base: api,
        distinct_id,
        bearer,
    })
}

/// The fixed dispatch label for a top-level command — the verb ONLY, never argv,
/// paths, or user input, so the recorded property can carry no PII.
pub fn label(command: &Commands) -> &'static str {
    match command {
        Commands::Agent { .. } => "agent",
        Commands::Auth { .. } => "auth",
        // Every spelling of the coding session reports as one command, because
        // it IS one command — `hanzo dev` is not a distinct thing to measure.
        Commands::Code(..) | Commands::Dev(..) => "code",
        Commands::Config { .. } => "config",
        Commands::Engine { .. } => "engine",
        Commands::Link { .. } => "link",
        Commands::Runner { .. } => "runner",
        Commands::Scan { .. } => "scan",
        Commands::Serve { .. } => "serve",
        Commands::Version => "version",
        Commands::Fabric { .. } => "fabric",
        Commands::Network { .. } => "network",
        Commands::Host { .. } => "host",
        Commands::Wallet { .. } => "wallet",
        Commands::Billing { .. } => "billing",
        Commands::Connector { .. } => "connector",
        Commands::Share { .. } => "share",
        Commands::Init { .. } => "init",
    }
}
