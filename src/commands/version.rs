//! `hanzo version` — our own version, and whether the OTHER name this binary is
//! installed under agrees with it.
//!
//! This CLI ships under two names. `hanzo` is what a person types; `hanzo-node`
//! is what cloud's Go control binary delegates to (`cloud/cli/link.go`
//! `fabricCLI()` resolves `hanzo-node` on PATH first, then `hanzo`). Same build,
//! two names — by design.
//!
//! The failure that design allows is INVISIBLE: install one name and not the
//! other, or upgrade one and not the other, and a user types `hanzo`, gets
//! delegated, and runs a different build with no version anywhere on screen. A
//! v1.7.2 `hanzo-node` sitting behind a current `hanzo` is what served ~150
//! commands that no longer exist. Nothing announced it.
//!
//! So `hanzo version` looks for the twin and reports the skew. It never repairs
//! it and never refuses to run — it makes a silent thing loud.

use colored::Colorize;
use std::path::{Path, PathBuf};

/// The name cloud's control binary delegates to.
const DELEGATE: &str = "hanzo-node";

/// What we found at the other name.
enum Twin {
    /// No `hanzo-node` on PATH. Nothing delegates here, nothing to skew.
    Absent,
    /// The same file as us — a link or a copy of this build. The healthy install.
    Same(PathBuf),
    /// A different build. THIS is the invisible failure, so it is reported loudly.
    Skewed { path: PathBuf, version: String },
    /// Present, but it would not say what it is.
    Unreadable(PathBuf),
}

pub fn run() {
    println!("{} v{}", "Hanzo CLI".bold(), env!("CARGO_PKG_VERSION"));

    match twin() {
        Twin::Absent | Twin::Same(_) => {}
        Twin::Skewed { path, version } => {
            println!();
            println!(
                "{} {} is v{} — this is v{}",
                "stale delegate:".yellow().bold(),
                path.display(),
                version,
                env!("CARGO_PKG_VERSION"),
            );
            println!(
                "  Verbs handed to `{DELEGATE}` run THAT build, not this one, so the \n  \
                 command surface you see may not be the one that answers. Reinstall so \n  \
                 both names are the same build."
            );
        }
        Twin::Unreadable(path) => {
            println!();
            println!(
                "{} {} did not report a version — it may not be this CLI",
                "stale delegate:".yellow().bold(),
                path.display(),
            );
        }
    }
}

/// Locate `hanzo-node` on PATH and decide what it is relative to us.
fn twin() -> Twin {
    let Some(path) = which(DELEGATE) else { return Twin::Absent };

    // Same inode (a hardlink) or same resolved target (a symlink, or literally
    // us) is the healthy install: one build, two names. Compare canonically so
    // a symlink chain does not read as a skew.
    if let (Ok(a), Ok(b)) = (std::fs::canonicalize(&path), std::env::current_exe()) {
        if let Ok(b) = std::fs::canonicalize(b) {
            if a == b {
                return Twin::Same(path);
            }
        }
    }

    match version_of(&path) {
        Some(v) if v == env!("CARGO_PKG_VERSION") => Twin::Same(path),
        Some(v) => Twin::Skewed { path, version: v },
        None => Twin::Unreadable(path),
    }
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

    /// A stub binary that prints `body` on `--version`.
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

    #[test]
    fn a_version_is_read_from_the_binary() {
        let d = tempfile::tempdir().unwrap();
        let p = stub(d.path(), "twin", "hanzo 1.2.3");
        assert_eq!(version_of(&p).as_deref(), Some("1.2.3"));
    }

    /// A `v` prefix is the same version, not a different one.
    #[test]
    fn a_v_prefix_is_tolerated() {
        let d = tempfile::tempdir().unwrap();
        let p = stub(d.path(), "twin", "Hanzo CLI v9.9.9");
        assert_eq!(version_of(&p).as_deref(), Some("9.9.9"));
    }

    /// Something that is not this CLI must read as unreadable, never as a
    /// version — reporting a confident wrong number is worse than saying
    /// nothing, because the whole point here is to be trusted about skew.
    #[test]
    fn a_non_version_is_not_mistaken_for_one() {
        let d = tempfile::tempdir().unwrap();
        assert_eq!(version_of(&stub(d.path(), "a", "usage: thing [opts]")), None);
        assert_eq!(version_of(&stub(d.path(), "b", "")), None);
    }

    /// PATH lookup finds the FIRST match, which is the one that would actually
    /// be exec'd — the precedence that let a stale build hide.
    #[test]
    fn which_finds_the_first_on_path() {
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
