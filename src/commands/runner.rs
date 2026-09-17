//! `hanzo runner` — provide THIS machine as a Hanzo CI runner.
//!
//! The runner is the cloud binary's own `runner` command, so this is a
//! transparent launcher over it rather than a reimplementation. It runs in the
//! FOREGROUND, which is how it runs: the process owns the terminal and Ctrl-C
//! stops it, so there is nothing else to start or stop. We never BUILD here
//! (CI/CD does); an absent binary is an honest error naming the override.

use anyhow::{anyhow, Result};
use std::path::PathBuf;

use crate::commands::launch;

fn cloud() -> Result<PathBuf> {
    launch::resolve("HANZO_CLOUD_BIN", &["hanzo-cloud", "cloud"]).ok_or_else(|| {
        anyhow!(
            "cloud binary not found. Set HANZO_CLOUD_BIN=/path/to/hanzo-cloud or put \
             `hanzo-cloud` on PATH — it carries the runner; we do not build it here (CI/CD does)."
        )
    })
}

/// `hanzo runner start` — register + run this machine as a CI runner.
pub async fn start() -> Result<()> {
    launch::exec(&cloud()?, &["runner".to_string()])
}
