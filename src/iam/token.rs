//! Secure persistence of IAM token sets — the PORTABLE credential store.
//!
//! A credential must be reachable EVERYWHERE `hanzo` runs: a desktop, but also a
//! container, a headless server, an SSH session and CI. So the store is a seam
//! ([`Vault`]) with two implementations, chosen at runtime by [`vault`]:
//!
//! - [`Keyring`] — the native OS keychain (macOS Keychain, Windows Credential
//!   Manager), used when one is present and answering. Compiled ONLY on those
//!   targets, so nothing else links a keychain C library.
//! - [`FileVault`] — an owner-only (`0600`) file, used everywhere else and as the
//!   fallback when a keychain is unreachable. It has NO native dependency, so it
//!   works in a container and cross-compiles cleanly to every target. On Linux
//!   the OS keychain is secret-service over D-Bus, which does not exist in a
//!   container / over SSH / in CI and whose C `libdbus` binding does not
//!   cross-compile — so Linux uses the file, at the SAME `0600` protection
//!   secret-service would have given.
//!
//! One entry per IDENTITY: the key is `{brand}/{owner}/{name}`, so holding both
//! `admin/z` and `hanzo/z` is the normal case and nothing clobbers. WHICH of
//! them is active is non-secret index data and lives in `config.toml` (`[auth]`)
//! — the store has no portable enumeration API, so the index is what makes
//! listing work offline. Same law as `commands::wallet`, which files its wallet
//! keys through the SAME [`vault`]: secret in the store, metadata in the config.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use super::identity::Identity;

/// Store namespace under which every Hanzo CLI credential is filed — the keychain
/// service name, and (as the file's own identity) the reason wallet keys and IAM
/// tokens share one store without colliding: their key strings are disjoint
/// (`wallet:0x…` vs `{brand}/{owner}/{name}`).
#[cfg_attr(not(any(target_os = "macos", target_os = "windows")), allow(dead_code))]
const SERVICE: &str = "ai.hanzo.cli";

/// An OAuth2/OIDC token response (RFC 6749 §5.1). Stored verbatim as the
/// keychain secret so the refresh and id tokens survive for the session.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenSet {
    pub access_token: String,
    #[serde(default = "default_token_type")]
    pub token_type: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refresh_token: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id_token: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_in: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
}

fn default_token_type() -> String {
    "Bearer".to_string()
}

/// The credential store, as a seam. Get/set/remove a secret by key — nothing
/// more, so the same seam serves IAM tokens ([`store`]/[`load`]/[`delete`]) and
/// wallet keys (`commands::wallet`). Two production implementations ([`Keyring`],
/// [`FileVault`]) are chosen by [`vault`]; the multi-identity LOGIC is written
/// against the trait and unit-tested against an in-memory vault, since a real
/// keychain prompts / needs a session keyring.
pub trait Vault {
    fn get(&self, key: &str) -> Result<Option<String>>;
    fn set(&self, key: &str, value: &str) -> Result<()>;
    /// Returns whether an entry existed.
    fn remove(&self, key: &str) -> Result<bool>;
}

/// An owner-only file holding the credentials, `key -> value`. The portable
/// backend: no native dependency, so it works in a container / headless / CI and
/// cross-compiles to every target.
///
/// Protection is filesystem permissions — mode `0600`, written atomically through
/// [`crate::private::write`], the SAME primitive (and the same guarantee) behind
/// `config.toml`, `machine-id` and the run-target records, and the same
/// protection `~/.ssh/id_*` relies on. Encrypting the bytes would need a key, and
/// on the platforms that USE this backend there is no OS keychain to hold it — so
/// the key would sit beside the ciphertext, which is obfuscation, not security.
/// `0600` is the honest floor.
///
/// Concurrency is the config's law: several `hanzo` processes may write at once,
/// so a `set`/`remove` takes the cross-process [`crate::config::Lock`] on a
/// sidecar `credentials.lock`, RE-READS current truth, applies the one-key change
/// and writes atomically — a concurrent writer of a DIFFERENT key is never lost.
pub struct FileVault {
    path: std::path::PathBuf,
}

impl FileVault {
    /// The store's location: `${XDG_DATA_HOME}/hanzo/credentials`
    /// (`~/.local/share/hanzo/credentials` on Linux), beside `machine-id`.
    pub fn resolve() -> Result<Self> {
        let dir = dirs::data_local_dir()
            .ok_or_else(|| anyhow::anyhow!("no data directory for the credential store"))?
            .join("hanzo");
        Ok(Self { path: dir.join("credentials") })
    }

    /// Point a `FileVault` at an explicit file — the seam a test writes through.
    #[cfg(test)]
    pub fn at(path: std::path::PathBuf) -> Self {
        Self { path }
    }

    fn read_map(&self) -> Result<std::collections::BTreeMap<String, String>> {
        match std::fs::read(&self.path) {
            Ok(bytes) => {
                serde_json::from_slice(&bytes).context("parsing the credential store (corrupt?)")
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                Ok(std::collections::BTreeMap::new())
            }
            Err(e) => Err(e).context("reading the credential store"),
        }
    }

    fn write_map(&self, map: &std::collections::BTreeMap<String, String>) -> Result<()> {
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir).context("creating the credential store directory")?;
        }
        let bytes = serde_json::to_vec(map).context("serializing the credential store")?;
        crate::private::write(&self.path, &bytes).context("writing the credential store")
    }
}

impl Vault for FileVault {
    fn get(&self, key: &str) -> Result<Option<String>> {
        Ok(self.read_map()?.get(key).cloned())
    }

    fn set(&self, key: &str, value: &str) -> Result<()> {
        let _lock = crate::config::Lock::acquire(&self.path)?;
        let mut map = self.read_map()?;
        map.insert(key.to_string(), value.to_string());
        self.write_map(&map)
    }

    fn remove(&self, key: &str) -> Result<bool> {
        let _lock = crate::config::Lock::acquire(&self.path)?;
        let mut map = self.read_map()?;
        let existed = map.remove(key).is_some();
        if existed {
            self.write_map(&map)?;
        }
        Ok(existed)
    }
}

/// The native OS keychain. Compiled only where one exists as a first-class,
/// dependency-free-to-cross-compile backend: macOS Keychain and Windows
/// Credential Manager. Linux secret-service is deliberately absent — see the
/// module doc and [`vault`].
#[cfg(any(target_os = "macos", target_os = "windows"))]
pub struct Keyring;

#[cfg(any(target_os = "macos", target_os = "windows"))]
impl Vault for Keyring {
    fn get(&self, key: &str) -> Result<Option<String>> {
        match entry(key)?.get_password() {
            Ok(json) => Ok(Some(json)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(e).context("reading credential from OS keychain"),
        }
    }

    fn set(&self, key: &str, value: &str) -> Result<()> {
        entry(key)?
            .set_password(value)
            .context("writing credential to OS keychain")
    }

    fn remove(&self, key: &str) -> Result<bool> {
        match entry(key)?.delete_credential() {
            Ok(()) => Ok(true),
            Err(keyring::Error::NoEntry) => Ok(false),
            Err(e) => Err(e).context("deleting credential from OS keychain"),
        }
    }
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
fn entry(key: &str) -> Result<keyring::Entry> {
    keyring::Entry::new(SERVICE, key).context("opening OS keychain entry")
}

/// Resolve the credential store for THIS run — THE one place the backend is
/// chosen, so identity tokens and wallet keys reach secrets identically.
///
/// macOS / Windows use the native keychain when it ANSWERS a probe, else the
/// file — a headless or service account with no unlocked keychain still works.
/// Every other target uses the file unconditionally.
pub fn vault() -> Result<Box<dyn Vault>> {
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    {
        if keychain_reachable() {
            return Ok(Box::new(Keyring));
        }
    }
    Ok(Box::new(FileVault::resolve()?))
}

/// Whether the native keychain is present and answering. A read of a name that
/// cannot exist must come back `NoEntry`; any OTHER error means the backend
/// itself is unreachable (locked / headless), so we fall back to the file rather
/// than fail every command.
#[cfg(any(target_os = "macos", target_os = "windows"))]
fn keychain_reachable() -> bool {
    match entry("__hanzo_probe__") {
        Ok(e) => matches!(e.get_password(), Ok(_) | Err(keyring::Error::NoEntry)),
        Err(_) => false,
    }
}

/// The keychain key for one identity of one brand. `Identity` forbids `/` in
/// both components, so this composition is unambiguous and non-spoofable.
pub fn key(brand: &str, id: &Identity) -> String {
    format!("{brand}/{}/{}", id.owner, id.name)
}

/// Persist `tokens` for `id` under `brand`.
pub fn store(v: &dyn Vault, brand: &str, id: &Identity, tokens: &TokenSet) -> Result<()> {
    v.set(&key(brand, id), &serde_json::to_string(tokens)?)
}

/// Load the stored token set for one identity, if any.
pub fn load(v: &dyn Vault, brand: &str, id: &Identity) -> Result<Option<TokenSet>> {
    match v.get(&key(brand, id))? {
        Some(json) => Ok(Some(serde_json::from_str(&json)?)),
        None => Ok(None),
    }
}

/// Remove one identity's credential. Returns whether one existed.
pub fn delete(v: &dyn Vault, brand: &str, id: &Identity) -> Result<bool> {
    v.remove(&key(brand, id))
}

/// The pre-multi-identity keychain key: the bare brand, one token per brand.
/// Read exactly once, by the forwards-only migration in `store`, which re-files
/// it and deletes it. Nothing else may read this — there is no dual-read path.
pub(super) fn legacy_key(brand: &str) -> &str {
    brand
}

#[cfg(test)]
pub(crate) mod memvault {
    use super::*;
    use std::collections::BTreeMap;
    use std::sync::Mutex;

    /// An in-memory [`Vault`] standing in for the OS keychain in tests.
    #[derive(Default)]
    pub struct MemVault {
        entries: Mutex<BTreeMap<String, String>>,
    }

    impl MemVault {
        pub fn new() -> Self {
            Self::default()
        }

        /// Every key currently held — lets a test assert exactly which slots exist.
        pub fn keys(&self) -> Vec<String> {
            self.entries.lock().unwrap().keys().cloned().collect()
        }

        pub fn has(&self, key: &str) -> bool {
            self.entries.lock().unwrap().contains_key(key)
        }
    }

    impl Vault for MemVault {
        fn get(&self, key: &str) -> Result<Option<String>> {
            Ok(self.entries.lock().unwrap().get(key).cloned())
        }

        fn set(&self, key: &str, value: &str) -> Result<()> {
            self.entries
                .lock()
                .unwrap()
                .insert(key.to_string(), value.to_string());
            Ok(())
        }

        fn remove(&self, key: &str) -> Result<bool> {
            Ok(self.entries.lock().unwrap().remove(key).is_some())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::memvault::MemVault;
    use super::*;
    use crate::iam::identity::testjwt::jwt;

    fn tokens(access: &str) -> TokenSet {
        TokenSet {
            access_token: access.to_string(),
            token_type: "Bearer".to_string(),
            refresh_token: None,
            id_token: None,
            expires_in: None,
            scope: None,
        }
    }

    // Keychain I/O itself is not exercised in unit tests (it would prompt /
    // require a session keyring); the value type's serde contract and the key
    // composition are what we must pin.
    #[test]
    fn token_set_roundtrips_and_defaults_token_type() {
        let json = r#"{"access_token":"AT","refresh_token":"RT","expires_in":3600,"scope":"openid profile email"}"#;
        let t: TokenSet = serde_json::from_str(json).unwrap();
        assert_eq!(t.access_token, "AT");
        assert_eq!(t.token_type, "Bearer"); // defaulted when absent
        assert_eq!(t.refresh_token.as_deref(), Some("RT"));
        assert_eq!(t.expires_in, Some(3600));

        let back = serde_json::to_string(&t).unwrap();
        assert!(back.contains(r#""access_token":"AT""#));
        // None fields are omitted from the stored blob.
        assert!(!back.contains("id_token"));
    }

    #[test]
    fn the_key_is_brand_scoped_per_identity() {
        let admin = Identity::from_access_token(&jwt("admin", "z")).unwrap();
        let org = Identity::from_access_token(&jwt("hanzo", "z")).unwrap();
        assert_eq!(key("hanzo", &admin), "hanzo/admin/z");
        assert_eq!(key("hanzo", &org), "hanzo/hanzo/z");
        // The same identity on another brand is a different slot.
        assert_ne!(key("lux", &admin), key("hanzo", &admin));
    }

    /// The incident in one test: two identities of the SAME human coexist.
    #[test]
    fn two_identities_coexist_and_neither_clobbers_the_other() {
        let v = MemVault::new();
        let admin = Identity::from_access_token(&jwt("admin", "z")).unwrap();
        let org = Identity::from_access_token(&jwt("hanzo", "z")).unwrap();

        store(&v, "hanzo", &admin, &tokens("ADMIN_AT")).unwrap();
        store(&v, "hanzo", &org, &tokens("ORG_AT")).unwrap();

        assert_eq!(
            load(&v, "hanzo", &admin).unwrap().unwrap().access_token,
            "ADMIN_AT"
        );
        assert_eq!(
            load(&v, "hanzo", &org).unwrap().unwrap().access_token,
            "ORG_AT"
        );
        assert_eq!(v.keys(), vec!["hanzo/admin/z", "hanzo/hanzo/z"]);
    }

    #[test]
    fn deleting_one_identity_leaves_the_other_intact() {
        let v = MemVault::new();
        let admin = Identity::from_access_token(&jwt("admin", "z")).unwrap();
        let org = Identity::from_access_token(&jwt("hanzo", "z")).unwrap();
        store(&v, "hanzo", &admin, &tokens("ADMIN_AT")).unwrap();
        store(&v, "hanzo", &org, &tokens("ORG_AT")).unwrap();

        assert!(delete(&v, "hanzo", &admin).unwrap());
        assert!(load(&v, "hanzo", &admin).unwrap().is_none());
        assert_eq!(
            load(&v, "hanzo", &org).unwrap().unwrap().access_token,
            "ORG_AT"
        );
        // Deleting what is already gone is not an error.
        assert!(!delete(&v, "hanzo", &admin).unwrap());
    }

    // ---- FileVault: the portable backend that makes "runs everywhere" true ----

    fn scratch_vault(tag: &str) -> (FileVault, std::path::PathBuf) {
        let p = std::env::temp_dir().join(format!(
            "hanzo-cred-{tag}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_file(&p);
        let _ = std::fs::remove_file(crate::config::lock_path(&p));
        (FileVault::at(p.clone()), p)
    }

    #[test]
    fn file_vault_roundtrips_get_set_remove() {
        let (v, p) = scratch_vault("roundtrip");
        assert!(v.get("k").unwrap().is_none(), "absent key reads None");
        v.set("k", "secret-value").unwrap();
        assert_eq!(v.get("k").unwrap().as_deref(), Some("secret-value"));
        assert!(v.remove("k").unwrap(), "remove reports it existed");
        assert!(v.get("k").unwrap().is_none());
        assert!(!v.remove("k").unwrap(), "removing what is gone is not an error");
        let _ = std::fs::remove_file(&p);
    }

    /// The clobber test: a second key must not erase the first. This is the whole
    /// reason `set` re-reads under the lock instead of writing a one-key file.
    #[test]
    fn file_vault_two_keys_coexist() {
        let (v, p) = scratch_vault("coexist");
        v.set("hanzo/admin/z", "ADMIN").unwrap();
        v.set("hanzo/hanzo/z", "ORG").unwrap();
        v.set("wallet:0xabc", "MNEMONIC").unwrap();
        assert_eq!(v.get("hanzo/admin/z").unwrap().as_deref(), Some("ADMIN"));
        assert_eq!(v.get("hanzo/hanzo/z").unwrap().as_deref(), Some("ORG"));
        assert_eq!(v.get("wallet:0xabc").unwrap().as_deref(), Some("MNEMONIC"));
        let _ = std::fs::remove_file(&p);
    }

    /// A secret file is owner-only or it is not a secret file. `private::write`
    /// pins `0600`; this proves the credential store inherits it.
    #[cfg(unix)]
    #[test]
    fn file_vault_is_owner_only() {
        let (v, p) = scratch_vault("mode");
        v.set("k", "s").unwrap();
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "credential store must be 0600, got {mode:o}");
        let _ = std::fs::remove_file(&p);
    }

    /// The identity multi-slot invariant, proven on the FILE backend (not just
    /// `MemVault`): two identities of one human coexist and delete independently.
    /// This is the "headless fallback still works" property at unit level.
    #[test]
    fn file_vault_backs_the_full_identity_flow() {
        let (v, p) = scratch_vault("identity");
        let admin = Identity::from_access_token(&jwt("admin", "z")).unwrap();
        let org = Identity::from_access_token(&jwt("hanzo", "z")).unwrap();
        store(&v, "hanzo", &admin, &tokens("ADMIN_AT")).unwrap();
        store(&v, "hanzo", &org, &tokens("ORG_AT")).unwrap();
        assert_eq!(load(&v, "hanzo", &admin).unwrap().unwrap().access_token, "ADMIN_AT");
        assert_eq!(load(&v, "hanzo", &org).unwrap().unwrap().access_token, "ORG_AT");
        assert!(delete(&v, "hanzo", &admin).unwrap());
        assert!(load(&v, "hanzo", &admin).unwrap().is_none());
        assert_eq!(load(&v, "hanzo", &org).unwrap().unwrap().access_token, "ORG_AT");
        let _ = std::fs::remove_file(&p);
    }

    /// Many `hanzo` processes, ONE credential file, each writing a DISTINCT key:
    /// every key must survive. Without the lock + re-read, a `set` would publish a
    /// map missing the keys written between its read and its write. `flock` is
    /// per-open-file-description, so independent threads faithfully stand in for
    /// separate invocations.
    #[test]
    fn file_vault_concurrent_writers_keep_each_others_keys() {
        let (_seed, p) = scratch_vault("race");
        const WRITERS: usize = 8;
        std::thread::scope(|s| {
            for i in 0..WRITERS {
                let p = p.clone();
                s.spawn(move || {
                    FileVault::at(p).set(&format!("hanzo/org{i}/z"), &format!("AT{i}")).unwrap();
                });
            }
        });
        let v = FileVault::at(p.clone());
        for i in 0..WRITERS {
            assert_eq!(
                v.get(&format!("hanzo/org{i}/z")).unwrap().as_deref(),
                Some(format!("AT{i}").as_str()),
                "lost a concurrent writer's key"
            );
        }
        let _ = std::fs::remove_file(&p);
    }

    /// The store lives beside `machine-id`, at `…/hanzo/credentials`.
    #[test]
    fn file_vault_default_location_is_the_data_dir() {
        let v = FileVault::resolve().unwrap();
        assert!(
            v.path.ends_with("hanzo/credentials"),
            "unexpected store path: {}",
            v.path.display()
        );
    }
}
