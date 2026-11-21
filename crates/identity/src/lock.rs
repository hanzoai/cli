//! The one cross-process exclusive lock the identity store and the config share.
//!
//! The lock lives on a SEPARATE `<name>.lock` file beside what it guards, never
//! on the guarded file itself: an atomic write replaces that file's inode, so a
//! lock held on the old inode would guard nothing once the rename lands.

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

/// The lock file beside `path`: `…/credentials.lock`, `…/config.toml.lock`.
pub fn lock_path(path: &Path) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(".lock");
    path.with_file_name(name)
}

/// An advisory OS lock (`flock` on unix) held until dropped. BLOCKING: keep the
/// critical section to a parse, a write and a rename; never hold it across a
/// network call or a human.
pub struct Lock(std::fs::File);

impl Lock {
    pub fn acquire(path: &Path) -> Result<Self> {
        let lock = lock_path(path);
        // On a brand-new machine the directory does not exist yet; the first
        // sign-in is what creates it.
        if let Some(dir) = lock.parent() {
            std::fs::create_dir_all(dir)
                .with_context(|| format!("creating directory {}", dir.display()))?;
        }
        let f = std::fs::OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(&lock)
            .with_context(|| format!("opening lock {}", lock.display()))?;
        f.lock().with_context(|| format!("locking {}", lock.display()))?;
        Ok(Self(f))
    }
}

impl Drop for Lock {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}
