//! Zero-Trust service hosting, dialing, supervision, and status (`hanzo link host|dial|status|rm`).

use crate::config::Config;
use crate::iam::paths::DEFAULT_BRAND;
use crate::iam::store;
use anyhow::{anyhow, bail, Context, Result};
use colored::*;
use std::time::Duration;

use super::{network, stopped, unit, zt, Caller};

/// Who a tunnel, and the calls before it, sign in as.
#[derive(clap::Args, Debug, Clone, Default)]
pub struct Signer {
    /// Sign in with what this command prints instead of your hanzo.id session: a
    /// machine's own IAM client (e.g. /etc/hanzo/link/token). Run without a shell
    /// and split on spaces, as zt runs it
    #[arg(long, value_name = "CMD")]
    pub token_command: Option<String>,
}

impl Signer {
    /// The command the tunnel signs in with: the one given, else this binary's
    /// own `auth token` — the identity every other command here speaks as.
    pub(super) fn tunnel_command(&self, cfg: &Config) -> Result<String> {
        if let Some(cmd) = &self.token_command {
            return Ok(cmd.clone());
        }
        let exe = std::env::current_exe().context("resolving our own binary")?;
        let exe = exe.to_str().context("this binary's path is not UTF-8")?;
        if exe.contains(char::is_whitespace) {
            bail!("{exe} has a space in it, and zt splits its token command on spaces: pass --token-command");
        }
        // `--as` rides along only when it names an identity held in that org;
        // otherwise the default signs in and the org stays a selection.
        Ok(match store::within(cfg, DEFAULT_BRAND).ok().flatten() {
            Some(id) => format!("{exe} --as {} auth token", id.owner),
            None => format!("{exe} auth token"),
        })
    }
}

/// Whether a tunnel runs here and now or under the service manager.
#[derive(clap::Args, Debug, Clone, Default)]
pub struct Install {
    /// Keep it running: write and start a systemd unit (a launchd job on macOS)
    #[arg(long)]
    pub install: bool,
    /// With --install: a system unit that starts at boot (run it with sudo)
    #[arg(long, requires = "install")]
    pub system: bool,
}

/// `hanzo link host [NAME HOST:PORT]` — host this identity's services on the
/// network. With a name, publish it first and take its host role.
pub async fn host(
    cfg: &mut Config,
    publish: Option<(String, String)>,
    signer: Signer,
    install: Install,
) -> Result<()> {
    if let Some((name, target)) = publish {
        let caller = Caller::sign_in(cfg, signer.token_command.as_deref()).await?;
        let dns = self::publish(&caller, &name, &target).await?;
        println!("{} {} → {target}", "✓".green(), dns.cyan().bold());
    }
    let what = "host this identity's services on Hanzo ZT".to_string();
    tunnel(
        cfg,
        &signer,
        &install,
        "host",
        what,
        zt::Mode::Host,
        vec!["host".into()],
    )
    .await
}

/// Make sure the caller is on its org's network, unless the platform holds its
/// identity: those roles are universe's `link-fabric.sh` to set, and an org role
/// would put the identity in reach of a tenant steward's DELETE. `session` is the
/// caller's own, `None` when it has no identity on the fabric yet. Returns the
/// identity's name.
pub(super) async fn join(caller: &Caller, session: Option<&zt::Session>) -> Result<String> {
    if let Some(s) = session {
        let me = s.me().await?;
        if me.platform() {
            return Ok(format!("{} (the platform's)", me.name));
        }
    }
    Ok(caller.ensure(&[]).await?.name)
}

/// Put `name` on the caller's org network, forwarding to `target`, and make the
/// caller its host. Idempotent. Returns the name the fabric answers at.
pub async fn publish(caller: &Caller, name: &str, target: &str) -> Result<String> {
    let name = network::label(name)?;
    let (host, port) = network::host_port(target)?;
    if let Some(s) = zt::Session::open(&caller.token).await? {
        let me = s.me().await;
        s.close().await;
        let me = me?;
        if me.platform() {
            bail!(
                "{} is the platform's identity, and its roles are universe's link-fabric.sh to set: \
                 host what it is bound to with `hanzo link host` alone, or publish as an org's identity",
                me.name
            );
        }
    }
    let fqn = format!("{name}.{}", caller.org);
    let dns = if caller.services().await?.iter().any(|s| s.service == fqn) {
        println!(
            "{} {fqn} is already published (`hanzo link rm {name}` first to point it elsewhere)",
            "·".dimmed()
        );
        format!("{fqn}.zt")
    } else {
        caller.publish(&name, &host, port).await?.dns
    };
    // The role names the service, and cloud refuses one for a service the org
    // does not have — so it is taken after the publish, never before.
    caller.ensure(&[format!("{name}-host")]).await?;
    Ok(dns)
}

/// `hanzo link dial SERVICE PORT` — carry local `PORT` to a service.
pub async fn dial(
    cfg: &mut Config,
    service: String,
    port: u16,
    signer: Signer,
    install: Install,
) -> Result<()> {
    let caller = Caller::sign_in(cfg, signer.token_command.as_deref()).await?;
    let fqn = caller.scope(&service)?;
    // The org's own service admits the org's identities, so make sure this one is
    // among them. A service the org does not list is the platform's or another
    // org's: its own policy decides who dials it, and the identity is left alone.
    let listed = match caller.services().await {
        Ok(list) => list.iter().any(|s| s.service == fqn),
        Err(e) => {
            crate::warn(&format!(
                "could not read {}'s services ({e}); dialing {fqn} anyway",
                caller.org
            ));
            false
        }
    };
    let who = format!("{}/{}", caller.who.owner, caller.who.name);
    let mut session = zt::Session::open(&caller.token).await?;
    if listed {
        join(&caller, session.as_ref()).await?;
        if session.is_none() {
            session = zt::Session::open(&caller.token).await?;
        }
    }
    let session = session.ok_or_else(|| {
        anyhow!(
            "{who} has no identity on Hanzo ZT, and {fqn} is not {}'s to put it there",
            caller.org
        )
    })?;
    let checked = session.dial_only(&who).await;
    session.close().await;
    checked?;
    let (id, what) = (format!("dial-{fqn}"), format!("dial {fqn} on :{port}"));
    let verb = vec!["dial".into(), fqn.clone(), port.to_string()];
    tunnel(
        cfg,
        &signer,
        &install,
        &id,
        what,
        zt::Mode::Proxy { service: fqn, port },
        verb,
    )
    .await
}

/// Run a tunnel here, supervised, or install a unit that runs this same command.
///
/// `verb` is the command that runs the tunnel with nothing left to publish
/// (`host`, or `dial SERVICE PORT`): a unit runs `hanzo link <verb>
/// --token-command CMD`, so it keeps the supervisor — zt alone does not rebind
/// after the controller restarts under it.
#[allow(clippy::too_many_arguments)]
async fn tunnel(
    cfg: &Config,
    signer: &Signer,
    install: &Install,
    id: &str,
    description: String,
    mode: zt::Mode,
    verb: Vec<String>,
) -> Result<()> {
    let bin = zt::resolve_or_install().await?;
    let command = signer.tunnel_command(cfg)?;
    if !install.install {
        if let zt::Mode::Proxy { port, .. } = &mode {
            println!(
                "{} listening on every interface at :{port} (zt's proxy mode)",
                "→".green()
            );
        }
        let guard = match &mode {
            zt::Mode::Host => Guard::Host(zt::Watch::new(&command)),
            zt::Mode::Proxy { port, .. } => Guard::Proxy(*port),
        };
        return supervise(&bin, &zt::args(&mode, &command), guard).await;
    }
    // A unit that can never sign in is not worth installing.
    network::run_token_command(&command)
        .await
        .context("checking the tunnel's token command")?;
    let exe = std::env::current_exe().context("resolving our own binary")?;
    let scope = if install.system {
        unit::Scope::System
    } else {
        unit::Scope::User
    };
    if scope == unit::Scope::System && !unit::root_only(&exe) {
        crate::warn(&format!(
            "a system unit runs {} as root, and a user other than root can write it",
            exe.display()
        ));
    }
    let argv = [exe.display().to_string(), "link".into()]
        .into_iter()
        .chain(verb)
        .chain(["--token-command".to_string(), command])
        .collect();
    let u = unit::Unit {
        id: id.to_string(),
        description,
        argv,
        source: invocation(),
    };
    let file = unit::install(&u, scope)?;
    let name = if cfg!(target_os = "macos") {
        u.launchd_label()
    } else {
        u.systemd_name()
    };
    println!(
        "{} {} → {}",
        "✓".green(),
        name.cyan().bold(),
        file.display()
    );
    if unit::lingers(scope) == Some(false) {
        println!("  it starts at login; `loginctl enable-linger` starts it at boot");
    }
    Ok(())
}

/// How long zt is given to come up before its guard starts asking.
const GRACE: Duration = Duration::from_secs(45);
/// How often a guard asks.
const POLL: Duration = Duration::from_secs(20);
/// The first wait before zt starts again, doubled per failure up to the last.
const BACKOFF: (Duration, Duration) = (Duration::from_secs(1), Duration::from_secs(60));
/// A run this long was healthy, so the next failure starts the backoff over.
const STEADY: Duration = Duration::from_secs(300);

/// What a supervised tunnel watches beside zt's own exit.
///
/// zt does not always recover on its own: after a controller restart a host's
/// service refresh can fail mid-restart and never be retried, leaving it running
/// with no terminator, and a proxy closes its listener for good when its service
/// leaves view. Both look alive from outside, so each is asked about directly.
enum Guard {
    /// Every service this identity may host has a terminator on the fabric.
    Host(zt::Watch),
    /// The local port still listens.
    Proxy(u16),
}

impl Guard {
    /// Resolve with the reason to start zt again: after [`GRACE`], two failed
    /// checks in a row, [`POLL`] apart. A check that could not be made decides
    /// nothing.
    async fn lost(&mut self) -> String {
        tokio::time::sleep(GRACE).await;
        let mut misses = 0;
        loop {
            let finding = match self {
                Guard::Host(watch) => match watch.unhosted().await {
                    Ok(none) if none.is_empty() => Some(None),
                    Ok(bare) => Some(Some(format!("{} has no host on Hanzo ZT", bare.join(", ")))),
                    Err(e) => {
                        tracing::debug!("reading this identity's services: {e:#}");
                        None
                    }
                },
                Guard::Proxy(port) => {
                    listening(*port).map(|up| (!up).then(|| format!(":{port} stopped listening")))
                }
            };
            match finding {
                Some(Some(why)) => {
                    misses += 1;
                    if misses >= 2 {
                        return why;
                    }
                }
                Some(None) => misses = 0,
                None => {}
            }
            tokio::time::sleep(POLL).await;
        }
    }
}

/// Run zt until the link is asked to stop, and start it again — with backoff —
/// whenever it exits or its guard finds it lost.
async fn supervise(bin: &std::path::Path, args: &[String], mut guard: Guard) -> Result<()> {
    let stop = stopped();
    tokio::pin!(stop);
    let mut backoff = BACKOFF.0;
    loop {
        let started = std::time::Instant::now();
        let mut child = tokio::process::Command::from(hanzo_gen::exe::command(bin))
            .args(args)
            .kill_on_drop(true)
            .spawn()
            .with_context(|| format!("starting {}", bin.display()))?;
        let why = tokio::select! {
            biased;
            _ = &mut stop => {
                let _ = child.kill().await;
                return Ok(());
            }
            status = child.wait() => match status {
                Ok(s) => format!("zt exited ({s})"),
                Err(e) => format!("zt could not be waited on ({e})"),
            },
            why = guard.lost() => why,
        };
        let _ = child.kill().await;
        if started.elapsed() >= STEADY {
            backoff = BACKOFF.0;
        }
        crate::warn(&format!(
            "{why}: starting zt again in {}s",
            backoff.as_secs()
        ));
        tokio::select! {
            biased;
            _ = &mut stop => return Ok(()),
            _ = tokio::time::sleep(backoff) => {}
        }
        backoff = (backoff * 2).min(BACKOFF.1);
    }
}

/// Whether something listens on `port`, from the kernel's own socket tables.
/// `None` where those cannot be read, which is every system but Linux.
fn listening(port: u16) -> Option<bool> {
    let tables: Vec<String> = ["/proc/net/tcp", "/proc/net/tcp6"]
        .iter()
        .filter_map(|t| std::fs::read_to_string(t).ok())
        .collect();
    if tables.is_empty() {
        return None;
    }
    Some(tables.iter().any(|t| listens_in(t, port)))
}

/// Whether a `/proc/net/tcp` table holds a LISTEN socket (state `0A`) on `port`.
pub(super) fn listens_in(table: &str, port: u16) -> bool {
    let want = format!("{port:04X}");
    table.lines().skip(1).any(|l| {
        let mut f = l.split_whitespace();
        let local = f.nth(1).unwrap_or_default();
        let state = f.nth(1).unwrap_or_default();
        state == "0A" && local.rsplit(':').next() == Some(want.as_str())
    })
}

/// The command line that ran, as a person would type it again.
fn invocation() -> String {
    std::iter::once("hanzo".to_string())
        .chain(std::env::args().skip(1).map(|a| {
            if a.is_empty() || a.contains(char::is_whitespace) {
                format!("'{a}'")
            } else {
                a
            }
        }))
        .collect::<Vec<_>>()
        .join(" ")
}

/// `hanzo link status` — this identity on the network, what the org publishes,
/// and which tunnels run here.
pub async fn status(cfg: &mut Config, signer: Signer) -> Result<()> {
    let caller = Caller::sign_in(cfg, signer.token_command.as_deref()).await?;
    println!(
        "{} {}/{} in {}",
        "identity".bold(),
        caller.who.owner,
        caller.who.name,
        caller.org.cyan()
    );
    match caller.identities().await {
        Ok(ids) => match ids.iter().find(|i| i.external_id == caller.sub) {
            Some(i) => println!("  on {}'s network as {} [{}]", caller.org, i.name.cyan(), i.roles.join(", ")),
            None => println!(
                "  not in {}'s list: `hanzo link` joins it (an identity the platform holds is never listed)",
                caller.org
            ),
        },
        Err(e) => println!("  {} {e}", "unreadable:".red()),
    }
    println!("{}", "services".bold());
    match caller.services().await {
        Ok(s) if s.is_empty() => println!("  none published by {}", caller.org),
        Ok(s) => s
            .iter()
            .for_each(|svc| println!("  {}  {}.zt", svc.service.cyan(), svc.service)),
        Err(e) => println!("  {} {e}", "unreadable:".red()),
    }
    println!("{}", "tunnels".bold());
    let ps = hanzo_gen::exe::find("ps").and_then(|ps| {
        hanzo_gen::exe::command(ps)
            .args(["-eo", "pid=,args="])
            .output()
            .ok()
    });
    let running = ps
        .map(|o| tunnels(&String::from_utf8_lossy(&o.stdout)))
        .unwrap_or_default();
    if running.is_empty() {
        println!("  no zt tunnel is running here");
    }
    for t in &running {
        let listening = t.port.map(|p| {
            let up = std::net::TcpStream::connect_timeout(
                &([127, 0, 0, 1], p).into(),
                Duration::from_millis(500),
            )
            .is_ok();
            if up {
                format!(" :{p} listening").green()
            } else {
                format!(" :{p} not listening").red()
            }
        });
        println!(
            "  pid {}  {}{}",
            t.pid,
            t.what,
            listening.map(|l| l.to_string()).unwrap_or_default()
        );
    }
    for (name, scope) in unit::installed() {
        let state = unit::state(&name, scope);
        let painted = if state == "active" || state == "loaded" {
            state.green()
        } else {
            state.yellow()
        };
        println!(
            "  {name} ({}) {painted}",
            if scope == unit::Scope::System {
                "system"
            } else {
                "user"
            }
        );
    }
    Ok(())
}

/// A running `zt tunnel`, read off the process table.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct Tunnel {
    pub(super) pid: u32,
    /// `host`, or `proxy k8s.hanzo:26443`.
    pub(super) what: String,
    /// The local port a proxy listens on.
    pub(super) port: Option<u16>,
}

/// Every `zt tunnel …` in `ps -eo pid=,args=` output.
pub(super) fn tunnels(ps: &str) -> Vec<Tunnel> {
    ps.lines()
        .filter_map(|line| {
            let mut words = line.split_whitespace();
            let pid = words.next()?.parse().ok()?;
            let bin = words.next()?;
            if std::path::Path::new(bin).file_name()? != "zt" || words.next()? != "tunnel" {
                return None;
            }
            let mode = words.next()?;
            let pairs: Vec<&str> = words.take_while(|w| !w.starts_with('-')).collect();
            let port = pairs
                .first()
                .and_then(|p| p.rsplit(':').next()?.parse().ok());
            let what = std::iter::once(mode)
                .chain(pairs)
                .collect::<Vec<_>>()
                .join(" ");
            Some(Tunnel { pid, what, port })
        })
        .collect()
}

/// `hanzo link rm NAME` — take a published service off the org's network.
pub async fn rm(cfg: &mut Config, name: String, signer: Signer) -> Result<()> {
    let caller = Caller::sign_in(cfg, signer.token_command.as_deref()).await?;
    let fqn = caller.scope(&name)?;
    let svc = caller
        .services()
        .await?
        .into_iter()
        .find(|s| s.service == fqn)
        .ok_or_else(|| anyhow!("{fqn} is not published on {}'s network", caller.org))?;
    caller.unpublish(&svc.id).await?;
    println!("{} {fqn} is off {}'s network", "✓".green(), caller.org);
    Ok(())
}

