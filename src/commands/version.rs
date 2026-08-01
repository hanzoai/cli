//! `hanzo version` — our own version, and whether the names this binary is
//! installed under agree with it. THE one implementation behind `hanzo version`,
//! `hanzo --version` and `hanzo -V` (see `main`).
//!
//! This CLI ships under two names. `hanzo` is what a person types; `hanzo-node`
//! is what cloud's Go control binary delegates to (`cloud/cli/link.go`
//! `fabricCLI()` resolves `hanzo-node` on PATH first, then `hanzo`). Same build,
//! two names — by design.
//!
//! The failure that design allows is INVISIBLE, and it has TWO directions:
//!
//! * a stale `hanzo-node` BEHIND us — install one name and not the other, or
//!   upgrade one and not the other, and a user types `hanzo`, gets delegated,
//!   and runs a different build with no version anywhere on screen. A v1.7.2
//!   `hanzo-node` sitting behind a current `hanzo` is what served ~150 commands
//!   that no longer exist.
//! * a different file AHEAD of us holding the name `hanzo` — an earlier PATH
//!   entry wins every bare `hanzo`, so the build a user upgraded is not the
//!   build that answers. Same silence, opposite direction.
//!
//! So this reports both. It never repairs either and never refuses to run — it
//! makes a silent thing loud.
//!
//! THE VERSION IS THE ONLY THING ON STDOUT: one line, `hanzo <semver>`, which is
//! the shape the Go control binary's delegate parser reads and the shape every
//! other `--version` in the world prints. A skew is a WARNING, not the answer,
//! so it goes to stderr and a caller piping the version gets exactly one line.

use colored::Colorize;
use std::path::{Path, PathBuf};

/// The name cloud's control binary delegates to.
const DELEGATE: &str = "hanzo-node";

/// The name a person types — the one an earlier PATH entry can steal.
const NAME: &str = "hanzo";

/// What we found at the other name.
enum Twin {
    /// No `hanzo-node` on PATH. Nothing delegates here, nothing to skew.
    Absent,
    /// The same file as us — a link or a copy of this build. The healthy install.
    Same,
    /// A different build. THIS is the invisible failure, so it is reported loudly.
    Skewed { path: PathBuf, version: String },
    /// Present, but it would not say what it is.
    Unreadable(PathBuf),
}

pub fn run() {
    println!("hanzo {}", env!("CARGO_PKG_VERSION"));

    match twin() {
        Twin::Absent | Twin::Same => {}
        Twin::Skewed { path, version } => {
            eprintln!();
            eprintln!(
                "{} {} is v{} — this is v{}",
                "stale delegate:".yellow().bold(),
                path.display(),
                version,
                env!("CARGO_PKG_VERSION"),
            );
            eprintln!(
                "  Verbs handed to `{DELEGATE}` run THAT build, not this one, so the \n  \
                 command surface you see may not be the one that answers. Reinstall so \n  \
                 both names are the same build."
            );
        }
        Twin::Unreadable(path) => {
            eprintln!();
            eprintln!(
                "{} {} did not report a version — it may not be this CLI",
                "stale delegate:".yellow().bold(),
                path.display(),
            );
        }
    }

    if let Some((wins, ours)) = shadow(which(NAME).as_deref().map(real), me()) {
        eprintln!();
        eprintln!(
            "{} `{NAME}` on PATH is {} — this is {}",
            "shadowed name:".yellow().bold(),
            wins.display(),
            ours.display(),
        );
        eprintln!(
            "  The PATH entry WINS: typing `{NAME}` runs that file, not this one. Remove \n  \
             it or reorder PATH so one build owns the name."
        );
    }
}

/// Locate `hanzo-node` on PATH and decide what it is relative to us.
fn twin() -> Twin {
    let Some(path) = which(DELEGATE) else { return Twin::Absent };

    // Same inode (a hardlink) or same resolved target (a symlink, or literally
    // us) is the healthy install: one build, two names. Compare canonically so
    // a symlink chain does not read as a skew.
    if me().is_some_and(|m| real(&path) == m) {
        return Twin::Same;
    }

    match version_of(&path) {
        Some(v) if v == env!("CARGO_PKG_VERSION") => Twin::Same,
        Some(v) => Twin::Skewed { path, version: v },
        None => Twin::Unreadable(path),
    }
}

/// Who OWNS the name, when it is not us: `(the file that wins, us)`.
///
/// `first` is the first `hanzo` on PATH — the file a shell actually execs — and
/// `me` is this executable, both already canonical. `None` when they are the
/// same file (the healthy install) or when the OS will not say which we are, so
/// silence always means "nothing is in front of you". Pure, so the rule is a
/// test rather than a comment.
fn shadow(first: Option<PathBuf>, me: Option<PathBuf>) -> Option<(PathBuf, PathBuf)> {
    let (first, me) = (first?, me?);
    (first != me).then_some((first, me))
}

/// This executable, canonically — the identity every skew is measured against.
fn me() -> Option<PathBuf> {
    std::env::current_exe().ok().map(|p| real(&p))
}

/// Resolve a path through its symlinks. An unresolvable path is ITSELF: a file
/// we cannot canonicalize is still a real candidate, and dropping it would turn
/// a skew into silence — the one outcome this module exists to prevent.
fn real(p: &Path) -> PathBuf {
    std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf())
}

/// First executable named `name` on PATH. We do our own lookup rather than
/// shelling out, so this answers the same way in a bare environment.
fn which(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).map(|dir| dir.join(name)).find(|p| is_executable(p))
}

#[cfg(unix)]
fn is_executable(p: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(p).map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0).unwrap_or(false)
}

#[cfg(not(unix))]
fn is_executable(p: &Path) -> bool {
    p.is_file()
}

/// Ask a binary its version. `--version` prints `hanzo <semver>`; take the last
/// whitespace-separated token so a differently-worded build still parses.
fn version_of(bin: &Path) -> Option<String> {
    let out = std::process::Command::new(bin).arg("--version").output().ok()?;
    if !out.status.success() {
        return None;
    }
    let line = String::from_utf8_lossy(&out.stdout);
    let token = line.lines().next()?.split_whitespace().last()?;
    let v = token.trim_start_matches('v');
    // Only accept something that looks like a version, so a usage dump or an
    // error banner is reported as unreadable rather than as a bogus skew.
    (!v.is_empty() && v.starts_with(|c: char| c.is_ascii_digit())).then(|| v.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    /// Writing an executable in one thread while another FORKS is a race the
    /// kernel names: the forked child inherits the still-open write descriptor,
    /// and the exec that follows fails ETXTBSY ("Text file busy"). These are the
    /// only tests here that both write an executable and run it, so holding one
    /// lock across write-then-exec removes the window rather than tolerating it.
    /// Measured before: 2 of 5 full runs failed, alternating between
    /// `a_version_is_read_from_the_binary` and `a_v_prefix_is_tolerated`. A gate
    /// that fails at random is a gate people learn to re-run instead of read.
    static EXEC: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// A stub binary that prints `body` on `--version`. Call it while holding
    /// [`EXEC`]; [`probe`] is the one-shot form that does both.
    fn stub(dir: &Path, name: &str, body: &str) -> PathBuf {
        let p = dir.join(name);
        let mut f = std::fs::File::create(&p).unwrap();
        write!(f, "#!/bin/sh\necho '{body}'\n").unwrap();
        drop(f);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        p
    }

    /// Write a stub that prints `body`, then ask it its version — the write and
    /// the exec under one lock, so no sibling test can fork between them.
    fn probe(body: &str) -> Option<String> {
        let g = EXEC.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let d = tempfile::tempdir().unwrap();
        let v = version_of(&stub(d.path(), "twin", body));
        drop(g);
        v
    }

    #[test]
    fn a_version_is_read_from_the_binary() {
        assert_eq!(probe("hanzo 1.2.3").as_deref(), Some("1.2.3"));
    }

    /// A `v` prefix is the same version, not a different one.
    #[test]
    fn a_v_prefix_is_tolerated() {
        assert_eq!(probe("Hanzo CLI v9.9.9").as_deref(), Some("9.9.9"));
    }

    /// Something that is not this CLI must read as unreadable, never as a
    /// version — reporting a confident wrong number is worse than saying
    /// nothing, because the whole point here is to be trusted about skew.
    #[test]
    fn a_non_version_is_not_mistaken_for_one() {
        assert_eq!(probe("usage: thing [opts]"), None);
        assert_eq!(probe(""), None);
    }

    /// The OTHER skew direction: a DIFFERENT file holding the name `hanzo` ahead
    /// of us on PATH. The same file under two spellings is the healthy install
    /// and must stay silent; only a genuinely different file is reported, and
    /// never on a system that will not say which executable we are.
    #[test]
    fn a_different_file_holding_our_name_is_reported_and_our_own_is_not() {
        let (us, them) = (PathBuf::from("/home/z/.local/bin/hanzo"), PathBuf::from("/usr/local/bin/hanzo"));

        assert_eq!(
            shadow(Some(them.clone()), Some(us.clone())),
            Some((them, us.clone())),
            "the PATH winner and us are named, in that order"
        );
        assert_eq!(shadow(Some(us.clone()), Some(us.clone())), None, "one file, no skew");
        assert_eq!(shadow(None, Some(us.clone())), None, "nothing on PATH is not a skew");
        assert_eq!(shadow(Some(us), None), None, "unknown self is never a skew");
    }

    /// PATH lookup finds the FIRST match, which is the one that would actually
    /// be exec'd — the precedence that let a stale build hide.
    #[test]
    fn which_finds_the_first_on_path() {
        let _g = EXEC.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let d = tempfile::tempdir().unwrap();
        let (early, late) = (d.path().join("early"), d.path().join("late"));
        std::fs::create_dir_all(&early).unwrap();
        std::fs::create_dir_all(&late).unwrap();
        stub(&early, DELEGATE, "hanzo 1.0.0");
        stub(&late, DELEGATE, "hanzo 2.0.0");

        let joined = std::env::join_paths([&early, &late]).unwrap();
        // SAFETY: single-threaded test; restored before returning.
        let prev = std::env::var_os("PATH");
        std::env::set_var("PATH", &joined);
        let found = which(DELEGATE).expect("finds the delegate");
        if let Some(prev) = prev {
            std::env::set_var("PATH", prev);
        }

        assert_eq!(version_of(&found).as_deref(), Some("1.0.0"), "the earlier PATH entry wins");
    }
}
