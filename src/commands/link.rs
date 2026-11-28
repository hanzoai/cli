//! `hanzo link` — the one way to link a computer to Hanzo cloud.
//!
//! A linked machine is three things, and this module is all of them:
//!
//! - ON THE ORG'S NETWORK. Hanzo ZT (hanzozt/zt) is the zero-trust fabric, and a
//!   machine is on it as its IAM subject: cloud makes sure that identity exists
//!   (`network::Caller::ensure`) and the `zt` tunnel signs in with the same token.
//!   `link host` publishes a service and hosts it, `link dial` carries a local port
//!   to one, `link status` says what is up, `link rm` takes a service down, and
//!   `--install` leaves either tunnel running under the service manager.
//! - A MACHINE THE WEB MAY USE, folder by folder: `link grant` lets runs started
//!   from the web work in a folder of it, `link revoke` stops that, `link folders`
//!   lists them (`folders`).
//!
//!   HOSTING AND DIALING ARE TWO IDENTITIES. zt has no dial-only mode: `tunnel
//!   proxy` also hosts everything its identity may bind, so a dialer that may
//!   bind turns into a second host. `link dial` asks the controller what its
//!   identity may bind and refuses one that may bind anything; on a machine
//!   that hosts, the dial signs in as a separate identity (`--token-command`).
//! - A RUN-TARGET, so the fleet sees its CPU and GPUs and can send it work.
//! - A SHELL the console can drive: ttyd serves one over a loopback port,
//!   `share::start` publishes that port (the same tunnel `hanzo share` uses), and
//!   the session registry gets a row carrying the URL.
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

mod dependencies;
pub mod folders;
mod network;
mod unit;
mod zt;

pub use network::Caller;

use crate::commands::share;
use crate::config::Config;
use crate::iam::paths::DEFAULT_BRAND;
use anyhow::{bail, Context, Result};
use colored::*;
use hanzo_gen::event::Status;
use hanzo_gen::session::SessionClient;
use hanzo_gen::{context, target, Root};
use std::future::Future;
use std::path::PathBuf;
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
            Shell::Multiplexed => vec!["sh".into(), "-c".into(), MUX.into(), "hanzo-shell".into()],
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

    let ttyd = hanzo_gen::exe::find("ttyd").context("ttyd is not installed (brew install ttyd)")?;
    let mut c = Command::from(hanzo_gen::exe::command(ttyd));
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

    // Refreshing accessor, not the raw one: a link holds a shell for hours, and
    // the access token lives one.
    let caller = Caller::sign_in(cfg, None).await?;
    let api = caller.api.clone();

    // On the org's network as its IAM subject: what lets this machine dial the
    // org's services and be made the host of one. Best-effort, like the registry
    // row below — a network that cannot take the identity does not take the
    // shell with it.
    let joined = async {
        let session = zt::Session::open(&caller.token).await?;
        let name = join(&caller, session.as_ref()).await;
        if let Some(s) = session {
            s.close().await;
        }
        name
    };
    match joined.await {
        Ok(name) => println!(
            "{} on {}'s network as {name}",
            "→".green(),
            caller.org.cyan()
        ),
        Err(e) => crate::warn(&format!(
            "could not put this machine on {}'s network ({e})",
            caller.org
        )),
    }

    // Hold this machine open as a run-target, so the fleet knows its CPU and GPUs
    // and the console has a machine to group the shell under. A BEAT, not a single
    // register: cloud decides liveness from when a machine last wrote, so a link
    // that announced itself once and went quiet reads offline while the shell it
    // published is still serving. The guard beats until this command returns —
    // detached and best-effort, never on the critical path of getting a shell up.
    let root = Root::need()?;
    let host = context::hostname(&root);
    let (machine, _runner) = target::beat_and_serve(
        crate::commands::dev::Store::host(cfg.clone()),
        &root,
        DEFAULT_BRAND,
        &api,
        &context::machine_id(&root),
        &host,
    );

    // Fail loudly if another `hanzo link` already holds the loopback shell port.
    if port_taken(TTYD_PORT) {
        bail!(
            "port {TTYD_PORT} is already serving — another `hanzo link` is almost certainly \
             running on this machine. One link per machine: end that one first, or \
             attach to the shell it already published with `tmux attach -t hanzo`."
        );
    }

    // Browser terminal is optional: if ttyd/tmux or the share tunnel is unavailable
    // on this workstation, keep the ZT identity, fleet heartbeat, and live Dev
    // runner active instead of tearing down the whole link.
    let missing = dependencies::missing(matches!(sh_kind, Shell::Multiplexed), |bin| {
        hanzo_gen::exe::find(bin).is_some()
    });
    if !missing.is_empty() {
        crate::warn(&format!(
            "browser terminal skipped (missing: {}). {}",
            missing.join(", "),
            dependencies::install_help()
        ));
        return hold_runner(&host).await;
    }

    // ttyd next: publishing a port nothing is serving would announce a URL that
    // 502s, which reads as "the fabric is broken" rather than "the shell died".
    let ttyd = match start_ttyd(TTYD_PORT, &sh_kind, !read_only).await {
        Ok(t) => t,
        Err(e) => {
            crate::warn(&format!(
                "browser terminal unavailable ({e}); continuing with live runner only"
            ));
            return hold_runner(&host).await;
        }
    };
    println!("{} {}", "→".green(), sh_kind.label().cyan());

    // WHOSE SHELL THIS IS, said out loud. A terminal reachable by anyone who
    // learns the URL is not something to opt in to protecting — and neither is
    // one reachable by anyone with an account. The address comes off the token
    // already in hand, and the frontend checks it against what hanzo.ai says
    // about whoever turns up, so this is a claim about the publisher rather than
    // a decision made here.
    let Some(email) = crate::iam::identity::email(&caller.token) else {
        drop(ttyd);
        crate::warn(
            "this identity carries no email address, so the browser terminal cannot be published; \
             continuing with live runner only (add an email to your Hanzo identity and run `hanzo auth login` to publish a shell)",
        );
        return hold_runner(&host).await;
    };
    let mut sh = match share::start(
        cfg,
        format!("http://127.0.0.1:{TTYD_PORT}"),
        "proxy".into(),
        None,
        Some(share::Owner {
            provider: "hanzo",
            email: &email,
        }),
    )
    .await
    {
        Ok(sh) => sh,
        Err(e) => {
            drop(ttyd);
            crate::warn(&format!(
                "browser terminal share unavailable ({e}); continuing with live runner only"
            ));
            return hold_runner(&host).await;
        }
    };
    let _ttyd = ttyd;

    // Register the session LAST, so the row never advertises a URL that is not
    // yet answering. The host is the SAME value the run-target above registered
    // under, which is what lets the console file this shell under that machine
    // instead of under nothing.
    let client = SessionClient::new(&api, &caller.token)?.with_org(caller.selected());
    let cwd = std::env::current_dir()
        .map(|p| p.display().to_string())
        .unwrap_or_default();
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
    let tmux = Tmux::default();
    let url = sh.url.clone();
    let out = on_machine(&machine, &url, serve(&client, listing.as_deref(), &mut sh, &tmux)).await;
    finish(&tmux, &client, listing.as_deref(), out).await
}

/// Hold `url` on the machine's own run-target for as long as `held` runs.
///
/// The terminal is a fact about the MACHINE, so it rides the beat that already
/// keeps the machine live: cloud stops a session an hour after its last activity,
/// and the shell is still serving then. The session row keeps carrying the same
/// url, which Tabs older than the machine field read.
async fn on_machine<T>(machine: &target::Beat, url: &str, held: impl Future<Output = T>) -> T {
    machine.terminal(Some(url));
    let out = held.await;
    // Heard before the guard drops: dropping it stops the beat that would say it.
    machine.withdraw().await;
    out
}

/// The tmux server a link works in: the user's own (its default socket), or one
/// at a socket of its own — what a test runs, so a test never touches the
/// session a person is working in.
#[derive(Clone, Default)]
pub(super) struct Tmux(pub(super) Option<PathBuf>);

impl Tmux {
    /// tmux, by its full path (`exe::find`), at this server's socket. `None` when
    /// tmux is not installed: nothing is run by a bare name.
    fn command(&self) -> Option<Command> {
        let mut c = Command::from(hanzo_gen::exe::command(hanzo_gen::exe::find("tmux")?));
        if let Some(socket) = &self.0 {
            c.arg("-S").arg(socket);
        }
        Some(c)
    }
}

/// Publish the shell and hold it until the link ends.
///
/// One function owns every ending so that one caller can record it: the shell
/// exiting, the tunnel dying under it, a publish that never landed, or the OS
/// asking this process to stop. None of those is "still running", and until now
/// only the first two returned at all.
/// `id` is `None` when the registry refused the row: everything the console needs
/// is skipped and everything the SHELL needs still happens.
async fn serve(
    client: &SessionClient,
    id: Option<&str>,
    sh: &mut share::Share,
    tmux: &Tmux,
) -> Result<()> {
    let url = sh.url.clone();

    // Wear the URL. Part of PUBLISHING, not of attaching — see `pin`.
    pin(tmux, Some(&url)).await;

    let _where = match id {
        Some(id) => {
            // Publishing the terminal is best-effort for the same reason the row
            // is: the tunnel already answers at this URL whether or not cloud
            // records it.
            if let Err(e) = client.publish_terminal(id, &url).await {
                crate::warn(&format!("could not publish the terminal URL ({e})"));
            }
            // Follow the shell. Held for exactly this session's lifetime.
            Some(follow(client.clone(), id.to_string(), tmux.clone()))
        }
        None => None,
    };

    println!("\n  {}  →  live\n", sh.url.green().bold());
    match id {
        Some(id) => println!("  {} {}", "session".dimmed(), id.dimmed()),
        None => println!(
            "  {}",
            "unlisted — the console cannot show this one".dimmed()
        ),
    }

    // Hand the caller a prompt on the SAME session ttyd is serving, rather than
    // making them wait on a tunnel they cannot type into. Both ends attach to one
    // tmux session, so what is typed here appears there and the reverse.
    //
    // When tmux will not take the terminal, the link is NOT over — the tunnel is
    // still serving and the browser can still drive it — so hold it instead.
    let held = async {
        if took_over(attach(tmux).await) {
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
fn follow(client: SessionClient, id: String, tmux: Tmux) -> Where {
    Where(tokio::spawn(async move {
        let mut last = String::new();
        loop {
            if let Some(now) = active_path(&tmux)
                .await
                .filter(|p| worth_reporting(&last, p))
            {
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
pub(super) fn worth_reporting(last: &str, now: &str) -> bool {
    !now.is_empty() && now != last
}

/// Where the shared session's active pane is, as tmux reports it.
pub(super) async fn active_path(tmux: &Tmux) -> Option<String> {
    let out = tmux
        .command()?
        .args([
            "display-message",
            "-p",
            "-t",
            "hanzo",
            "#{pane_current_path}",
        ])
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
pub(super) async fn pin(tmux: &Tmux, url: Option<&str>) {
    let Some(mut new) = tmux.command() else {
        return;
    };
    let _ = new
        .args(["new-session", "-d", "-s", "hanzo"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .await;
    for args in bar_args(url) {
        let Some(mut set) = tmux.command() else {
            return;
        };
        let _ = set
            .args(&args)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .await;
    }
}

/// The tmux options that put `url` on the bar, or clear it when there is none.
pub(super) fn bar_args(url: Option<&str>) -> [Vec<String>; 2] {
    let bar = url.map(|u| format!(" {u} ")).unwrap_or_default();
    [
        vec![
            "set-option".into(),
            "-t".into(),
            "hanzo".into(),
            "status-right".into(),
            bar,
        ],
        vec![
            "set-option".into(),
            "-t".into(),
            "hanzo".into(),
            "status-right-length".into(),
            "80".into(),
        ],
    ]
}

/// Put the caller on the same tmux session ttyd serves, and report how it exited.
///
/// `None` means tmux could not be spawned at all.
async fn attach(tmux: &Tmux) -> Option<i32> {
    tmux.command()?
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
pub(super) fn took_over(code: Option<i32>) -> bool {
    code == Some(0)
}

/// Record how this link ended, then hand the ending back unchanged.
///
/// THE one place a linked session is closed. Withdrawing the terminal URL is not
/// a second step here — ending the session is what withdraws it (see
/// `SessionClient::set_status`), so the row cannot end up closed-but-watchable or
/// watchable-but-closed. Best effort: cloud being unreachable costs a stale row,
/// not a failed command.
async fn hold_runner(host: &str) -> Result<()> {
    println!(
        "{} live runner active for {} — ready for Dev runs (Ctrl-C leaves)",
        "→".green(),
        host.cyan()
    );
    stopped().await;
    Ok(())
}

pub(super) async fn finish(
    tmux: &Tmux,
    client: &SessionClient,
    id: Option<&str>,
    out: Result<()>,
) -> Result<()> {
    if let Some(id) = id {
        let _ = match &out {
            Ok(()) => client.set_status(id, Status::Done).await,
            Err(e) => client.fail(id, &format!("{e:#}")).await,
        };
    }
    // The tmux session outlives the link. Leaving the URL up would advertise a
    // tunnel that stopped answering the moment this returned.
    pin(tmux, None).await;
    out
}

/// Resolve when the OS asks this process to stop.
///
/// Ctrl-C is the portable one, but a link far more often ends by SIGHUP — the
/// terminal window closed — or SIGTERM from a logout or a supervisor. A signal we
/// cannot register for is one that simply never arrives, which is not the same as
/// being asked to stop, so it waits forever instead of reporting a false ending.
#[cfg(unix)]
pub(super) async fn stopped() {
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
pub(super) async fn stopped() {
    let _ = tokio::signal::ctrl_c().await;
}

// ---- the network: host, dial, status, rm ------------------------------------


mod fabric;
pub use fabric::{dial, host, publish, rm, status, Install, Signer};
use fabric::join;

#[cfg(test)]
mod tests;
