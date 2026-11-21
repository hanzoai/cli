//! The non-secret index: which identity is active. It lives in the CLI's
//! `config.toml` under `[auth] active`, as `brand -> "owner/name"`. Read-only
//! here: only `hanzo auth login` and `hanzo auth use` change it.

use crate::identity::{Identity, Selector};
use std::path::{Path, PathBuf};

/// Where the CLI keeps `config.toml`.
pub fn config_path() -> PathBuf {
    dirs::config_dir().unwrap_or_else(|| PathBuf::from(".")).join("hanzo").join("config.toml")
}

/// The active identity of `brand` named by the config at `path`, if any.
pub fn active_at(path: &Path, brand: &str) -> Option<Identity> {
    let text = std::fs::read_to_string(path).ok()?;
    let doc: toml::Table = text.parse().ok()?;
    let held = doc.get("auth")?.get("active")?.get(brand)?.as_str()?;
    match held.parse::<Selector>().ok()? {
        Selector::Exact(id) => Some(id),
        _ => None,
    }
}

/// The active identity of `brand` on this machine.
pub fn active(brand: &str) -> Option<Identity> {
    active_at(&config_path(), brand)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_active_identity_of_a_brand() {
        let dir = std::env::temp_dir().join(format!("hanzo-index-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("config.toml");
        std::fs::write(&p, "[auth.active]\nhanzo = \"hanzo/z\"\n").unwrap();
        let id = active_at(&p, "hanzo").unwrap();
        assert_eq!((id.owner.as_str(), id.name.as_str()), ("hanzo", "z"));
        assert!(active_at(&p, "lux").is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
