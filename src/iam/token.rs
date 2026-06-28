//! Secure persistence of IAM token sets in the OS keychain.
//!
//! Tokens are credentials, so they live ONLY in the platform keychain (macOS
//! Keychain, Windows Credential Manager, Linux Secret Service) via the
//! `keyring` crate — never in a plaintext file on disk. One entry per brand so
//! authenticating against multiple tenants does not clobber.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

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

fn entry(brand: &str) -> Result<keyring::Entry> {
    keyring::Entry::new(SERVICE, brand).context("opening OS keychain entry")
}

/// Persist a token set for `brand` in the OS keychain.
pub fn store(brand: &str, tokens: &TokenSet) -> Result<()> {
    let json = serde_json::to_string(tokens)?;
    entry(brand)?
        .set_password(&json)
        .context("writing credential to OS keychain")
}

/// Load the stored token set for `brand`, if any.
pub fn load(brand: &str) -> Result<Option<TokenSet>> {
    match entry(brand)?.get_password() {
        Ok(json) => Ok(Some(serde_json::from_str(&json)?)),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(e) => Err(e).context("reading credential from OS keychain"),
    }
}

/// Remove the stored credential for `brand`. Returns whether one existed.
pub fn delete(brand: &str) -> Result<bool> {
    match entry(brand)?.delete_credential() {
        Ok(()) => Ok(true),
        Err(keyring::Error::NoEntry) => Ok(false),
        Err(e) => Err(e).context("deleting credential from OS keychain"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Keychain I/O is not exercised in unit tests (it would prompt / require a
    // session keyring); the value type's serde contract is what we must pin.
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
}
