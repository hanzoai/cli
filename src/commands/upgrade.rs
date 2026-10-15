//! `hanzo upgrade [tool…]`: every Hanzo tool to its current release, and the
//! coding agents `hanzo` runs.
//!
//! Hanzo's own tools (`hanzo`, `mcp`, `dev`, `zt`) are native binaries, and
//! hanzo.sh is the one implementation of fetching one: platform, asset name,
//! checksum, second name. Upgrading is running it again, into the directory this
//! `hanzo` lives in, so the build that answers `hanzo` is the build that was
//! upgraded.
//!
//! An agent upgrades the way its own file says ([`Upgrade`]): one that ships on
//! npm (`gemini`, `qwen`, `opencode`, …) is installed under `~/.hanzo`, its
//! binary in `~/.hanzo/bin`, where `hanzo --<agent>` runs it first; one with an
//! updater of its own (`claude update`, `goose update`, …) runs that; one
//! installed with `uv tool install` (Aider, OpenHands) is upgraded by uv.
//!
//! With no tool named, everything Hanzo's is upgraded, and so is every agent
//! already installed: an npm agent under `~/.hanzo`, a self-updating one
//! anywhere, a uv one where uv installed it. Naming an agent installs it when
//! it ships on npm.

use anyhow::{bail, Context, Result};
use colored::Colorize;
use std::path::{Path, PathBuf};

use super::code::agents::{self, Agent, Upgrade};
use super::code::backend::BackendKind;

/// Hanzo's native tools, as hanzo.sh names them.
pub const NATIVE: &[&str] = &["hanzo", "mcp", "dev", "zt"];

/// The installer, served at the root of hanzo.sh.
const INSTALLER: &str = "https://hanzo.sh";

/// An agent `hanzo upgrade` can name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Tool {
    pub name: &'static str,
    pub title: &'static str,
    pub programs: &'static [&'static str],
    pub upgrade: Upgrade,
}

impl From<&'static Agent> for Tool {
    fn from(a: &'static Agent) -> Tool {
        Tool { name: a.name, title: a.title, programs: a.programs, upgrade: a.upgrade }
    }
}

/// Claude Code, Codex and Open Interpreter have backends of their own rather than
/// agent files; each updates itself.
const BACKENDS: &[Tool] = &[
    Tool { name: "claude", title: "Claude Code", programs: &["claude"], upgrade: Upgrade::Own(&["update"]) },
    Tool { name: "codex", title: "Codex", programs: &["codex"], upgrade: Upgrade::Own(&["update"]) },
    Tool { name: "interpreter", title: "Open Interpreter", programs: &["interpreter"], upgrade: Upgrade::Own(&["update"]) },
];

/// Every agent `hanzo upgrade` knows, in the order help lists them.
fn tools() -> impl Iterator<Item = Tool> {
    BACKENDS.iter().copied().chain(agents::AGENTS.iter().map(Tool::from))
}

/// The agent a spelling names, by the same resolver `hanzo --<agent>` uses.
fn named(n: &str) -> Option<Tool> {
    let backend = |name: &str| BACKENDS.iter().copied().find(|t| t.name == name);
    match BackendKind::parse(n).ok()? {
        BackendKind::Dev => None,
        BackendKind::Claude => backend("claude"),
        BackendKind::Codex => backend("codex"),
        BackendKind::Interpreter => backend("interpreter"),
        BackendKind::Tool(a) => Some(Tool::from(a)),
    }
}

/// What one invocation asks for: native tools for hanzo.sh, agents by their own way.
#[derive(Debug, PartialEq)]
pub struct Plan {
    pub native: Vec<String>,
    pub tools: Vec<Tool>,
}

/// Sort the named tools; with none named, every native tool and every agent
/// already installed.
pub fn plan(names: &[String], installed: impl Fn(&Tool) -> bool) -> Result<Plan> {
    if names.is_empty() {
        return Ok(Plan { native: NATIVE.iter().map(|s| s.to_string()).collect(), tools: tools().filter(|t| installed(t)).collect() });
    }
    let mut p = Plan { native: Vec::new(), tools: Vec::new() };
    for n in names {
        let n = n.trim().to_ascii_lowercase();
        let native = match n.as_str() {
            "hanzo-mcp" => Some("mcp"),
            "hanzo-node" | "cli" => Some("hanzo"),
            other => NATIVE.iter().copied().find(|t| *t == other),
        };
        if let Some(t) = native {
            p.native.push(t.to_string());
        } else if let Some(t) = named(&n) {
            p.tools.push(t);
        } else {
            let mut known: Vec<&str> = NATIVE.to_vec();
            known.extend(tools().map(|t| t.name));
            bail!("no tool named `{n}` (upgradable: {})", known.join(", "));
        }
    }
    Ok(p)
}

/// Whether `t` is installed the way it upgrades: an npm agent under `~/.hanzo/bin`,
/// a self-updating one anywhere, a uv one in uv's tool directory.
fn installed(t: &Tool) -> bool {
    match t.upgrade {
        Upgrade::Npm(_) => agents::prefix().is_some_and(|p| t.programs.iter().any(|b| p.join("bin").join(b).is_file())),
        Upgrade::Own(_) => agents::find_program(t.programs).is_some(),
        Upgrade::Uv(_) => agents::find_program(t.programs).is_some_and(|p| by_uv(&p)),
    }
}

/// Whether a binary is one `uv tool install` placed: a link into uv's tool directory.
fn by_uv(bin: &Path) -> bool {
    bin.canonicalize().is_ok_and(|p| p.components().collect::<Vec<_>>().windows(2).any(|w| w[0].as_os_str() == "uv" && w[1].as_os_str() == "tools"))
}

/// The directory this binary lives in, which is where its siblings are upgraded.
fn here() -> Result<PathBuf> {
    let exe = std::env::current_exe().context("finding this binary")?;
    let exe = exe.canonicalize().unwrap_or(exe);
    Ok(exe.parent().context("this binary has no directory")?.to_path_buf())
}

pub async fn run(names: Vec<String>) -> Result<()> {
    let plan = plan(&names, installed)?;
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

    let prefix = agents::prefix().context("no home directory")?;
    let mut npm_used = false;
    for t in &plan.tools {
        let ok = match t.upgrade {
            Upgrade::Npm(package) => {
                let npm = which::which("npm").context("installing an agent needs npm (Node.js 20 or newer)")?;
                npm_used = true;
                println!("{} {} ({package}) → {}", "upgrading".bold(), t.title, prefix.join("bin").display());
                tokio::process::Command::new(&npm)
                    .args(["install", "--global", "--no-audit", "--no-fund", "--no-update-notifier", "--loglevel=error", "--prefix"])
                    .arg(&prefix)
                    .arg(format!("{package}@latest"))
                    .status()
                    .await
                    .with_context(|| format!("running npm for {package}"))?
                    .success()
            }
            Upgrade::Own(args) => match agents::find_program(t.programs) {
                Some(bin) => {
                    println!("{} {} ({} {})", "upgrading".bold(), t.title, bin.display(), args.join(" "));
                    tokio::process::Command::new(&bin).args(args).status().await.with_context(|| format!("running {}", bin.display()))?.success()
                }
                None => {
                    println!("  {} {} is not installed: see https://docs.hanzo.ai/docs/guides/coding for its installer", "✗".red(), t.title);
                    false
                }
            },
            Upgrade::Uv(package) => match which::which("uv") {
                Ok(uv) => {
                    println!("{} {} (uv tool upgrade {package})", "upgrading".bold(), t.title);
                    tokio::process::Command::new(uv).args(["tool", "upgrade", package]).status().await.context("running uv")?.success()
                }
                Err(_) => {
                    println!("  {} {} upgrades with uv, which is not installed", "✗".red(), t.title);
                    false
                }
            },
        };
        match (ok, agents::find_program(t.programs)) {
            (true, Some(bin)) => {
                let v = super::code::backend::backend_version(&bin.display().to_string()).unwrap_or_else(|| "?".into());
                println!("  {} {} {v}", "✓".green(), t.name);
            }
            _ => failed.push(t.name.to_string()),
        }
    }
    if npm_used && std::env::var_os("PATH").is_some_and(|p| !std::env::split_paths(&p).any(|d| d == prefix.join("bin"))) {
        println!("  {}", format!("hanzo runs them from {} without it on PATH; add it to run them directly", prefix.join("bin").display()).dimmed());
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
    fn nothing_named_is_everything_hanzo_ships_and_the_agents_installed() {
        let p = plan(&[], |t| t.name == "gemini" || t.name == "claude").unwrap();
        assert_eq!(p.native, names(NATIVE));
        assert_eq!(p.tools.iter().map(|t| t.name).collect::<Vec<_>>(), ["claude", "gemini"]);
    }

    #[test]
    fn a_named_tool_is_sorted_to_the_way_it_upgrades() {
        let p = plan(&names(&["hanzo-mcp", "qwen-code", "dev", "kilocode", "claude-code", "aider", "goose", "oi"]), |_| false).unwrap();
        assert_eq!(p.native, names(&["mcp", "dev"]));
        let got: Vec<(&str, Upgrade)> = p.tools.iter().map(|t| (t.name, t.upgrade)).collect();
        assert_eq!(
            got,
            [
                ("qwen", Upgrade::Npm("@qwen-code/qwen-code")),
                ("kilo", Upgrade::Npm("@kilocode/cli")),
                ("claude", Upgrade::Own(&["update"])),
                ("aider", Upgrade::Uv("aider-chat")),
                ("goose", Upgrade::Own(&["update"])),
                ("interpreter", Upgrade::Own(&["update"])),
            ]
        );
    }

    #[test]
    fn an_unknown_name_is_refused_with_the_list() {
        let e = plan(&names(&["nope"]), |_| false).unwrap_err().to_string();
        assert!(e.contains("upgradable: hanzo, mcp, dev, zt, claude, codex, interpreter, cursor"), "{e}");
    }

    #[test]
    fn every_agent_can_be_named() {
        for t in tools() {
            assert_eq!(plan(&names(&[t.name]), |_| false).unwrap().tools, [t]);
        }
    }

    #[test]
    fn a_binary_uv_placed_is_a_link_into_its_tool_directory() {
        let d = tempfile::tempdir().unwrap();
        let real = d.path().join("share/uv/tools/aider-chat/bin");
        std::fs::create_dir_all(&real).unwrap();
        std::fs::write(real.join("aider"), "").unwrap();
        let bin = d.path().join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(real.join("aider"), bin.join("aider")).unwrap();
        #[cfg(unix)]
        assert!(by_uv(&bin.join("aider")));
        std::fs::write(bin.join("plain"), "").unwrap();
        assert!(!by_uv(&bin.join("plain")));
    }
}
