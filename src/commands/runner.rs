//! `hanzo runner` — provide THIS machine as a Hanzo CI runner.
//!
//! The runner daemon is `arcd` (Hanzo's self-hosted CI on our own fleet — NO
//! GitHub builders). We resolve an EXISTING `arcd` binary and pass the verb
//! through — `start` runs the runner, `stop` stops it, `status` reports it. We
//! never BUILD here (CI/CD does); an absent `arcd` is an honest error naming the
//! override. This is a transparent launcher over `arcd`, not a reimplementation.

use anyhow::{anyhow, Result};
use std::path::PathBuf;

use crate::commands::launch;

fn arcd() -> Result<PathBuf> {
    launch::resolve("HANZO_RUNNER_BIN", &["arcd"]).ok_or_else(|| {
        anyhow!(
            "arcd not found. Set HANZO_RUNNER_BIN=/path/to/arcd or put `arcd` on PATH (the \
             self-hosted CI runner; we do not build it here — CI/CD does)."
        )
    })
}

/// `hanzo runner start` — register + run this machine as a CI runner.
pub async fn start() -> Result<()> {
    launch::exec(&arcd()?, &["start".to_string()])
}

/// `hanzo runner stop` — stop the runner on this machine.
pub async fn stop() -> Result<()> {
    launch::exec(&arcd()?, &["stop".to_string()])
}

/// `hanzo runner status` — report the runner's state.
pub async fn status() -> Result<()> {
    launch::exec(&arcd()?, &["status".to_string()])
}
