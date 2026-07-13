//! Read a JWT's claims WITHOUT verifying its signature.
//!
//! Used only to record/display the local user's own identity (email/sub/owner/
//! exp) when persisting to the ecosystem-shared flat store — it never grants
//! trust, so base64url-decoding the middle segment is all that is needed.

use anyhow::{anyhow, Result};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use serde_json::{Map, Value};

/// Decode the payload segment of a JWT into its claims map. The signature is
/// intentionally NOT checked — the result is for local display/record only.
pub fn decode_claims(token: &str) -> Result<Map<String, Value>> {
    let parts: Vec<&str> = token.split('.').collect();
    if parts.len() < 2 {
        return Err(anyhow!("not a JWT (need 3 dot-separated segments)"));
    }
    let seg = parts[1].trim_end_matches('=');
    let bytes = URL_SAFE_NO_PAD
        .decode(seg)
        .map_err(|e| anyhow!("decode JWT payload: {e}"))?;
    match serde_json::from_slice(&bytes).map_err(|e| anyhow!("parse JWT claims: {e}"))? {
        Value::Object(m) => Ok(m),
        _ => Err(anyhow!("JWT claims are not a JSON object")),
    }
}

/// Read a string claim, returning "" when absent or not a string.
pub fn claim_str(claims: &Map<String, Value>, key: &str) -> String {
    claims
        .get(key)
        .and_then(Value::as_str)
        .map(String::from)
        .unwrap_or_default()
}

/// Read a numeric claim as i64 (JWT `exp`/`iat` are numeric), 0 when absent.
pub fn claim_i64(claims: &Map<String, Value>, key: &str) -> i64 {
    claims.get(key).and_then(Value::as_i64).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    // build an unsigned JWT (alg=none) carrying claims — enough to exercise the
    // signature-free decode the CLI uses for display.
    fn make_jwt(claims: Value) -> String {
        let hdr = URL_SAFE_NO_PAD.encode(br#"{"alg":"none","typ":"JWT"}"#);
        let payload = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&claims).unwrap());
        format!("{hdr}.{payload}.sig")
    }

    #[test]
    fn decodes_claims() {
        let tok = make_jwt(serde_json::json!({
            "email": "z@hanzo.ai", "owner": "hanzo", "sub": "abc", "exp": 1783110016i64
        }));
        let claims = decode_claims(&tok).unwrap();
        assert_eq!(claim_str(&claims, "email"), "z@hanzo.ai");
        assert_eq!(claim_str(&claims, "owner"), "hanzo");
        assert_eq!(claim_str(&claims, "sub"), "abc");
        assert_eq!(claim_i64(&claims, "exp"), 1783110016);
    }

    #[test]
    fn missing_claim_is_empty() {
        let claims = decode_claims(&make_jwt(serde_json::json!({"sub": "x"}))).unwrap();
        assert_eq!(claim_str(&claims, "email"), "");
        assert_eq!(claim_i64(&claims, "exp"), 0);
    }

    #[test]
    fn rejects_non_jwt() {
        assert!(decode_claims("not-a-jwt").is_err());
    }
}
