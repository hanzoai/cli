//! `hanzo mcp` — local Hanzo MCP server & client setup (`doctor`, `install`, `pair`,
//! `index`, `share`, `kai`), forwarding directly to the native `hanzo-mcp` binary.
//!
//! Cloud `/v1/mcp` (`hanzo mcp create`) is automatically absorbed beneath this
//! group by `commands::product::augment`.

use anyhow::{anyhow, Result};
use std::path::PathBuf;

use crate::commands::launch;
use crate::McpCommands;

/// Resolve the native `hanzo-mcp` binary (`$HANZO_MCP_BIN`, `PATH`, or `~/.local/bin/hanzo-mcp`).
pub(crate) fn resolve_binary() -> Result<PathBuf> {
    if let Some(bin) = launch::resolve("HANZO_MCP_BIN", &["hanzo-mcp"]) {
        return Ok(bin);
    }
    if let Some(home) = dirs::home_dir() {
        for rel in [".local/bin/hanzo-mcp", ".hanzo/bin/hanzo-mcp"] {
            let p = home.join(rel);
            if p.exists() {
                return Ok(p);
            }
        }
    }
    Err(anyhow!(
        "hanzo-mcp is not installed (set HANZO_MCP_BIN or run `hanzo upgrade mcp`)"
    ))
}

pub(crate) fn build_argv(command: Option<McpCommands>, passthrough: Vec<String>) -> Vec<String> {
    match command {
        None => passthrough,
        Some(McpCommands::Doctor { json, fix }) => {
            let mut argv = vec!["doctor".to_string()];
            if json {
                argv.push("--json".to_string());
            }
            if fix {
                argv.push("--fix".to_string());
            }
            argv
        }
        Some(McpCommands::Install {
            target,
            binary,
            dry_run,
        }) => {
            let mut argv = vec!["install".to_string(), target];
            if let Some(b) = binary {
                argv.push("--binary".to_string());
                argv.push(b.display().to_string());
            }
            if dry_run {
                argv.push("--dry-run".to_string());
            }
            argv
        }
        Some(McpCommands::Pair { reset }) => {
            let mut argv = vec!["pair".to_string()];
            if reset {
                argv.push("--reset".to_string());
            }
            argv
        }
        Some(McpCommands::Index { dir }) => {
            vec!["index".to_string(), dir.display().to_string()]
        }
        Some(McpCommands::Share { browser, tab, url }) => vec![
            "share".to_string(),
            "--browser".to_string(),
            browser,
            "--tab".to_string(),
            tab,
            "--url".to_string(),
            url,
        ],
        Some(McpCommands::Kai { args }) => {
            let mut argv = vec!["kai".to_string()];
            argv.extend(args);
            argv
        }
        Some(McpCommands::Connect {
            url,
            node_id,
            name,
            tools,
            once,
        }) => {
            let mut argv = Vec::new();
            if let Some(t) = tools {
                argv.push("--tools".to_string());
                argv.push(t);
            }
            argv.push("connect".to_string());
            if let Some(u) = url {
                argv.push("--url".to_string());
                argv.push(u);
            }
            if let Some(id) = node_id {
                argv.push("--node-id".to_string());
                argv.push(id);
            }
            if let Some(n) = name {
                argv.push("--name".to_string());
                argv.push(n);
            }
            if once {
                argv.push("--once".to_string());
            }
            argv
        }
    }
}

pub(crate) fn run(command: Option<McpCommands>, passthrough: Vec<String>) -> Result<()> {
    let bin = resolve_binary()?;
    let argv = build_argv(command, passthrough);
    launch::exec(&bin, &argv)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_expected_subcommand_argvs() {
        assert_eq!(
            build_argv(Some(McpCommands::Doctor { json: true, fix: true }), vec![]),
            vec!["doctor", "--json", "--fix"]
        );
        assert_eq!(
            build_argv(
                Some(McpCommands::Install {
                    target: "all".into(),
                    binary: None,
                    dry_run: true,
                }),
                vec![]
            ),
            vec!["install", "all", "--dry-run"]
        );
        assert_eq!(
            build_argv(None, vec!["--transport".into(), "stdio".into()]),
            vec!["--transport", "stdio"]
        );
    }
}
