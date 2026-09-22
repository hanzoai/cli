//! `hanzo engine serve MODEL` — serve a model from THIS machine on a local
//! /v1 chat-completions endpoint, with optional .pleo n-gram memory table overlay.
//!
//! The backend is the Hanzo engine (`~/work/hanzo/engine`): its `serve` command
//! exposes `/v1/chat/completions` and `/v1/messages` for a local model — the two
//! request shapes the ecosystem has settled on, stated as shapes rather than as
//! somebody else's product name.
//! We resolve an EXISTING engine binary and exec `serve -m <model>` through the
//! shared launcher — we never BUILD here (CI/CD does). Extra engine flags (e.g.
//! `--port`) pass through after `--`. ONE way to serve a model locally.

use anyhow::{anyhow, Result};
use colored::*;
use std::path::PathBuf;

use crate::commands::launch;

/// Resolve the engine binary. NOT `hanzo` on PATH — that is THIS CLI; the engine
/// ships as `hanzo-engine` (or point `HANZO_ENGINE_BIN` at a build of it).
fn engine_bin() -> Option<PathBuf> {
    launch::resolve("HANZO_ENGINE_BIN", &["hanzo-engine"])
}

/// `hanzo engine serve MODEL [--overlay FILE] [-- engine args…]`
pub async fn serve(model: String, overlay: Option<String>, mut passthrough: Vec<String>) -> Result<()> {
    let bin = engine_bin().ok_or_else(|| {
        anyhow!(
            "engine not found. Set HANZO_ENGINE_BIN=/path/to/engine (the `serve` binary from \
             ~/work/hanzo/engine), or put `hanzo-engine` on PATH (we do not build the engine \
             here — CI/CD does)."
        )
    })?;
    println!(
        "{} serving {} on a local /v1 chat-completions endpoint",
        "→".cyan(),
        model.cyan().bold()
    );
    if let Some(ref ov) = overlay {
        println!(
            "  {} applying .pleo overlay: {}",
            "•".green(),
            ov.bold()
        );
        passthrough.push("--overlay".to_string());
        passthrough.push(ov.clone());
    }
    let mut argv = vec!["serve".to_string(), "-m".to_string(), model];
    argv.extend(passthrough);
    launch::exec(&bin, &argv)
}

/// `hanzo engine up` — bring up the native bare-metal GPU engine & router
pub async fn up() -> Result<()> {
    println!("{}", "Bringing up native bare-metal GPU inference engine & router...".cyan().bold());
    let res = reqwest::get("http://127.0.0.1:1235/health").await;
    if res.is_err() {
        println!("  {} Starting hanzo-router (:1235)...", "→".cyan());
        let _ = std::process::Command::new("systemctl")
            .args(["--user", "restart", "hanzo-router.service"])
            .status();
    } else {
        println!("  {} hanzo-router is active (:1235)", "●".green());
    }

    let telem = crate::commands::monitor::collect_telemetry().await;
    crate::commands::monitor::render_dashboard(&telem);
    Ok(())
}

/// `hanzo engine down` — stop the native bare-metal GPU engine & router
pub async fn down() -> Result<()> {
    println!("{}", "Stopping native bare-metal GPU inference engine & router...".cyan().bold());
    let _ = std::process::Command::new("systemctl")
        .args(["--user", "stop", "hanzo-router.service"])
        .status();
    println!("  {} hanzo-router stopped", "●".yellow());
    Ok(())
}

/// `hanzo engine status` — non-interactive inference telemetry snapshot
pub async fn status() -> Result<()> {
    let telem = crate::commands::monitor::collect_telemetry().await;
    crate::commands::monitor::render_dashboard(&telem);
    Ok(())
}
