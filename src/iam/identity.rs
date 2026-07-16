//! `Identity` — WHO a token authenticates as.
//!
//! One value, derived from one place: the token's own claims. Everything that
//! needs to know which principal a stored credential belongs to reads this.

use anyhow::{bail, Context, Result};
use base64::Engine;
use serde::Deserialize;
use std::fmt;
use std::str::FromStr;

/// Who a token authenticates as. Derived from the token's OWN claims, so a
/// stored credential can never be mislabeled into another principal's slot.
/// Casdoor names a principal `owner/name`; `owner` is ALSO the org the gateway
/// bills AND the SuperAdmin predicate — one value, three uses.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Identity {
    pub owner: String,
    pub name: String,
}

/// The claims we read off an access token. Casdoor issues `owner` (the org) and
/// `name` (the username) on every access token it mints.
#[derive(Debug, Deserialize)]
struct Claims {
    owner: String,
    name: String,
}

impl Identity {
    /// Build an identity from already-trusted components, validating both.
    ///
    /// Private on purpose: an `Identity` may only enter the system from
    /// [`Identity::from_access_token`] (a token's own claims) or
    /// [`Identity::from_str`] (a user SELECTING among identities that already
    /// exist). Neither can file a credential under a name of the caller's
    /// choosing — see `iam::store::add`, which takes no identity argument.
    fn new(owner: impl Into<String>, name: impl Into<String>) -> Result<Self> {
        let (owner, name) = (owner.into(), name.into());
        check_component("owner", &owner)?;
        check_component("name", &name)?;
        Ok(Self { owner, name })
    }

    /// Derive the identity from an access token's OWN claims, offline.
    ///
    /// THIS LABELS OUR OWN STORAGE ONLY. It is NEVER an authorization decision.
    /// The decode is deliberately unverified — we hold no signing key, and
    /// filing a credential must not need a network round-trip. SuperAdmin
    /// (`owner == "admin"`) and billing are decided SERVER-SIDE from the
    /// token the server itself verifies; forging `owner` here only mislabels
    /// the forger's own keychain slot and grants nothing. Do not let this
    /// decode gate anything.
    pub fn from_access_token(access_token: &str) -> Result<Self> {
        let mut parts = access_token.split('.');
        let payload = match (parts.next(), parts.next(), parts.next(), parts.next()) {
            (Some(h), Some(p), Some(s), None) if !h.is_empty() && !p.is_empty() && !s.is_empty() => p,
            // A key is not an identity. An `hk-` gateway key has no derivable
            // principal, so filing it in an identity-keyed store would mean
            // FABRICATING one — worse than refusing. Name the alternative rather
            // than dead-ending: "not a token" tells a CI user nothing about why.
            //
            // If a real machine-to-machine caller ever needs `hk-`, the answer is
            // an env read at the point of use (`HANZO_API_KEY` → the gateway),
            // NOT an identity in this store. Do not re-litigate this into a
            // synthetic principal.
            _ => bail!(
                "not a hanzo.id access token: the CLI files a credential under the `owner`/`name` \
                 claims the token itself carries, and this value has none.\n\
                 An `hk-` gateway API key identifies no principal, so it is not an identity and \
                 cannot be stored as one.\n\
                 Run `hanzo login` to sign in as a human identity (it obtains an IAM access token)."
            ),
        };
        // JWT payloads are base64url WITHOUT padding (RFC 7515 §2); tolerate a
        // padded encoder rather than fail on a cosmetic difference.
        let raw = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(payload.trim_end_matches('='))
            .context("decoding access-token claims")?;
        let claims: Claims = serde_json::from_slice(&raw)
            .context("parsing access-token claims (no `owner`/`name`?)")?;
        Self::new(claims.owner, claims.name)
    }
}

/// Reject anything that could break out of its slot in the keychain key
/// (`{brand}/{owner}/{name}`) or the `owner/name` index string. A claim is
/// attacker-influenced data: an `owner` of `../hanzo` or `a/b` would let a
/// forged token address ANOTHER identity's storage slot. Structure over trust.
fn check_component(field: &str, v: &str) -> Result<()> {
    if v.is_empty() {
        bail!("token claim `{field}` is empty");
    }
    if v.len() > 128 {
        bail!("token claim `{field}` is too long ({} > 128)", v.len());
    }
    if !v.starts_with(|c: char| c.is_ascii_alphanumeric()) {
        bail!("token claim `{field}` must start with a letter or digit: {v:?}");
    }
    if let Some(bad) = v.chars().find(|c| !matches!(c, 'a'..='z' | 'A'..='Z' | '0'..='9' | '.' | '_' | '-' | '@')) {
        bail!("token claim `{field}` contains an unsupported character {bad:?}: {v:?}");
    }
    Ok(())
}

impl fmt::Display for Identity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.owner, self.name)
    }
}

/// A user-supplied selector naming an identity that ALREADY exists: the exact
/// `owner/name`, or a bare `owner` to be resolved when it is unambiguous.
/// Selecting is not labeling — resolution only ever returns a stored identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Selector {
    Exact(Identity),
    Owner(String),
}

impl FromStr for Selector {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self> {
        match s.split_once('/') {
            Some((owner, name)) => Ok(Selector::Exact(Identity::new(owner, name)?)),
            None => {
                check_component("owner", s)?;
                Ok(Selector::Owner(s.to_string()))
            }
        }
    }
}

#[cfg(test)]
pub(crate) mod testjwt {
    use base64::Engine;

    /// Mint an unsigned-but-well-formed JWT carrying `owner`/`name`. The CLI
    /// never verifies the signature (that is the server's job), so a fixed
    /// placeholder is faithful to what the decode path actually sees.
    pub fn jwt(owner: &str, name: &str) -> String {
        claims_jwt(&format!(r#"{{"owner":"{owner}","name":"{name}","sub":"u-1"}}"#))
    }

    pub fn claims_jwt(claims_json: &str) -> String {
        let b64 = |b: &[u8]| base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(b);
        format!(
            "{}.{}.{}",
            b64(br#"{"alg":"RS256","typ":"JWT"}"#),
            b64(claims_json.as_bytes()),
            "c2ln" // signature bytes are irrelevant to a labeling decode
        )
    }
}

#[cfg(test)]
mod tests {
    use super::testjwt::{claims_jwt, jwt};
    use super::*;

    #[test]
    fn identity_is_derived_from_the_tokens_own_claims() {
        let id = Identity::from_access_token(&jwt("admin", "z")).unwrap();
        assert_eq!(id.owner, "admin");
        assert_eq!(id.name, "z");
        assert_eq!(id.to_string(), "admin/z");

        let id = Identity::from_access_token(&jwt("hanzo", "z")).unwrap();
        assert_eq!(id.to_string(), "hanzo/z");
    }

    /// The billing key IS `owner` — one value, no separate selector anywhere.
    #[test]
    fn owner_is_the_billing_org() {
        let su = Identity::from_access_token(&jwt("admin", "z")).unwrap();
        let org = Identity::from_access_token(&jwt("hanzo", "z")).unwrap();
        // Same human, same username — the ONLY thing that distinguishes the
        // billing org (and the SuperAdmin predicate) is `owner`.
        assert_eq!(su.name, org.name);
        assert_ne!(su.owner, org.owner);
    }

    #[test]
    fn a_non_jwt_token_has_no_derivable_identity() {
        // An `hk-` gateway key carries no identity claims; it cannot be filed.
        for bad in ["hk-abcdef", "", "a.b", "a.b.c.d", "...", "not a token"] {
            assert!(
                Identity::from_access_token(bad).is_err(),
                "expected {bad:?} to be rejected"
            );
        }
    }

    #[test]
    fn claims_without_owner_or_name_are_rejected() {
        assert!(Identity::from_access_token(&claims_jwt(r#"{"name":"z"}"#)).is_err());
        assert!(Identity::from_access_token(&claims_jwt(r#"{"owner":"admin"}"#)).is_err());
        assert!(Identity::from_access_token(&claims_jwt(r#"{"owner":"","name":"z"}"#)).is_err());
    }

    /// A claim is attacker-influenced. A separator in `owner`/`name` would let a
    /// forged token address another identity's keychain slot — reject it at the
    /// value boundary so no slot can ever be spoofed.
    #[test]
    fn claims_cannot_inject_the_key_separator_or_traverse() {
        for (owner, name) in [
            ("hanzo/admin", "z"),
            ("admin", "z/../hanzo"),
            ("..", "z"),
            (".hidden", "z"),
            ("admin", ""),
            ("ad min", "z"),
            ("admin", "z\u{0}"),
            ("admin\\z", "z"),
        ] {
            let token = claims_jwt(&format!(r#"{{"owner":"{owner}","name":"{name}"}}"#));
            assert!(
                Identity::from_access_token(&token).is_err(),
                "expected {owner:?}/{name:?} to be rejected"
            );
        }
    }

    #[test]
    fn selector_parses_exact_and_bare_owner() {
        assert_eq!(
            "admin/z".parse::<Selector>().unwrap(),
            Selector::Exact(Identity::new("admin", "z").unwrap())
        );
        assert_eq!(
            "admin".parse::<Selector>().unwrap(),
            Selector::Owner("admin".to_string())
        );
        assert!("admin/".parse::<Selector>().is_err());
        assert!("/z".parse::<Selector>().is_err());
        assert!("a/b/c".parse::<Selector>().is_err());
    }
}
