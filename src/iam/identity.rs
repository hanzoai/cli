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
/// IAM names a principal `owner/name`; `owner` is ALSO the org the gateway
/// bills AND the SuperAdmin predicate — one value, three uses.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Identity {
    pub owner: String,
    pub name: String,
}

/// The claims we read off an access token. IAM issues `owner` (the org) and
/// `name` (the username) on every access token it mints, and `email` for a
/// human identity.
#[derive(Debug, Deserialize)]
struct Claims {
    owner: String,
    name: String,
    #[serde(default)]
    email: String,
}

/// The address a token claims, or `None`.
///
/// The SAME unverified decode as [`Identity::from_access_token`], for the same
/// kind of use: a label, never a decision. `hanzo link` hands this to the
/// share's frontend as the one address that may open the published shell, and
/// the frontend then asks hanzo.id who the visitor actually is — so a wrong
/// value here can only lock the publisher out of their own terminal, never let
/// anybody else in.
pub fn email(access_token: &str) -> Option<String> {
    let claims: Claims = serde_json::from_slice(&payload(access_token)?).ok()?;
    (!claims.email.trim().is_empty()).then_some(claims.email)
}

/// A JWT's claims segment, decoded. `None` for anything that is not one.
///
/// JWT payloads are base64url WITHOUT padding (RFC 7515 §2); a padded encoder is
/// tolerated rather than failed on a cosmetic difference.
fn payload(access_token: &str) -> Option<Vec<u8>> {
    let mut parts = access_token.split('.');
    let p = match (parts.next(), parts.next(), parts.next(), parts.next()) {
        (Some(h), Some(p), Some(s), None) if !h.is_empty() && !p.is_empty() && !s.is_empty() => p,
        _ => return None,
    };
    base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(p.trim_end_matches('='))
        .ok()
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
        let raw = match payload(access_token) {
            Some(raw) => raw,
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
                 Run `hanzo auth login` to sign in as a human identity (it obtains an IAM access token)."
            ),
        };
        let claims: Claims = serde_json::from_slice(&raw)
            .context("parsing access-token claims (no `owner`/`name`?)")?;
        Self::new(claims.owner, claims.name)
    }
}

/// Reject anything that could break out of its slot in the keychain key
/// (`{brand}/{owner}/{name}`) or the `owner/name` index string. A claim is
/// attacker-influenced data: an `owner` of `../hanzo` or `a/b` would let a
/// forged token address ANOTHER identity's storage slot. Structure over trust
/// — and ONLY structure: IAM mints usernames like "Grace Hopper" or "José",
/// so the rule is a deny-list of what actually escapes a slot (separators,
/// control bytes, non-space whitespace), never an allowlist of blessed ASCII.
fn check_component(field: &str, v: &str) -> Result<()> {
    if v.is_empty() {
        bail!("token claim `{field}` is empty");
    }
    if v.len() > 128 {
        bail!("token claim `{field}` is too long ({} > 128)", v.len());
    }
    if !v.starts_with(|c: char| c.is_alphanumeric()) {
        bail!("token claim `{field}` must start with a letter or digit: {v:?}");
    }
    if v.ends_with(' ') {
        bail!("token claim `{field}` ends with a space: {v:?}");
    }
    if let Some(bad) = v
        .chars()
        .find(|&c| c == '/' || c == '\\' || c.is_control() || (c.is_whitespace() && c != ' '))
    {
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

    // `hanzo link` publishes a shell FOR an address, so an address that is not
    // there has to be absent rather than empty: the frontend reads it as a glob,
    // and an empty glob matches nothing — a terminal its own publisher cannot
    // open. Refusing to publish says so; publishing an unopenable one does not.
    #[test]
    fn an_address_is_read_off_the_token_or_reported_missing() {
        assert_eq!(
            email(&claims_jwt(r#"{"owner":"hanzo","name":"a","email":"a@hanzo.ai"}"#)).as_deref(),
            Some("a@hanzo.ai")
        );
        for without in [
            r#"{"owner":"hanzo","name":"a"}"#,
            r#"{"owner":"hanzo","name":"a","email":""}"#,
            r#"{"owner":"hanzo","name":"a","email":"   "}"#,
        ] {
            assert_eq!(email(&claims_jwt(without)), None, "{without}");
        }
        // A gateway key is not a token and claims nothing.
        assert_eq!(email("hk-abc123"), None);
        assert_eq!(email(""), None);
    }

    #[test]
    fn identity_is_derived_from_the_tokens_own_claims() {
        let id = Identity::from_access_token(&jwt("admin", "z")).unwrap();
        assert_eq!(id.owner, "admin");
        assert_eq!(id.name, "z");
        assert_eq!(id.to_string(), "admin/z");

        let id = Identity::from_access_token(&jwt("hanzo", "z")).unwrap();
        assert_eq!(id.to_string(), "hanzo/z");
    }

    /// IAM mints human usernames — an interior space or a non-ASCII letter is a
    /// legal claim, and rejecting it turns a successful browser sign-in into a
    /// dead end (v1.9.9 did exactly this to "Grace Hopper").
    #[test]
    fn human_usernames_are_legal_claims() {
        let id = Identity::from_access_token(&jwt("hanzo", "Grace Hopper")).unwrap();
        assert_eq!(id.to_string(), "hanzo/Grace Hopper");
        let id = Identity::from_access_token(&jwt("hanzo", "José")).unwrap();
        assert_eq!(id.name, "José");
        // And the selector round-trips it, quoted at the shell like any name.
        assert_eq!(
            "hanzo/Grace Hopper".parse::<Selector>().unwrap(),
            Selector::Exact(Identity::new("hanzo", "Grace Hopper").unwrap())
        );
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
            ("admin", "z\u{0}"),
            ("admin\\z", "z"),
            ("admin", "z\tk"),
            ("admin", "z "),
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
