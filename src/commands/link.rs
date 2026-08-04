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
//! A LINK THAT ENDS SAYS SO. The registry has no heartbeat, so "running" means
//! only that nobody said otherwise — which makes every unrecorded exit an
//! immortal row and a live shell indistinguishable from a corpse. Every way out
//! of this command therefore lands on one `finish`, and finishing is a single
//! act: the status and the withdrawn terminal URL travel in the same request, so
//! there is no window where the console can frame a dead tunnel.
//!
//! WHICH SHELL is a parameter, not three code paths. `$SHELL` by default (your
//! zsh), or name any command: `bash`, or `tmux` for a session that survives a
//! disconnect and can be attached locally at the same time.

use crate::commands::code::event::Status;
use crate::commands::code::session::SessionClient;
use crate::commands::code::{context, target};
use crate::commands::{network, share};
use crate::config::Config;
use crate::iam::{paths, store};
use anyhow::{anyhow, bail, Context, Result};
use colored::*;
use std::process::Stdio;
use std::time::Duration;
use tokio::process::{Child, Command};

/// The loopback port ttyd serves on. Fixed rather than random so a second `link`
/// on one machine fails loudly on a busy port instead of quietly publishing a
/// second, different shell under the first one's name.
const TTYD_PORT: u16 = 7681;

/// The default tmux session a bare link opens.
pub const DEFAULT_SHELL_NAME: &str = "hanzo";

/// What ttyd runs, and whether a pane may choose WHICH shell.
///
/// These are two different shapes, not one with a flag, because they differ in
/// what the URL is allowed to say.
pub enum Shell {
    /// The default. ttyd runs a wrapper and the query names the tmux session, so
    /// ONE link — one port, one tunnel, one sign-in — serves as many independent
    /// shells as the console asks for. `?arg=build` is a shell called `build`;
    /// no arg is [`DEFAULT_SHELL_NAME`].
    Multiplexed,
    /// A command named by the caller (`--shell bash`), run directly. It takes NO
    /// url argument — see [`Shell::url_arg`].
    Named(String),
}

/// The shell script the multiplexed form runs.
///
/// It reduces the requested name to `[A-Za-z0-9_-]` and 32 characters BEFORE tmux
/// sees it, and reads only `$1`. Both halves matter: `--url-arg` appends EVERY
/// `arg=` in the query to argv, and `;` is tmux's own command separator, so
/// `?arg=x&arg=;&arg=whoami` would otherwise arrive as a command rather than a
/// name. Stripping the runes and ignoring `$2`onward is what keeps the query data.
const MUX: &str =
    "n=$(printf %s \"${1:-hanzo}\" | tr -cd \"a-zA-Z0-9_-\" | cut -c1-32); exec tmux new -A -s \"${n:-hanzo}\"";

impl Shell {
    fn from_flag(shell: Option<&str>) -> Shell {
        match shell {
            // `tmux` asks for exactly what the default already is.
            None | Some("tmux") => Shell::Multiplexed,
            Some(other) => Shell::Named(other.to_string()),
        }
    }

    /// The argv ttyd runs.
    fn argv(&self) -> Vec<String> {
        match self {
            // `sh -c SCRIPT NAME` puts NAME in $0, so the client's arg lands in $1
            // — which is why the placeholder is here and not a file on disk.
            Shell::Multiplexed => vec![
                "sh".into(),
                "-c".into(),
                MUX.into(),
                "hanzo-shell".into(),
            ],
            Shell::Named(cmd) => vec![cmd.clone()],
        }
    }

    /// Whether ttyd accepts a shell name from the URL.
    ///
    /// ONLY the wrapper does. A named command would read the query as its own
    /// flags — `--shell bash` plus `?arg=-c&arg=whoami` is `bash -c whoami`, which
    /// is remote code execution handed over by a query string. The wrapper is
    /// written to be given arguments; a raw command is not.
    fn url_arg(&self) -> bool {
        matches!(self, Shell::Multiplexed)
    }

    /// What to print, so the caller can see what they are running.
    fn label(&self) -> String {
        match self {
            Shell::Multiplexed => format!("tmux ({DEFAULT_SHELL_NAME}, +named shells per pane)"),
            Shell::Named(c) => c.clone(),
        }
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

/// Whether something already serves `port` on loopback.
///
/// Asked by BINDING, because that is the question: a connect probe cannot tell
/// "another link is here" from "our own is up", and both look identical from
/// outside. A bind that fails is the port being unavailable to us, which is
/// exactly what stops ttyd.
fn port_taken(port: u16) -> bool {
    std::net::TcpListener::bind(("127.0.0.1", port)).is_err()
}

/// How long ttyd is given to die on a port it cannot have.
const TTYD_SETTLE: Duration = Duration::from_millis(600);

async fn start_ttyd(port: u16, shell: &Shell, writable: bool) -> Result<Ttyd> {
    // A SPAWN THAT SUCCEEDS PROVES A PROCESS WAS CREATED, NOT THAT IT IS SERVING.
    // ttyd exits immediately when the port is taken, and that exit lands after
    // `spawn()` has already returned Ok — so a second `hanzo link` on this machine
    // sailed past, published a tunnel at 127.0.0.1:<port>, and put the FIRST
    // link's shell behind its own session row. Measured on this machine: two link
    // processes, ONE ttyd, and two tunnels claiming one share name.
    //
    // The comment on TTYD_PORT has always promised this fails loudly. Now it does.
    if port_taken(port) {
        bail!(
            "port {port} is already serving — another `hanzo link` is almost certainly \
             running on this machine. One link per machine: end that one first, or \
             attach to the shell it already published with `tmux attach -t hanzo`."
        );
    }

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
    if shell.url_arg() {
        c.arg("--url-arg");
    }
    // OUR page, not ttyd's. It is themed to match the console that frames it,
    // carries a key row for devices with no Esc or Ctrl, and forwards the
    // workspace's chords — none of which ttyd's own page can do, because it is
    // somebody else's document inside a cross-origin frame. Best-effort: a page
    // we cannot write is a reason to serve ttyd's, never to have no terminal.
    match crate::commands::term::install() {
        Ok(page) => {
            c.arg("--index").arg(page);
        }
        Err(e) => tracing::debug!("serving ttyd's own page ({e})"),
    }
    let child = c
        .args(shell.argv())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .context("starting ttyd (brew install ttyd)")?;
    let mut t = Ttyd(child);

    // The check above races anything that grabs the port in the same instant, and
    // says nothing about a ttyd that dies for another reason. This catches both:
    // if our own child is already gone, there is no shell to publish.
    tokio::time::sleep(TTYD_SETTLE).await;
    if let Ok(Some(status)) = t.0.try_wait() {
        bail!("ttyd exited immediately ({status}) — no shell is being served on port {port}");
    }
    Ok(t)
}

/// `hanzo link [--shell S] [--read-only] [--title T]`.
pub async fn run(
    cfg: &mut Config,
    shell: Option<String>,
    read_only: bool,
    title: Option<String>,
) -> Result<()> {
    let sh_kind = Shell::from_flag(shell.as_deref());

    let api = network::active(cfg).api.trim_end_matches('/').to_string();
    // Refreshing accessor, not the raw one: a link holds a shell for hours, and
    // the access token lives one.
    let (_id, tok) = store::active_token(cfg, paths::DEFAULT_BRAND)
        .await?
        .ok_or_else(|| anyhow!("not signed in — run `hanzo auth login` first"))?;

    // Hold this machine open as a run-target, so the fleet knows its CPU and GPUs
    // and the console has a machine to group the shell under. A BEAT, not a single
    // register: cloud decides liveness from when a machine last wrote, so a link
    // that announced itself once and went quiet reads offline while the shell it
    // published is still serving. The guard beats until this command returns —
    // detached and best-effort, never on the critical path of getting a shell up.
    let _machine = target::beat(cfg, &api, &context::machine_id(), &context::hostname());

    // ttyd next: publishing a port nothing is serving would announce a URL that
    // 502s, which reads as "the fabric is broken" rather than "the shell died".
    let _ttyd = start_ttyd(TTYD_PORT, &sh_kind, !read_only).await?;
    println!("{} {}", "→".green(), sh_kind.label().cyan());

    let mut sh = share::start(
        cfg,
        format!("http://127.0.0.1:{TTYD_PORT}"),
        "proxy".into(),
        None,
        // Gated by default. A terminal reachable by anyone who learns the URL is
        // not something to opt IN to protecting.
        Some("hanzo"),
    )
    .await?;

    // Register the session LAST, so the row never advertises a URL that is not
    // yet answering. The host is the SAME value the run-target above registered
    // under, which is what lets the console file this shell under that machine
    // instead of under nothing.
    let client = SessionClient::new(&api, &tok.access_token)?;
    let cwd = std::env::current_dir()
        .map(|p| p.display().to_string())
        .unwrap_or_default();
    let host = context::hostname();
    // A registry that cannot take the row does not take the SHELL with it.
    //
    // The tunnel is up and serving by now; the row is how the console FINDS it,
    // not what makes it work. Failing here handed back nothing at all — no
    // terminal, no URL — for an outage in a different process, which is the one
    // outcome that helps nobody. The machine's own heartbeat has always degraded
    // this way (`sync` swallows every failure); the session did not, and the
    // difference was never a decision.
    //
    // Unlisted is said out loud, because a link the console cannot show is a
    // different thing from a link, and finding that out later is worse.
    let listing = match client
        .register("tmux", title.as_deref().unwrap_or(&cwd), &host, &cwd)
        .await
    {
        Ok(reg) => Some(reg.id),
        Err(e) => {
            crate::warn(&format!(
                "could not register this session ({e}); the terminal below works,                  but the console cannot list it"
            ));
            None
        }
    };

    // From here the registry may hold a LIVE row, so every way out — including
    // the ones that are errors — has to travel through `finish`.
    let out = serve(&client, listing.as_deref(), &mut sh).await;
    finish(&client, listing.as_deref(), out).await
}

/// Publish the shell and hold it until the link ends.
///
/// One function owns every ending so that one caller can record it: the shell
/// exiting, the tunnel dying under it, a publish that never landed, or the OS
/// asking this process to stop. None of those is "still running", and until now
/// only the first two returned at all.
/// `id` is `None` when the registry refused the row: everything the console needs
/// is skipped and everything the SHELL needs still happens.
async fn serve(client: &SessionClient, id: Option<&str>, sh: &mut share::Share) -> Result<()> {
    let url = sh.url.clone();

    // Wear the URL. Part of PUBLISHING, not of attaching — see `pin`.
    pin(Some(&url)).await;

    let _where = match id {
        Some(id) => {
            // Publishing the terminal is best-effort for the same reason the row
            // is: the tunnel already answers at this URL whether or not cloud
            // records it.
            if let Err(e) = client.publish_terminal(id, &url).await {
                crate::warn(&format!("could not publish the terminal URL ({e})"));
            }
            // Follow the shell. Held for exactly this session's lifetime.
            Some(follow(client.clone(), id.to_string()))
        }
        None => None,
    };

    println!("\n  {}  →  live\n", sh.url.green().bold());
    match id {
        Some(id) => println!("  {} {}", "session".dimmed(), id.dimmed()),
        None => println!("  {}", "unlisted — the console cannot show this one".dimmed()),
    }

    // Hand the caller a prompt on the SAME session ttyd is serving, rather than
    // making them wait on a tunnel they cannot type into. Both ends attach to one
    // tmux session, so what is typed here appears there and the reverse.
    //
    // When tmux will not take the terminal, the link is NOT over — the tunnel is
    // still serving and the browser can still drive it — so hold it instead.
    let held = async {
        if took_over(attach().await) {
            Ok(()) // the caller had the shell and left it: the link is done
        } else {
            println!(
                "  {} attach here with {}",
                "no local terminal —".dimmed(),
                "tmux attach -t hanzo".cyan()
            );
            sh.wait().await
        }
    };

    tokio::select! {
        r = held => r,
        // Ctrl-C, a closed terminal window or a `kill` ends a link as surely as
        // exiting the shell does, and it is the ending that went unrecorded: the
        // process died before it could speak and left the row "running" for good.
        // Returning normally also lets ttyd and the tunnel die with their handles
        // rather than being orphaned by a signal.
        _ = stopped() => Ok(()),
    }
}

/// How often the link asks tmux where the shell has got to.
///
/// A person changes directory in seconds and reads the console in minutes, so
/// this is about being RIGHT rather than instant. It is one `tmux
/// display-message` — no process spawned per window, no watcher on the
/// filesystem — and a PATCH goes out only when the answer actually changed.
const WHERE_EVERY: std::time::Duration = std::time::Duration::from_secs(15);

/// A watcher that keeps the session's `cwd` true, for as long as it is held.
struct Where(tokio::task::JoinHandle<()>);

impl Drop for Where {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// Keep telling cloud where the shell is.
///
/// `cwd` is registered once, and for a run that starts in a directory and stays
/// there that is the whole truth. A linked shell is not that: it is a place a
/// person moves around in, so the console went on naming the directory `hanzo
/// link` happened to start in long after the shell had walked away.
///
/// The answer comes from tmux, which already knows it — `#{pane_current_path}` of
/// the active pane — rather than from anything this process tracks itself. Only a
/// CHANGE is reported: an unchanged path is not news, and a PATCH per tick would
/// be a write loop that says nothing.
fn follow(client: SessionClient, id: String) -> Where {
    Where(tokio::spawn(async move {
        let mut last = String::new();
        loop {
            if let Some(now) = active_path().await.filter(|p| worth_reporting(&last, p)) {
                // Best-effort, exactly like the heartbeat: a console showing a
                // slightly stale directory is not worth failing a shell over.
                if client.set_cwd(&id, &now).await.is_ok() {
                    last = now;
                }
            }
            tokio::time::sleep(WHERE_EVERY).await;
        }
    }))
}

/// Whether a path is news.
///
/// Only a CHANGE is reported. Ticking a PATCH every interval regardless would be
/// a write loop that says nothing, and it would move the row's `updatedAt`
/// forever — making a long-idle session look busy to anything reading recency.
fn worth_reporting(last: &str, now: &str) -> bool {
    !now.is_empty() && now != last
}

/// Where the shared session's active pane is, as tmux reports it.
async fn active_path() -> Option<String> {
    let out = Command::new("tmux")
        .args(["display-message", "-p", "-t", "hanzo", "#{pane_current_path}"])
        .output()
        .await
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let p = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!p.is_empty()).then_some(p)
}

/// Show the live URL on the shared session's own status line — or clear it.
///
/// tmux clears the screen on attach, so anything printed before it — including the
/// one thing the caller needs to copy — scrolls away the moment the shell appears.
/// The status line survives that, and every clear after it.
///
/// THIS IS PART OF PUBLISHING, NOT OF ATTACHING. It used to ride along on the
/// local `tmux new -A` invocation, so it only happened when tmux took the caller's
/// terminal — which is precisely the case that does NOT happen headless, or from
/// inside tmux. Meanwhile the tmux SERVER outlives every link, so the bar went on
/// advertising whichever URL was last pinned successfully: a link from hours ago,
/// pointing at a tunnel that no longer exists.
///
/// Session-scoped (`-t hanzo`), never `-g`. The global form writes the server-wide
/// default and leaks this link's URL into every other tmux session on the machine.
///
/// The session is created DETACHED first so there is something to set the option
/// on: ttyd does not run its command until a browser connects, so at publish time
/// the session may not exist yet. Creating it is idempotent — an existing session
/// makes `new-session` fail, which is exactly the outcome that needs no action.
async fn pin(url: Option<&str>) {
    let _ = Command::new("tmux")
        .args(["new-session", "-d", "-s", "hanzo"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .await;
    for args in bar_args(url) {
        let _ = Command::new("tmux")
            .args(&args)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .await;
    }
}

/// The tmux options that put `url` on the bar, or clear it when there is none.
fn bar_args(url: Option<&str>) -> [Vec<String>; 2] {
    let bar = url.map(|u| format!(" {u} ")).unwrap_or_default();
    [
        vec!["set-option".into(), "-t".into(), "hanzo".into(), "status-right".into(), bar],
        vec!["set-option".into(), "-t".into(), "hanzo".into(), "status-right-length".into(), "80".into()],
    ]
}

/// Put the caller on the same tmux session ttyd serves, and report how it exited.
///
/// `None` means tmux could not be spawned at all.
async fn attach() -> Option<i32> {
    Command::new("tmux")
        .args(["new", "-A", "-s", "hanzo"])
        .status()
        .await
        .ok()
        .and_then(|s| s.code())
}

/// Whether tmux TOOK OVER the caller's terminal.
///
/// Only a clean exit means it did. Every other outcome means it never had the
/// terminal: a non-zero exit ("open terminal failed: not a terminal" when there is
/// no tty, "sessions should be nested with care" when `hanzo link` is run from
/// INSIDE tmux), a tmux that is not installed, or one killed by a signal.
///
/// This distinction is the whole difference between a link and a one-second link.
/// `Command::status()` answers `Ok` for a FAILED exit as readily as a successful
/// one, so treating "it returned" as "the shell exited" ended the link immediately
/// on every machine that could not attach — while the tunnel it had just published
/// was serving perfectly well.
fn took_over(code: Option<i32>) -> bool {
    code == Some(0)
}

/// Record how this link ended, then hand the ending back unchanged.
///
/// THE one place a linked session is closed. Withdrawing the terminal URL is not
/// a second step here — ending the session is what withdraws it (see
/// `SessionClient::set_status`), so the row cannot end up closed-but-watchable or
/// watchable-but-closed. Best effort: cloud being unreachable costs a stale row,
/// not a failed command.
async fn finish(client: &SessionClient, id: Option<&str>, out: Result<()>) -> Result<()> {
    if let Some(id) = id {
        let _ = client.set_status(id, Status::of(out.is_ok())).await;
    }
    // The tmux session outlives the link. Leaving the URL up would advertise a
    // tunnel that stopped answering the moment this returned.
    pin(None).await;
    out
}

/// Resolve when the OS asks this process to stop.
///
/// Ctrl-C is the portable one, but a link far more often ends by SIGHUP — the
/// terminal window closed — or SIGTERM from a logout or a supervisor. A signal we
/// cannot register for is one that simply never arrives, which is not the same as
/// being asked to stop, so it waits forever instead of reporting a false ending.
#[cfg(unix)]
async fn stopped() {
    use tokio::signal::unix::{signal, SignalKind};
    async fn on(kind: SignalKind) {
        match signal(kind) {
            Ok(mut s) => {
                s.recv().await;
            }
            Err(_) => std::future::pending().await,
        }
    }
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {}
        _ = on(SignalKind::hangup()) => {}
        _ = on(SignalKind::terminate()) => {}
    }
}

#[cfg(not(unix))]
async fn stopped() {
    let _ = tokio::signal::ctrl_c().await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::code::testmock::MockCloud;
    use anyhow::anyhow;

    /// A link that ended cleanly is DONE — and saying so is what stops the row
    /// from outliving the shell. The ending is one PATCH carrying both facts, so
    /// the console can never see a finished session still advertising a terminal.
    #[tokio::test]
    async fn a_clean_exit_closes_the_session_and_withdraws_the_terminal() {
        let mock = MockCloud::start().await;
        let client = SessionClient::new(&mock.base_url(), "T").unwrap();

        finish(&client, Some("sess_1"), Ok(())).await.unwrap();

        let reqs = mock.requests();
        assert_eq!(reqs.len(), 1, "one act, one request");
        assert_eq!(reqs[0].method, "PATCH");
        assert_eq!(reqs[0].path, "/v1/agents/sessions/sess_1");
        assert_eq!(reqs[0].json()["status"], "done");
        assert_eq!(reqs[0].json()["terminal"], "");
    }

    /// The failing exits are the ones that used to leak: a tunnel that died, a
    /// publish that never landed, a shell that could not start. They END the
    /// session too — as an error — and the caller still gets its error back
    /// unchanged, because recording an ending must not swallow one.
    #[tokio::test]
    async fn a_failed_link_ends_as_an_error_and_still_reports_it() {
        let mock = MockCloud::start().await;
        let client = SessionClient::new(&mock.base_url(), "T").unwrap();

        let err = finish(&client, Some("sess_1"), Err(anyhow!("share ended: exit 1")))
            .await
            .unwrap_err();

        assert!(err.to_string().contains("share ended"), "got: {err}");
        let reqs = mock.requests();
        assert_eq!(reqs[0].json()["status"], "error");
        assert_eq!(reqs[0].json()["terminal"], "");
    }

    /// Cloud being unreachable costs a stale row, never a failed command: the
    /// shell already ran, and refusing to return its result because a PATCH 403'd
    /// would be reporting the wrong thing.
    #[tokio::test]
    async fn a_control_plane_that_refuses_the_close_does_not_fail_the_link() {
        let mock = MockCloud::start_status(403).await;
        let client = SessionClient::new(&mock.base_url(), "T").unwrap();

        assert!(finish(&client, Some("sess_1"), Ok(())).await.is_ok());
    }

    // ONE LINK, MANY SHELLS. The default runs a wrapper rather than a fixed tmux
    // command, so a pane can ask for a shell by name over the SAME tunnel — one
    // port, one gate sign-in, N independent tmux sessions. Proven against ttyd
    // 1.7.7: connecting with ?arg=alpha and ?arg=beta created two separate
    // sessions from one server.
    #[test]
    fn the_default_serves_a_shell_the_url_can_name() {
        let sh = Shell::from_flag(None);
        assert!(sh.url_arg(), "the query has to be able to name a shell");
        let argv = sh.argv();
        // `sh -c SCRIPT $0` — the placeholder is what puts the client's arg in $1.
        assert_eq!(argv[0], "sh");
        assert_eq!(argv[1], "-c");
        assert_eq!(argv[3], "hanzo-shell", "a $0 placeholder, so the arg lands in $1");
        assert!(argv[2].contains("tmux new -A -s"), "{}", argv[2]);
    }

    // `tmux` names what the default already is; it must not become a second way.
    #[test]
    fn asking_for_tmux_asks_for_the_default() {
        assert_eq!(Shell::from_flag(Some("tmux")).argv(), Shell::from_flag(None).argv());
    }

    // A NAMED COMMAND TAKES NO URL ARGUMENT, and this is the security-critical
    // half. `--url-arg` appends the query to argv, so `--shell bash` plus
    // `?arg=-c&arg=whoami` would be `bash -c whoami` — remote execution handed
    // over by a query string. The wrapper is written to be given arguments; a raw
    // command is not, so it is never offered them.
    #[test]
    fn a_named_shell_is_never_given_the_url() {
        for cmd in ["bash", "zsh", "fish"] {
            let sh = Shell::from_flag(Some(cmd));
            assert_eq!(sh.argv(), vec![cmd.to_string()]);
            assert!(!sh.url_arg(), "`--shell {cmd}` must not read the query");
        }
    }

    // The name reaching tmux is bounded BEFORE tmux sees it: `;` is tmux's own
    // command separator, and --url-arg lets a caller send several args.
    #[test]
    fn the_wrapper_strips_a_name_down_to_something_safe() {
        let script = &Shell::from_flag(None).argv()[2];
        assert!(script.contains("tr -cd"), "the name must be reduced to a rune set");
        assert!(script.contains("cut -c1-32"), "and bounded in length");
        assert!(script.contains("${1:-hanzo}"), "and read from $1 alone");
        assert!(!script.contains("$2"), "extra arguments are ignored, never used");
    }

}
