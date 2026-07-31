//! `hanzo link` — put a shell on the fabric and register it, so it can be driven
//! from the console.
//!
//! Four things, none of them new: this machine registers as a run-target so the
//! fleet can see its CPU and GPUs and send it work, ttyd serves a shell over a
//! loopback port, `share::start` publishes that port (the same tunnel `hanzo
//! share` uses), and the session registry gets a row carrying the URL. The
//! console lists the machine, the shell under it, and frames the terminal.
//!
//! COMPUTE AND SHELL ARE ONE ACT. Linking a machine that the fleet can schedule
//! onto but nobody can look at, or a shell on a machine the fleet does not know
//! about, are both half a link — so this does both and the console shows them
//! together.
//!
//! The bytes never pass through cloud — it holds the address, not the connection
//! — so a link that ends stops answering in its own frame rather than leaving a
//! viewer holding a half-open stream.
//!
//! WHICH SHELL is a parameter, not three code paths. `$SHELL` by default (your
//! zsh), or name any command: `bash`, or `tmux` for a session that survives a
//! disconnect and can be attached locally at the same time.

use crate::commands::code::session::SessionClient;
use crate::commands::code::{context, target};
use crate::commands::{network, share};
use crate::config::Config;
use crate::iam::{paths, store};
use anyhow::{anyhow, Context, Result};
use colored::*;
use std::process::Stdio;
use tokio::process::{Child, Command};

/// The loopback port ttyd serves on. Fixed rather than random so a second `link`
/// on one machine fails loudly on a busy port instead of quietly publishing a
/// second, different shell under the first one's name.
const TTYD_PORT: u16 = 7681;

/// Resolve what ttyd should run.
///
/// `tmux` expands to an attach-or-create so a link can be re-established onto the
/// SAME shell after a disconnect — the one case where a multiplexer earns its
/// keep. Everything else is passed through, and the default is the user's own
/// `$SHELL`, because a linked terminal should look like their terminal.
fn shell_command(shell: Option<&str>) -> Vec<String> {
    match shell {
        Some("tmux") => vec![
            "tmux".into(),
            "new".into(),
            "-A".into(),
            "-s".into(),
            "hanzo".into(),
        ],
        Some(other) => vec![other.to_string()],
        None => vec![std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".into())],
    }
}

/// A ttyd child that dies with its handle, so an ended link never leaves a shell
/// listening.
struct Ttyd(Child);

impl Drop for Ttyd {
    fn drop(&mut self) {
        let _ = self.0.start_kill();
    }
}

fn start_ttyd(port: u16, cmd: &[String], writable: bool) -> Result<Ttyd> {
    let mut c = Command::new("ttyd");
    c.arg("--port")
        .arg(port.to_string())
        // Loopback ONLY: the fabric is the single way in, so the shell is never
        // exposed on the machine's LAN even briefly.
        .arg("--interface")
        .arg("127.0.0.1");
    if writable {
        c.arg("--writable");
    }
    let child = c
        .args(cmd)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .context("starting ttyd (brew install ttyd)")?;
    Ok(Ttyd(child))
}

/// `hanzo link [--shell S] [--read-only] [--title T]`.
pub async fn run(
    cfg: &mut Config,
    shell: Option<String>,
    read_only: bool,
    title: Option<String>,
) -> Result<()> {
    let cmd = shell_command(shell.as_deref());

    let api = network::active(cfg).api.trim_end_matches('/').to_string();
    // Refreshing accessor, not the raw one: a link holds a shell for hours, and
    // the access token lives one.
    let (_id, tok) = store::active_token_fresh(cfg, paths::DEFAULT_BRAND)
        .await?
        .ok_or_else(|| anyhow!("not signed in — run `hanzo auth login` first"))?;

    // Register this machine as a run-target first, so the fleet knows its CPU and
    // GPUs and the console has a machine to group the shell under. DETACHED and
    // best-effort, exactly as `hanzo code` does it: capability probing and the
    // cloud write must never be on the critical path of getting a shell up.
    {
        let (api, token) = (api.clone(), tok.access_token.clone());
        let host = hostname();
        tokio::spawn(async move {
            let machine = context::Machine::capture().await;
            target::sync(&api, &token, &context::machine_id(), &host, &machine).await;
        });
    }

    // ttyd next: publishing a port nothing is serving would announce a URL that
    // 502s, which reads as "the fabric is broken" rather than "the shell died".
    let _ttyd = start_ttyd(TTYD_PORT, &cmd, !read_only)?;
    println!("{} {}", "→".green(), cmd.join(" ").cyan());

    let mut sh = share::start(
        cfg,
        format!("http://127.0.0.1:{TTYD_PORT}"),
        "proxy".into(),
        None,
    )
    .await?;

    // Register the session LAST, so the row never advertises a URL that is not
    // yet answering.
    let client = SessionClient::new(&api, &tok.access_token)?;
    let cwd = std::env::current_dir()
        .map(|p| p.display().to_string())
        .unwrap_or_default();
    let host = hostname();
    let reg = client
        .register_shell(&cmd[0], title.as_deref().unwrap_or(&cwd), &host, &cwd)
        .await?;
    client.set_terminal(&reg.id, Some(&sh.url)).await?;

    println!("\n  {}  →  live\n", sh.url.green().bold());
    println!("  {} {}", "session".dimmed(), reg.id.dimmed());

    let out = sh.wait().await;
    // Withdraw the URL on the way out. Best effort: the tunnel is already gone, so
    // a failure here costs a stale row, not a reachable shell.
    let _ = client.set_terminal(&reg.id, None).await;
    out
}

fn hostname() -> String {
    std::process::Command::new("hostname")
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "unknown".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_to_the_users_own_shell() {
        std::env::set_var("SHELL", "/opt/homebrew/bin/zsh");
        assert_eq!(shell_command(None), vec!["/opt/homebrew/bin/zsh"]);
    }

    #[test]
    fn names_a_shell_verbatim() {
        assert_eq!(shell_command(Some("bash")), vec!["bash"]);
        assert_eq!(shell_command(Some("zsh")), vec!["zsh"]);
    }

    // tmux is attach-or-create so a dropped link comes back to the SAME shell
    // rather than a fresh one, which is the only reason to involve it at all.
    #[test]
    fn tmux_attaches_or_creates_one_named_session() {
        assert_eq!(
            shell_command(Some("tmux")),
            vec!["tmux", "new", "-A", "-s", "hanzo"]
        );
    }
}
