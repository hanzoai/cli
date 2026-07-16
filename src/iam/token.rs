//! Secure persistence of IAM token sets in the OS keychain.
//!
//! Tokens are credentials, so they live ONLY in the platform keychain (macOS
//! Keychain, Windows Credential Manager, Linux Secret Service) via the
//! `keyring` crate — never in a plaintext file on disk.
//!
//! One entry per IDENTITY: the key is `{brand}/{owner}/{name}`, so holding both
//! `admin/z` and `hanzo/z` is the normal case and nothing clobbers. WHICH of
//! them is active is non-secret index data and lives in `config.toml` (`[auth]`)
//! — the keychain has no portable enumeration API, so the index is what makes
//! listing work offline. Same law as `commands::wallet`: secret in the
//! keychain, metadata in the config.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use super::identity::Identity;

/// Keychain service name under which every Hanzo CLI credential is filed.
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

/// The credential store, as a seam. The OS keychain cannot be exercised in unit
/// tests (it prompts / needs a session keyring), so the multi-identity LOGIC is
/// written against this trait and tested against an in-memory vault, while
/// production runs against [`Keyring`]. There is exactly ONE implementation in
/// the shipped binary — this is a test seam, not a strategy.
pub trait Vault {
    fn get(&self, key: &str) -> Result<Option<String>>;
    fn set(&self, key: &str, value: &str) -> Result<()>;
    /// Returns whether an entry existed.
    fn remove(&self, key: &str) -> Result<bool>;
}

/// The real OS keychain.
pub struct Keyring;

/// The credential store every command uses.
pub fn keyring() -> Keyring {
    Keyring
}

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

fn entry(key: &str) -> Result<keyring::Entry> {
    keyring::Entry::new(SERVICE, key).context("opening OS keychain entry")
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
}
