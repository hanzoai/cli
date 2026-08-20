//! Resolve an existing Hanzo binary and exec it — the ONE way the launcher
//! commands (`model serve`, `serve`, `runner`) find and run a sibling binary.
//!
//! We never BUILD here (CI/CD does); we resolve an EXISTING binary by an explicit
//! env override first, then PATH candidates, and exec it in the foreground with
//! inherited stdio, so the child's output and exit ARE the user's. An absent
//! binary is the caller's honest error naming the override — the same contract
//! `fabric` (hanzod) has always used. This is a transparent launcher: it invents
//! no behavior, it only finds and runs.

use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};
use std::process::Command;

/// Resolve a binary: `$env` (when it points at a real file), then each PATH
/// candidate in order. `None` when nothing resolves — the caller crafts the
/// honest, binary-specific error (engine vs cloud vs arcd differ).
pub fn resolve(env: &str, candidates: &[&str]) -> Option<PathBuf> {
    if let Ok(p) = std::env::var(env) {
        let pb = PathBuf::from(p);
        if pb.exists() {
            return Some(pb);
        }
    }
    candidates.iter().find_map(|c| which::which(c).ok())
}

/// Exec `bin args…` in the FOREGROUND, inheriting stdio, and map its exit status
/// to our result. A launcher is transparent: the child owns the terminal
/// (Ctrl-C stops it) and its non-zero exit is our non-zero exit.
pub fn exec(bin: &Path, args: &[String]) -> Result<()> {
    let status = Command::new(bin)
        .args(args)
        .status()
        .with_context(|| format!("running {}", bin.display()))?;
    if !status.success() {
        bail!("{} exited with {status}", bin.display());
    }
    Ok(())
}
