//! `--install`: a link that outlives the shell that made it — a systemd unit on
//! Linux (user, or system with `--system`), a launchd job on macOS — written in
//! the style of `units/hanzo-beat.service` and enabled at once.
//!
//! One id names a unit on both systems: `hanzo-link-<id>.service` for systemd,
//! `com.ai.hanzo.link.<id>` for launchd. Installing the same id again rewrites
//! the unit and restarts it, so re-running a command is how a unit is changed.

use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};
use std::process::Command;

/// Where a unit runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    /// The signed-in user's service manager.
    User,
    /// The machine's — root's to write.
    System,
}

/// One service, as both service managers see it.
#[derive(Debug, Clone)]
pub struct Unit {
    /// `host`, or `dial-<service>`.
    pub id: String,
    /// What it does, in a few words.
    pub description: String,
    /// The command it runs, absolute path first.
    pub argv: Vec<String>,
    /// The `hanzo` invocation that wrote it — the way to change it.
    pub source: String,
}

impl Unit {
    pub fn systemd_name(&self) -> String {
        format!("hanzo-link-{}.service", self.id)
    }

    pub fn launchd_label(&self) -> String {
        format!("com.ai.hanzo.link.{}", self.id)
    }

    /// The systemd unit file.
    pub fn systemd(&self, scope: Scope) -> String {
        let exec = self.argv.iter().map(|a| systemd_arg(a)).collect::<Vec<_>>().join(" ");
        let wanted = match scope {
            Scope::User => "default.target",
            Scope::System => "multi-user.target",
        };
        format!(
            "# Written by `{source}`. Run it again to change this unit.\n\
             [Unit]\n\
             Description=Hanzo link: {description}\n\
             After=network-online.target\n\
             Wants=network-online.target\n\
             \n\
             [Service]\n\
             ExecStart={exec}\n\
             Restart=always\n\
             RestartSec=2\n\
             \n\
             [Install]\n\
             WantedBy={wanted}\n",
            source = self.source,
            description = self.description,
        )
    }

    /// The launchd job.
    pub fn launchd(&self) -> String {
        let label = self.launchd_label();
        let args: String = self.argv.iter().map(|a| format!("<string>{}</string>", xml(a))).collect();
        let log = xml(&format!("/tmp/{label}.log"));
        format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
             <!-- Written by `{source}`. Run it again to change this job. -->\n\
             <!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
             <plist version=\"1.0\">\n\
             <dict>\n\
             \x20 <key>Label</key><string>{label}</string>\n\
             \x20 <key>ProgramArguments</key>\n\
             \x20 <array>{args}</array>\n\
             \x20 <key>RunAtLoad</key><true/>\n\
             \x20 <key>KeepAlive</key><true/>\n\
             \x20 <key>StandardOutPath</key><string>{log}</string>\n\
             \x20 <key>StandardErrorPath</key><string>{log}</string>\n\
             </dict>\n\
             </plist>\n",
            source = xml(&self.source),
            label = xml(&label),
        )
    }
}

/// One argument on a systemd `ExecStart=` line. `%` opens a specifier and `$` a
/// variable, so both are doubled; anything with a space, a quote or a backslash
/// is double-quoted with those escaped.
fn systemd_arg(a: &str) -> String {
    let a = a.replace('%', "%%").replace('$', "$$");
    if !a.is_empty() && !a.contains(|c: char| c.is_whitespace() || matches!(c, '"' | '\'' | '\\' | ';')) {
        return a;
    }
    format!("\"{}\"", a.replace('\\', "\\\\").replace('"', "\\\""))
}

fn xml(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

/// Where this unit's file lives on this OS.
pub fn path(unit: &Unit, scope: Scope) -> Result<PathBuf> {
    let home = || dirs::home_dir().context("no home directory");
    Ok(match (cfg!(target_os = "macos"), scope) {
        (true, Scope::User) => home()?.join("Library/LaunchAgents").join(format!("{}.plist", unit.launchd_label())),
        (true, Scope::System) => PathBuf::from("/Library/LaunchDaemons").join(format!("{}.plist", unit.launchd_label())),
        (false, Scope::User) => home()?.join(".config/systemd/user").join(unit.systemd_name()),
        (false, Scope::System) => PathBuf::from("/etc/systemd/system").join(unit.systemd_name()),
    })
}

/// Write the unit, enable it, and (re)start it. Returns the file written.
pub fn install(unit: &Unit, scope: Scope) -> Result<PathBuf> {
    let file = path(unit, scope)?;
    let body = if cfg!(target_os = "macos") { unit.launchd() } else { unit.systemd(scope) };
    let dir = file.parent().context("a unit path has a parent")?;
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    std::fs::write(&file, body).with_context(|| match scope {
        Scope::System => format!("writing {} (a system unit is root's: run it with sudo)", file.display()),
        Scope::User => format!("writing {}", file.display()),
    })?;
    if cfg!(target_os = "macos") {
        let domain = match scope {
            Scope::User => format!("gui/{}", uid()?),
            Scope::System => "system".to_string(),
        };
        // A job already loaded must be booted out before a new plist loads; a job
        // that was never loaded refuses, which is the state wanted.
        let _ = Command::new("launchctl").args(["bootout", &domain]).arg(&file).status();
        run(Command::new("launchctl").args(["bootstrap", &domain]).arg(&file))?;
    } else {
        let ctl = |args: &[&str]| {
            let mut c = Command::new("systemctl");
            if scope == Scope::User {
                c.arg("--user");
            }
            c.args(args);
            c
        };
        run(&mut ctl(&["daemon-reload"]))?;
        run(&mut ctl(&["enable", &unit.systemd_name()]))?;
        run(&mut ctl(&["restart", &unit.systemd_name()]))?;
    }
    Ok(file)
}

/// Whether only root can change the file at `path` — what a system unit may run.
#[cfg(unix)]
pub fn root_only(path: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    std::fs::metadata(path).is_ok_and(|m| m.uid() == 0 && m.mode() & 0o022 == 0)
}

#[cfg(not(unix))]
pub fn root_only(_path: &Path) -> bool {
    true
}

/// Whether a user unit will run with nobody logged in. `None` where the answer
/// does not apply (macOS, a system unit) or cannot be read.
pub fn lingers(scope: Scope) -> Option<bool> {
    if cfg!(target_os = "macos") || scope == Scope::System {
        return None;
    }
    let user = std::env::var("USER").ok()?;
    Some(Path::new("/var/lib/systemd/linger").join(user).exists())
}

/// Every `hanzo link` unit installed here, with its scope.
pub fn installed() -> Vec<(String, Scope)> {
    let mut out = Vec::new();
    let dirs: [(Option<PathBuf>, Scope); 2] = if cfg!(target_os = "macos") {
        [
            (dirs::home_dir().map(|h| h.join("Library/LaunchAgents")), Scope::User),
            (Some(PathBuf::from("/Library/LaunchDaemons")), Scope::System),
        ]
    } else {
        [
            (dirs::home_dir().map(|h| h.join(".config/systemd/user")), Scope::User),
            (Some(PathBuf::from("/etc/systemd/system")), Scope::System),
        ]
    };
    for (dir, scope) in dirs {
        let Some(Ok(entries)) = dir.map(std::fs::read_dir) else { continue };
        let mut names: Vec<String> = entries
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| ours(n))
            .collect();
        names.sort();
        out.extend(names.into_iter().map(|n| (n, scope)));
    }
    out
}

/// Whether a unit file is one `--install` writes: `host`, or `dial-<service>`.
/// Other units share the `hanzo-link-` prefix (a machine's own guard, say).
fn ours(file: &str) -> bool {
    let id = file
        .strip_prefix("hanzo-link-")
        .and_then(|r| r.strip_suffix(".service"))
        .or_else(|| file.strip_prefix("com.ai.hanzo.link.").and_then(|r| r.strip_suffix(".plist")));
    matches!(id, Some(id) if id == "host" || id.starts_with("dial-"))
}

/// A unit's state as its service manager reports it: `active`, `inactive`,
/// `failed`… (systemd), or `loaded` / `not loaded` (launchd).
pub fn state(name: &str, scope: Scope) -> String {
    if cfg!(target_os = "macos") {
        let label = name.trim_end_matches(".plist");
        let loaded = Command::new("launchctl").args(["list", label]).output().map(|o| o.status.success());
        return match loaded {
            Ok(true) => "loaded".into(),
            _ => "not loaded".into(),
        };
    }
    let mut c = Command::new("systemctl");
    if scope == Scope::User {
        c.arg("--user");
    }
    match c.args(["is-active", name]).output() {
        Ok(o) => String::from_utf8_lossy(&o.stdout).trim().to_string(),
        Err(e) => format!("unknown ({e})"),
    }
}

fn run(c: &mut Command) -> Result<()> {
    let status = c.status().with_context(|| format!("running {c:?}"))?;
    if !status.success() {
        bail!("{c:?} exited with {status}");
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn uid() -> Result<u32> {
    let out = Command::new("id").arg("-u").output().context("running id -u")?;
    String::from_utf8_lossy(&out.stdout).trim().parse().context("parsing id -u")
}

#[cfg(not(target_os = "macos"))]
fn uid() -> Result<u32> {
    bail!("launchd domains exist only on macOS")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dial() -> Unit {
        Unit {
            id: "dial-k8s.hanzo".into(),
            description: "dial k8s.hanzo on :26443".into(),
            argv: vec![
                "/home/z/.local/bin/hanzo".into(),
                "link".into(),
                "dial".into(),
                "k8s.hanzo".into(),
                "26443".into(),
                "--token-command".into(),
                "/home/z/.local/bin/hanzo auth token".into(),
            ],
            source: "hanzo link dial k8s.hanzo 26443 --install".into(),
        }
    }

    #[test]
    fn one_id_names_the_unit_on_both_service_managers() {
        let u = dial();
        assert_eq!(u.systemd_name(), "hanzo-link-dial-k8s.hanzo.service");
        assert_eq!(u.launchd_label(), "com.ai.hanzo.link.dial-k8s.hanzo");
    }

    #[test]
    fn a_user_unit_restarts_forever_and_starts_with_the_session() {
        let text = dial().systemd(Scope::User);
        assert!(text.starts_with("# Written by `hanzo link dial k8s.hanzo 26443 --install`."), "{text}");
        assert!(text.contains("Description=Hanzo link: dial k8s.hanzo on :26443\n"));
        assert!(text.contains("After=network-online.target\nWants=network-online.target\n"));
        assert!(text.contains("Restart=always\nRestartSec=2\n"));
        assert!(text.ends_with("[Install]\nWantedBy=default.target\n"), "{text}");
    }

    #[test]
    fn a_system_unit_starts_with_the_machine() {
        assert!(dial().systemd(Scope::System).ends_with("WantedBy=multi-user.target\n"));
    }

    /// A token command with spaces is ONE argument to zt, so it is quoted as one
    /// — exactly the hand-written unit this replaces.
    #[test]
    fn exec_start_keeps_the_token_command_one_argument() {
        let text = dial().systemd(Scope::User);
        let exec = text.lines().find(|l| l.starts_with("ExecStart=")).unwrap();
        assert_eq!(
            exec,
            "ExecStart=/home/z/.local/bin/hanzo link dial k8s.hanzo 26443 --token-command \
             \"/home/z/.local/bin/hanzo auth token\""
        );
    }

    #[test]
    fn exec_start_escapes_what_systemd_would_expand() {
        assert_eq!(systemd_arg("/opt/a%b"), "/opt/a%%b");
        assert_eq!(systemd_arg("$HOME/t"), "$$HOME/t");
        assert_eq!(systemd_arg("say \"hi\""), "\"say \\\"hi\\\"\"");
        assert_eq!(systemd_arg("a;b"), "\"a;b\"");
        assert_eq!(systemd_arg(""), "\"\"");
    }

    #[test]
    fn a_launchd_job_keeps_every_argument_whole_and_escaped() {
        let mut u = dial();
        u.argv.push("a&b<c>".into());
        let text = u.launchd();
        assert!(text.contains("<key>Label</key><string>com.ai.hanzo.link.dial-k8s.hanzo</string>"));
        assert!(text.contains("<string>/home/z/.local/bin/hanzo auth token</string>"));
        assert!(text.contains("<string>a&amp;b&lt;c&gt;</string>"));
        assert!(text.contains("<key>KeepAlive</key><true/>"));
        assert!(text.contains("/tmp/com.ai.hanzo.link.dial-k8s.hanzo.log"));
    }

    #[test]
    fn only_the_units_install_writes_are_listed_as_ours() {
        for mine in ["hanzo-link-host.service", "hanzo-link-dial-k8s.hanzo.service", "com.ai.hanzo.link.host.plist"] {
            assert!(ours(mine), "{mine}");
        }
        for theirs in ["hanzo-link-guard.service", "link.service", "hanzo-beat.service", "hanzo-link-host.service.d"] {
            assert!(!ours(theirs), "{theirs}");
        }
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn units_live_where_systemd_reads_them() {
        let u = dial();
        assert_eq!(
            path(&u, Scope::System).unwrap(),
            PathBuf::from("/etc/systemd/system/hanzo-link-dial-k8s.hanzo.service")
        );
        assert!(path(&u, Scope::User)
            .unwrap()
            .ends_with(".config/systemd/user/hanzo-link-dial-k8s.hanzo.service"));
    }
}
