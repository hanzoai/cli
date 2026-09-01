//! `hanzo vm <args…>` — the native microVM CLI (hanzoai/vm), run verbatim.
//!
//! One resolver, no reimplementation: `hanzo-vm` on PATH, else the place its
//! installer puts it (`~/.local/bin/hanzo-vm`). Absent, the install line is the
//! whole answer. `hanzo up` boots its k3s VM through the same resolver, so
//! "where is the vm binary?" is answered in exactly one place.

use crate::commands::launch;
use anyhow::{anyhow, Result};
use std::path::PathBuf;

/// Locate the `hanzo-vm` binary: PATH first, then the installer's default.
pub(crate) fn resolve() -> Option<PathBuf> {
    which::which("hanzo-vm").ok().or_else(|| {
        let p = dirs::home_dir()?.join(".local/bin/hanzo-vm");
        p.is_file().then_some(p)
    })
}

/// The honest absence error, naming both ways to install.
pub(crate) fn missing() -> anyhow::Error {
    anyhow!(
        "hanzo-vm not found. Install it with `cargo install hanzo-vm`, or\n\
         `curl -fsSL https://raw.githubusercontent.com/hanzoai/vm/main/install.sh | sh`"
    )
}

/// `hanzo vm <args…>` — exec the binary with the args verbatim. A passthrough is
/// transparent: the child owns the terminal and its exit is our exit, exactly
/// the [`launch`] contract.
pub fn run(args: Vec<String>) -> Result<()> {
    let bin = resolve().ok_or_else(missing)?;
    launch::exec(&bin, &args)
}
