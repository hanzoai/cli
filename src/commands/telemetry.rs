//! `hanzo telemetry` — manage telemetry opt-in/opt-out.
//!
//! Telemetry is opt-in (off by default). Enable with `hanzo telemetry on` or
//! `hanzo config set telemetry.enabled true`. Disable with `hanzo telemetry off`.

use crate::config::Config;
use anyhow::Result;

/// `hanzo telemetry status` — print current telemetry state.
pub fn status(cfg: &Config) -> Result<()> {
    let enabled = cfg.telemetry.enabled;
    println!(
        "Telemetry: {}",
        if enabled {
            "enabled (opt-in)"
        } else {
            "disabled (opt-in required)"
        }
    );
    println!("Config: telemetry.enabled = {}", enabled);

    // Check env overrides
    if let Ok(v) = std::env::var("HANZO_TELEMETRY") {
        println!("Env HANZO_TELEMETRY = {}", v);
    }
    if let Ok(v) = std::env::var("DO_NOT_TRACK") {
        println!("Env DO_NOT_TRACK = {}", v);
    }
    if std::env::var("CI").ok().as_deref() == Some("true") {
        println!("Env CI = true (forces off unless HANZO_TELEMETRY=1/true/on)");
    }
    Ok(())
}

/// `hanzo telemetry on` — enable telemetry (opt-in).
pub fn enable(cfg: &mut Config) -> Result<()> {
    cfg.update(|c| {
        c.telemetry.enabled = true;
        Ok(())
    })?;
    println!("Telemetry enabled. Run `hanzo telemetry status` to verify.");
    Ok(())
}

/// `hanzo telemetry off` — disable telemetry.
pub fn disable(cfg: &mut Config) -> Result<()> {
    cfg.update(|c| {
        c.telemetry.enabled = false;
        Ok(())
    })?;
    println!("Telemetry disabled.");
    Ok(())
}

/// Entry point for `hanzo telemetry <subcommand>`.
pub async fn run(cfg: &mut Config, subcommand: TelemetrySubcommand) -> Result<()> {
    match subcommand {
        TelemetrySubcommand::Status => status(cfg),
        TelemetrySubcommand::On => enable(cfg),
        TelemetrySubcommand::Off => disable(cfg),
    }
}

#[derive(clap::Subcommand)]
pub enum TelemetrySubcommand {
    /// Show current telemetry state (config + env)
    Status,
    /// Enable telemetry (opt-in)
    On,
    /// Disable telemetry
    Off,
}
