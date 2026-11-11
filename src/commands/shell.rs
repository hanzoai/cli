//! Opt-in shell startup: a static hook and one background fleet heartbeat.
use crate::config::Config;
use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};

const BEGIN: &str = "# >>> hanzo shell >>>";
const END: &str = "# <<< hanzo shell <<<";

#[derive(clap::Subcommand)]
pub enum Action {
    /// Enable a quiet background machine heartbeat in new interactive shells.
    Install {
        /// bash, zsh or fish; defaults to $SHELL. Linux, macOS and WSL.
        #[arg(long)]
        shell: Option<String>,
        /// Add Hanzo to PATH without enabling the background heartbeat.
        #[arg(long)]
        path_only: bool,
    },
    /// Remove the startup hook and stop the background heartbeat.
    Uninstall {
        #[arg(long)]
        shell: Option<String>,
    },
    #[command(hide = true)]
    Daemon,
}

fn home() -> Result<PathBuf> {
    dirs::home_dir().context("cannot locate your home directory")
}

fn shell_name(explicit: Option<&str>) -> Result<String> {
    let name = explicit
        .map(String::from)
        .or_else(|| std::env::var("SHELL").ok())
        .context("could not detect your shell; use `hanzo shell install --shell bash|zsh|fish`")?;
    let name = Path::new(&name)
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();
    if !matches!(name.as_str(), "bash" | "zsh" | "fish") {
        bail!("unsupported shell {name:?}; supported shells: bash, zsh, fish (Linux, macOS, WSL)");
    }
    Ok(name)
}

fn profile(home: &Path, shell: &str) -> PathBuf {
    match shell {
        "bash" if cfg!(target_os = "macos") => home.join(".bash_profile"),
        "zsh" => std::env::var_os("ZDOTDIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.into())
            .join(".zshrc"),
        "fish" => std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".config"))
            .join("fish/conf.d/hanzo.fish"),
        _ => home.join(".bashrc"),
    }
}

fn quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

fn hook(shell: &str, executable: &Path, active: bool) -> Result<String> {
    let path = executable
        .to_str()
        .context("Hanzo executable path is not UTF-8")?;
    if path.contains(['\n', '\r']) {
        bail!("Hanzo executable path contains a newline");
    }
    let bin = quote(path);
    let dir = executable
        .parent()
        .context("Hanzo executable has no parent")?
        .to_str()
        .context("invalid executable directory")?;
    let body = if shell == "fish" {
        // Fish's single-quoted strings escape backslash and apostrophe directly.
        let fish_quote = |s: &str| format!("'{}'", s.replace('\\', "\\\\").replace('\'', "\\'"));
        let start = if active {
            format!("    if test -x {}\n        command nohup {} shell daemon </dev/null >/dev/null 2>&1 &\n    end\n", fish_quote(path), fish_quote(path))
        } else {
            String::new()
        };
        format!(
            "if status is-interactive\n    fish_add_path --path {}\n{start}end",
            fish_quote(dir)
        )
    } else {
        let start = if active {
            format!("    if [ -x {bin} ]; then\n      (command nohup {bin} shell daemon </dev/null >/dev/null 2>&1 &)\n    fi\n")
        } else {
            String::new()
        };
        format!("case $- in\n  *i*)\n    case \":$PATH:\" in *:{}:*) ;; *) export PATH={}\":$PATH\" ;; esac\n{start}    ;;\nesac", quote(dir), quote(dir))
    };
    Ok(format!("{BEGIN}\n{body}\n{END}\n"))
}

/// Only remove our exact marked block, preserving all unrelated profile bytes.
fn without_hook(input: &str) -> Result<String> {
    let mut result = String::new();
    let mut inside = false;
    for line in input.split_inclusive('\n') {
        match line.trim_end_matches(['\r', '\n']) {
            BEGIN if !inside => inside = true,
            END if inside => inside = false,
            BEGIN | END => bail!("malformed Hanzo shell block; leaving the profile untouched"),
            _ if !inside => result.push_str(line),
            _ => {}
        }
    }
    if inside {
        bail!("unfinished Hanzo shell block; leaving the profile untouched");
    }
    Ok(result)
}

pub fn install(shell: Option<&str>, enable: bool) -> Result<()> {
    update_profile(shell, enable, false)
}

fn update_profile(shell: Option<&str>, enable: bool, path_only: bool) -> Result<()> {
    let shell = shell_name(shell)?;
    let home = home()?;
    let path = profile(&home, &shell);
    // Follow a dotfile symlink instead of replacing it with a regular file.
    let path = if path.is_symlink() {
        path.canonicalize()?
    } else {
        path
    };
    let original = match std::fs::read_to_string(&path) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(e.into()),
    };
    let mut body = without_hook(&original)?;
    if enable {
        if !body.is_empty() && !body.ends_with('\n') {
            body.push('\n');
        }
        let active = !path_only || home.join(".hanzo/shell-enabled").exists();
        body.push_str(&hook(&shell, &std::env::current_exe()?, active)?);
    }
    std::fs::create_dir_all(path.parent().context("profile has no parent")?)?;
    std::fs::write(&path, body)?;
    let marker = home.join(".hanzo/shell-enabled");
    if enable && path_only {
        println!(
            "Hanzo PATH configured in {}. Open a new terminal, then run `hanzo`.",
            path.display()
        );
    } else if enable {
        std::fs::create_dir_all(marker.parent().unwrap())?;
        hanzo_gen::private::write(&marker, b"enabled\n")?;
        println!("Enabled Hanzo in {}. Open a new terminal; sign in once with `hanzo`. No browser terminal is published.", path.display());
    } else {
        match std::fs::remove_file(marker) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
        println!(
            "Removed Hanzo from {}. The background heartbeat stops within two seconds.",
            path.display()
        );
    }
    Ok(())
}

pub async fn run(cfg: &Config, action: Action) -> Result<()> {
    match action {
        Action::Install { shell, path_only } => update_profile(shell.as_deref(), true, path_only),
        Action::Uninstall { shell } => install(shell.as_deref(), false),
        Action::Daemon => daemon(cfg).await,
    }
}

async fn daemon(cfg: &Config) -> Result<()> {
    let dir = home()?.join(".hanzo");
    let enabled = dir.join("shell-enabled");
    if !enabled.exists() {
        return Ok(());
    }
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(dir.join("shell.lock"))?;
    match lock.try_lock() {
        Ok(()) => {}
        Err(std::fs::TryLockError::WouldBlock) => return Ok(()),
        Err(e) => return Err(e.into()),
    }
    let config_path = cfg.effective_path();
    let mut previous = None;
    let mut beat = None;
    // The OS releases the singleton lock on every exit, including crashes.
    while enabled.exists() {
        // Login/account changes in another terminal must reach this long-lived
        // process too. The configuration index holds no credential secrets.
        let current = std::fs::read(&config_path).ok();
        if beat.is_none() || current != previous {
            if let (Ok(fresh), Ok(root)) = (Config::load(Some(config_path.clone())), hanzo_gen::Root::need()) {
                let api = crate::commands::network::active(&fresh).api;
                let host = crate::commands::dev::Store::host(fresh);
                beat = Some(hanzo_gen::target::beat_and_serve(
                    host,
                    &root,
                    crate::iam::paths::DEFAULT_BRAND,
                    &api,
                    &hanzo_gen::context::machine_id(&root),
                    &hanzo_gen::context::hostname(&root),
                ));
                previous = current;
            }
        }
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn profile_updates_preserve_user_content_and_are_idempotent() {
        let user = "export MY_SETTING=1\n# user comment\n";
        let block = hook("bash", Path::new("/tmp/a b/'hanzo"), true).unwrap();
        let once = format!("{user}{block}");
        assert_eq!(without_hook(&once).unwrap(), user);
        assert_eq!(format!("{}{block}", without_hook(&once).unwrap()), once);
        assert!(without_hook(&format!("{user}{BEGIN}\n")).is_err());
    }
    #[test]
    fn supported_hooks_parse_and_do_nothing_in_noninteractive_shells() {
        for shell in ["bash", "zsh"] {
            if which::which(shell).is_err() {
                continue;
            }
            let script = hook(shell, Path::new("/tmp/not installed/'hanzo"), true).unwrap();
            let status = std::process::Command::new(shell)
                .args(["-n", "-c", &script])
                .status()
                .unwrap();
            assert!(status.success());
            let output = std::process::Command::new(shell)
                .args(["-c", &format!("{script}\nprintf ok")])
                .output()
                .unwrap();
            assert!(output.status.success());
            assert_eq!(output.stdout, b"ok");
            assert!(output.stderr.is_empty());
        }
    }
}
