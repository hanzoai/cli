//! `hanzo model serve MODEL` — serve a model from THIS machine on a local
//! /v1 chat-completions endpoint.
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

/// `hanzo model serve MODEL [-- engine args…]`
pub async fn serve(model: String, passthrough: Vec<String>) -> Result<()> {
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
    let mut argv = vec!["serve".to_string(), "-m".to_string(), model];
    argv.extend(passthrough);
    launch::exec(&bin, &argv)
}
