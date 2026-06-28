//! The single source of truth for Hanzo IAM OIDC endpoint paths (HIP-0111).
//!
//! This mirrors `@hanzo/iam`'s `src/paths.ts`: there is ONE set of paths, no
//! legacy `/oauth/*`, no `/api/` prefix. Hanzo IAM is a Casdoor-derived OIDC
//! provider served per-brand from a configurable origin (`server_url`).
//!
//! CRITICAL GOTCHA (from the SDK): IAM serves a `200 text/html` SPA catch-all
//! for ANY unregistered path. A wrong path is silent breakage, not a 404. So
//! these exact paths are the only ones we ever hit, and we never let a
//! discovery round-trip resolve to a different path — the hard-coded values
//! here ARE the discovery fallback.

/// Authorization endpoint (RFC 6749 §3.1).
pub const AUTHORIZE: &str = "/v1/iam/oauth/authorize";
/// Token endpoint (RFC 6749 §3.2).
pub const TOKEN: &str = "/v1/iam/oauth/token";
/// UserInfo endpoint (OIDC Core §5.3).
pub const USERINFO: &str = "/v1/iam/oauth/userinfo";

/// Resolve a brand key to its canonical IAM `server_url` origin. White-label is
/// host-based: one IAM deployment serves every brand and selects the tenant by
/// the origin it is reached on. This is the SINGLE place the mapping lives.
pub fn server_url_for_brand(brand: &str) -> Option<&'static str> {
    match brand {
        "hanzo" => Some("https://iam.hanzo.ai"),
        "lux" => Some("https://lux.id"),
        "zoo" => Some("https://zoo.id"),
        "bootnode" => Some("https://id.bootno.de"),
        "pars" => Some("https://pars.id"),
        _ => None,
    }
}

/// The default brand for the `hanzo` CLI.
pub const DEFAULT_BRAND: &str = "hanzo";

/// Strip trailing slashes from a server origin so paths concatenate cleanly.
pub fn trim_server_url(server_url: &str) -> &str {
    server_url.trim_end_matches('/')
}

/// Build an absolute IAM endpoint URL from a server origin and a path constant.
///
/// ```
/// # use hanzo::iam::paths::{iam_url, TOKEN};
/// assert_eq!(iam_url("https://iam.hanzo.ai", TOKEN), "https://iam.hanzo.ai/v1/iam/oauth/token");
/// ```
pub fn iam_url(server_url: &str, path: &str) -> String {
    format!("{}{}", trim_server_url(server_url), path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn brand_origins_are_canonical() {
        assert_eq!(server_url_for_brand("hanzo"), Some("https://iam.hanzo.ai"));
        assert_eq!(server_url_for_brand("lux"), Some("https://lux.id"));
        assert_eq!(server_url_for_brand("zoo"), Some("https://zoo.id"));
        assert_eq!(server_url_for_brand("bootnode"), Some("https://id.bootno.de"));
        assert_eq!(server_url_for_brand("pars"), Some("https://pars.id"));
        assert_eq!(server_url_for_brand("nope"), None);
    }

    #[test]
    fn endpoints_are_hip0111_exact() {
        // No /api/ prefix, no legacy /oauth/*. Exactly the HIP-0111 paths.
        assert_eq!(iam_url("https://iam.hanzo.ai", AUTHORIZE), "https://iam.hanzo.ai/v1/iam/oauth/authorize");
        assert_eq!(iam_url("https://iam.hanzo.ai", TOKEN), "https://iam.hanzo.ai/v1/iam/oauth/token");
        assert_eq!(iam_url("https://iam.hanzo.ai", USERINFO), "https://iam.hanzo.ai/v1/iam/oauth/userinfo");
    }

    #[test]
    fn trailing_slashes_are_trimmed() {
        assert_eq!(iam_url("https://lux.id/", TOKEN), "https://lux.id/v1/iam/oauth/token");
        assert_eq!(iam_url("https://lux.id///", USERINFO), "https://lux.id/v1/iam/oauth/userinfo");
    }
}
