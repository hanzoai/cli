//! PKCE (Proof Key for Code Exchange, RFC 7636) for OAuth2/OIDC.
//!
//! Mirrors `@hanzo/iam`'s `src/pkce.ts`: the verifier is the base64url (no pad)
//! encoding of 32 cryptographically-random bytes (256 bits of entropy → a
//! 43-char `[A-Za-z0-9-_]` string, within RFC 7636's 43–128 range); the
//! challenge is the base64url-encoded SHA-256 of the verifier (the `S256`
//! method). `state` is an independent 256-bit value for CSRF protection.

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use rand::{rngs::OsRng, RngCore};
use sha2::{Digest, Sha256};

/// 256 bits of entropy for both the verifier and the state.
const ENTROPY_BYTES: usize = 32;

/// A PKCE verifier/challenge pair. Send `challenge` + `method=S256` on the
/// authorize request; send `verifier` on the token exchange.
pub struct Pkce {
    pub verifier: String,
    pub challenge: String,
}

fn random_base64url(byte_len: usize) -> String {
    let mut bytes = vec![0u8; byte_len];
    OsRng.fill_bytes(&mut bytes);
    URL_SAFE_NO_PAD.encode(bytes)
}

/// Generate a PKCE code verifier + S256 challenge pair.
pub fn generate_pkce() -> Pkce {
    let verifier = random_base64url(ENTROPY_BYTES);
    let challenge = challenge_for(&verifier);
    Pkce { verifier, challenge }
}

/// Compute the S256 challenge for a given verifier (split out for testing
/// against RFC 7636's known-answer vector).
pub fn challenge_for(verifier: &str) -> String {
    let digest = Sha256::digest(verifier.as_bytes());
    URL_SAFE_NO_PAD.encode(digest)
}

/// Generate a high-entropy (256-bit) `state` parameter for CSRF protection.
pub fn generate_state() -> String {
    random_base64url(ENTROPY_BYTES)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RFC 7636 Appendix B — the canonical S256 known-answer test. If this
    /// passes, our challenge derivation is byte-identical to the spec (and to
    /// the @hanzo/iam SDK).
    #[test]
    fn rfc7636_appendix_b_known_answer() {
        let verifier = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
        let challenge = challenge_for(verifier);
        assert_eq!(challenge, "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM");
    }

    #[test]
    fn verifier_is_43_chars_url_safe() {
        let pkce = generate_pkce();
        // base64url(32 bytes) with no padding == 43 chars, within RFC 7636 range.
        assert_eq!(pkce.verifier.len(), 43);
        assert!(pkce
            .verifier
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'));
    }

    #[test]
    fn challenge_matches_verifier() {
        let pkce = generate_pkce();
        assert_eq!(pkce.challenge, challenge_for(&pkce.verifier));
    }

    #[test]
    fn state_is_high_entropy_and_unique() {
        let a = generate_state();
        let b = generate_state();
        assert_eq!(a.len(), 43);
        assert_ne!(a, b, "state must be unpredictable per-flow");
    }
}
