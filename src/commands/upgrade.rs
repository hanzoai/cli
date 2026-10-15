//! `hanzo upgrade [tool…]`: every Hanzo tool to its current release, and the
//! coding agents `hanzo` runs.
//!
//! Hanzo's own tools (`hanzo`, `mcp`, `dev`, `zt`) are native binaries, and
//! hanzo.sh is the one implementation of fetching one: platform, asset name,
//! checksum, second name. Upgrading is running it again, into the directory this
//! `hanzo` lives in, so the build that answers `hanzo` is the build that was
//! upgraded. An agent that ships on npm (`gemini`, `qwen`, `opencode`, …) is
//! installed under `~/.hanzo`, its binary in `~/.hanzo/bin`, where `hanzo --<agent>`
//! finds it; nothing global is touched.
//!
//! With no tool named, everything Hanzo's is upgraded, and so is every agent
//! already under `~/.hanzo`. Naming an agent installs it.

use anyhow::{bail, Context, Result};
use colored::Colorize;
use std::path::PathBuf;

use super::code::agents::{self, Agent};

/// Hanzo's native tools, as hanzo.sh names them.
pub const NATIVE: &[&str] = &["hanzo", "mcp", "dev", "zt"];

/// The installer, served at the root of hanzo.sh.
const INSTALLER: &str = "https://hanzo.sh";

/// What one invocation asks for: native tools for hanzo.sh, agents for npm.
#[derive(Debug, PartialEq)]
pub struct Plan {
    pub native: Vec<String>,
    pub agents: Vec<&'static Agent>,
}

/// Sort the named tools; with none named, every native tool and every agent
/// already installed under `~/.hanzo`.
pub fn plan(names: &[String], installed: impl Fn(&Agent) -> bool) -> Result<Plan> {
    if names.is_empty() {
        return Ok(Plan {
            native: NATIVE.iter().map(|s| s.to_string()).collect(),
            agents: agents::AGENTS.iter().filter(|a| a.package.is_some() && installed(a)).collect(),
        });
    }
    let mut p = Plan { native: Vec::new(), agents: Vec::new() };
    for n in names {
        let n = n.trim().to_ascii_lowercase();
        let native = match n.as_str() {
            "hanzo-mcp" => Some("mcp"),
            "hanzo-node" | "cli" => Some("hanzo"),
            other => NATIVE.iter().copied().find(|t| *t == other),
        };
        if let Some(t) = native {
            p.native.push(t.to_string());
        } else if let Some(a) = agents::find(&n) {
            if a.package.is_none() {
                bail!("{} installs from its own site, not through hanzo: see `hanzo --{} --help` for its docs", a.title, a.name);
            }
            p.agents.push(a);
        } else {
            let mut known: Vec<&str> = NATIVE.to_vec();
            known.extend(agents::AGENTS.iter().filter(|a| a.package.is_some()).map(|a| a.name));
            bail!("no tool named `{n}` (upgradable: {})", known.join(", "));
        }
    }
    Ok(p)
}

/// Whether `a` was installed by `hanzo upgrade`, under `~/.hanzo/bin`.
fn under_hanzo(a: &Agent) -> bool {
    agents::prefix().is_some_and(|p| a.programs.iter().any(|b| p.join("bin").join(b).is_file()))
}

/// The directory this binary lives in, which is where its siblings are upgraded.
fn here() -> Result<PathBuf> {
    let exe = std::env::current_exe().context("finding this binary")?;
    let exe = exe.canonicalize().unwrap_or(exe);
    Ok(exe.parent().context("this binary has no directory")?.to_path_buf())
}

pub async fn run(names: Vec<String>) -> Result<()> {
    let plan = plan(&names, under_hanzo)?;
    let mut failed = Vec::new();

    if !plan.native.is_empty() {
        if cfg!(windows) {
            bail!("on Windows, download the release from https://github.com/hanzoai/cli/releases; hanzo.sh is a POSIX installer");
        }
        let dir = here()?;
        println!("{} {} → {}", "upgrading".bold(), plan.native.join(", "), dir.display());
        let script = reqwest::get(INSTALLER)
            .await
            .and_then(reqwest::Response::error_for_status)
            .context("fetching the installer from hanzo.sh")?
            .text()
            .await
            .context("reading the installer")?;
        if !script.starts_with("#!") {
            bail!("hanzo.sh did not answer with an installer");
        }
        let status = tokio::process::Command::new("sh")
            .arg("-c")
            .arg(script)
            .arg("hanzo.sh")
            .arg("--dir")
            .arg(&dir)
            .args(&plan.native)
            .status()
            .await
            .context("running the installer with sh")?;
        if !status.success() {
            failed.push(format!("{} ({status})", plan.native.join(", ")));
        }
    }

    if !plan.agents.is_empty() {
        let npm = which::which("npm").context("installing an agent needs npm (Node.js 20 or newer)")?;
        let prefix = agents::prefix().context("no home directory")?;
        for a in &plan.agents {
            let package = a.package.expect("planned agents ship on npm");
            println!("{} {} ({package}) → {}", "upgrading".bold(), a.title, prefix.join("bin").display());
            let status = tokio::process::Command::new(&npm)
                .args(["install", "--global", "--no-audit", "--no-fund", "--no-update-notifier", "--loglevel=error", "--prefix"])
                .arg(&prefix)
                .arg(format!("{package}@latest"))
                .status()
                .await
                .with_context(|| format!("running npm for {package}"))?;
            match (status.success(), agents::program(a)) {
                (true, Some(bin)) => {
                    let v = super::code::backend::backend_version(&bin.display().to_string()).unwrap_or_else(|| "?".into());
                    println!("  {} {} {v}", "✓".green(), a.name);
                }
                _ => failed.push(a.name.to_string()),
            }
        }
        if std::env::var_os("PATH").is_some_and(|p| !std::env::split_paths(&p).any(|d| d == prefix.join("bin"))) {
            println!("  {}", format!("hanzo finds them in {} without it on PATH; add it to run them directly", prefix.join("bin").display()).dimmed());
        }
    }

    if !failed.is_empty() {
        bail!("not upgraded: {}", failed.join(", "));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn nothing_named_is_everything_hanzo_ships_and_the_agents_it_installed() {
        let p = plan(&[], |a| a.name == "gemini").unwrap();
        assert_eq!(p.native, names(NATIVE));
        assert_eq!(p.agents.iter().map(|a| a.name).collect::<Vec<_>>(), ["gemini"]);
    }

    #[test]
    fn a_named_tool_is_sorted_to_its_installer() {
        let p = plan(&names(&["hanzo-mcp", "qwen-code", "dev", "kilocode"]), |_| false).unwrap();
        assert_eq!(p.native, names(&["mcp", "dev"]));
        assert_eq!(p.agents.iter().map(|a| a.name).collect::<Vec<_>>(), ["qwen", "kilo"]);
    }

    #[test]
    fn an_agent_from_its_own_site_or_an_unknown_name_is_refused_with_the_list() {
        let e = plan(&names(&["cursor"]), |_| false).unwrap_err().to_string();
        assert!(e.contains("its own site"), "{e}");
        let e = plan(&names(&["nope"]), |_| false).unwrap_err().to_string();
        assert!(e.contains("upgradable: hanzo, mcp, dev, zt, gemini"), "{e}");
    }
}
