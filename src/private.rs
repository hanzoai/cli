//! Writing a file that only its owner can read — ONE way, everywhere.
//!
//! Two properties, always together, because every caller wants both and neither
//! is safe to forget:
//!
//! - **Owner-only.** The mode is set on the temp file BEFORE it is published, so
//!   the bytes are never momentarily world-readable. `fs::write` + a later
//!   `set_permissions` leaves exactly that window.
//! - **Atomic.** A reader sees the old file or the new one, never a half-written
//!   one. `fs::write` truncates in place: a crash mid-write leaves a torn file,
//!   which for a parsed file (the config) breaks every command, not just the one
//!   that crashed.
//!
//! The temp lives in the SAME directory, so `rename` stays within one filesystem
//! and is therefore atomic, and it has a FIXED name: the next write truncates a
//! temp left by a crashed run, so orphans clean themselves up rather than
//! accumulating. Concurrent writers to one path are serialised by the caller
//! that needs it (see `Config::update`'s lock).

use std::io;
use std::path::{Path, PathBuf};

/// The temp `write` publishes from: `…/config.toml` → `…/config.toml.tmp`.
fn tmp_path(path: &Path) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(".tmp");
    path.with_file_name(name)
}

/// Write `bytes` to `path`, atomically, readable only by the owner.
pub fn write(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let tmp = tmp_path(path);
    match publish(&tmp, path, bytes) {
        Ok(()) => Ok(()),
        Err(e) => {
            // Never leave a partial temp lying next to a good file.
            let _ = std::fs::remove_file(&tmp);
            Err(e)
        }
    }
}

fn publish(tmp: &Path, path: &Path, bytes: &[u8]) -> io::Result<()> {
    use std::io::Write;

    let mut opts = std::fs::OpenOptions::new();
    opts.create(true).write(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut f = opts.open(tmp)?;
    // `mode` above only applies when the open CREATES the file, so a temp left
    // by a crashed run would keep its old permissions. Set them explicitly too.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        f.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    }
    f.write_all(bytes)?;
    // Durable BEFORE the swap: otherwise a crash can leave the rename landed
    // with unflushed content, i.e. an atomically-published empty file.
    f.sync_all()?;
    drop(f); // Windows will not rename an open file.

    // The rename carries the temp's inode — and therefore its 0600 — onto the
    // target. This is why the mode is set on the temp rather than afterwards:
    // a `chmod` after the rename would be a second window, and a `set_permissions`
    // on the TARGET would be undone by the next write.
    std::fs::rename(tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(tag: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!(
            "hanzo-private-{tag}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_file(&p);
        let _ = std::fs::remove_file(tmp_path(&p));
        p
    }

    #[test]
    fn writes_are_owner_only() {
        let p = scratch("mode");
        write(&p, b"hello").unwrap();
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "hello");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&p).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "must be owner-only, got {mode:o}");
        }
        let _ = std::fs::remove_file(&p);
    }

    /// The reversion red measured: hardened perms must SURVIVE a rewrite. The
    /// old `fs::write` preserved an existing file's mode; a naive tmp+rename
    /// replaces the inode and silently hands it back to `0666 & ~umask`.
    #[test]
    fn a_rewrite_does_not_revert_hardened_permissions() {
        let p = scratch("revert");
        write(&p, b"one").unwrap();
        write(&p, b"two").unwrap();
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "two");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&p).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "a rewrite must not widen the mode, got {mode:o}");
        }
        let _ = std::fs::remove_file(&p);
    }

    /// A temp left by a crashed run is truncated and reused, then renamed away —
    /// it never accumulates, and it never survives with stale permissions.
    #[test]
    fn a_stale_temp_is_reused_and_never_orphaned() {
        let p = scratch("orphan");
        let tmp = tmp_path(&p);
        std::fs::write(&tmp, b"leftover garbage from a crash").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o666)).unwrap();
        }

        write(&p, b"fresh").unwrap();

        assert!(!tmp.exists(), "the temp must be renamed away, never orphaned");
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "fresh");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&p).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "a reused temp must not carry stale perms, got {mode:o}");
        }
        let _ = std::fs::remove_file(&p);
    }

    /// The temp is a sibling: `rename` must stay on one filesystem to be atomic.
    #[test]
    fn the_temp_is_a_sibling_of_the_target() {
        let p = PathBuf::from("/a/b/config.toml");
        assert_eq!(tmp_path(&p), PathBuf::from("/a/b/config.toml.tmp"));
        assert_eq!(tmp_path(&p).parent(), p.parent());
    }
}
