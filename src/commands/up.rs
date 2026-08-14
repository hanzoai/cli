//! `hanzo up` — run a Hanzo service from THIS machine.
//!
//! `serve cloud` runs the COMPLETE Hanzo Cloud API on one listener; `serve
//! <service>` runs one subsystem independently (iam | kms | gateway | storage |
//! pubsub). All of these are subcommands of the ONE Go binary (`hanzo-cloud`), so
//! we resolve it and exec it with the subsystem as its argument — we never BUILD
//! here (CI/CD does). Extra flags pass through after `--`.

use anyhow::{anyhow, Result};
use colored::*;
use std::path::PathBuf;

use crate::commands::launch;

/// Resolve the cloud binary: `HANZO_CLOUD_BIN`, then `hanzo-cloud`/`cloud` on
/// PATH. ONE resolver — `fabric --with-cloud` uses it too, so "where is cloud?"
/// is answered in exactly one place.
pub fn resolve_cloud_bin() -> Option<PathBuf> {
    launch::resolve("HANZO_CLOUD_BIN", &["hanzo-cloud", "cloud"])
}

fn missing() -> anyhow::Error {
    anyhow!(
        "cloud binary not found. Set HANZO_CLOUD_BIN=/path/to/hanzo-cloud or put `hanzo-cloud` \
         on PATH (we do not build it here — CI/CD does)."
    )
}

/// `hanzo up cloud [-- args…]` — the whole API on one listener.
pub async fn cloud(passthrough: Vec<String>) -> Result<()> {
    let bin = resolve_cloud_bin().ok_or_else(missing)?;
    println!("{} running the Hanzo Cloud API", "→".cyan());
    let mut argv = vec!["cloud".to_string()];
    argv.extend(passthrough);
    launch::exec(&bin, &argv)
}

/// `hanzo up <service> [-- args…]` — one subsystem, standalone. The service
/// name is the binary's own subcommand; the binary is the authority on which
/// names it serves, so an unknown one is its error, not a guess here.
pub async fn service(name: String, passthrough: Vec<String>) -> Result<()> {
    let bin = resolve_cloud_bin().ok_or_else(missing)?;
    println!("{} running the {} service", "→".cyan(), name.cyan().bold());
    let mut argv = vec![name];
    argv.extend(passthrough);
    launch::exec(&bin, &argv)
}
