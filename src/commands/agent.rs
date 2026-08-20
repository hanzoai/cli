//! `hanzo agent run TASK` — the ONE way to run a managed AI task.
//!
//! Two modes over the SAME session-aware orchestrator (`commands::code`):
//! - `--mode code` (default): a managed coding workspace — wrap Claude Code / `dev`,
//!   attach the Hanzo MCP toolset, route model calls through api.hanzo.ai, and
//!   stream the session to Hanzo cloud when signed in.
//! - `--mode desktop`: browser/desktop control (à la hanzo.bot) — the same
//!   orchestrator with the first-party Hanzo MCP browser/computer tools attached,
//!   so the agent drives a browser and the desktop rather than only the repo.
//!
//! Both are the identical run object (linked, resumable, metered); the mode picks
//! what the agent is pointed at. There is one implementation — `code::run` — and
//! this is the single verb that reaches it.

use anyhow::Result;

use crate::commands::code;
use crate::config::Config;

/// What the agent is pointed at.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// A managed coding workspace.
    Code,
    /// Browser + desktop control.
    Desktop,
}

impl Mode {
    /// Parse the `--mode` flag. clap validates the choice, so this is total.
    pub fn parse(s: &str) -> Mode {
        match s {
            "desktop" => Mode::Desktop,
            _ => Mode::Code,
        }
    }
}

/// `hanzo agent run` — run the task under the chosen mode. Desktop mode guarantees
/// the Hanzo MCP toolset is attached (its browser/computer tools are how the agent
/// controls the desktop), so it is never silently dropped by a persisted
/// `mcp: false`; everything else is the shared code run.
pub async fn run(cfg: &mut Config, mut opts: code::Options, mode: Mode) -> Result<()> {
    if mode == Mode::Desktop {
        // The browser/computer control tools ARE the Hanzo MCP toolset — desktop
        // mode cannot run without them, so it pins them on.
        opts.mcp = true;
    }
    code::run(cfg, opts).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mode_parses_desktop_else_code() {
        assert_eq!(Mode::parse("desktop"), Mode::Desktop);
        assert_eq!(Mode::parse("code"), Mode::Code);
        assert_eq!(Mode::parse("anything"), Mode::Code);
    }
}
