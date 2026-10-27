## Summary
`CONTRIBUTING.md` describes setting up and testing a Python project, but this repo is a Rust CLI. A new contributor who follows it clones a repo that does not exist and installs files this repo does not have.

## Evidence
Checked at `5ce6db4`.

- `CONTRIBUTING.md:45` runs `git clone https://github.com/Dev-AI/dev.git` (404). Lines 17, 20, 23 and 37 link to `dev.chat` (the domain does not exist).

File (src/commands/upgrade.rs):
```rust
use anyhow::{bail, Context, Result};
use colored::Colorize;
use std::path::{Path, PathBuf};

use hanzo_gen::agents::{self, Agent, Upgrade};
use hanzo_gen::backend::BackendKind;
use hanzo_gen::Root;

/// Hanzo's native tools, as hanzo.sh names them.
pub const NATIVE: &[&str] = &["hanzo", "mcp", "dev", "gen", "zt"];

/// The installer, served at the root of hanzo.sh.
const INSTALLER: &str = "https://hanzo.sh";

/// An agent `hanzo upgrade` can name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Tool {
    pub name: &'static
