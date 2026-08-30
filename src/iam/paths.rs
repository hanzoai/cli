//! The single source of truth for Hanzo IAM OIDC endpoint paths (HIP-0111).
//!
//! This mirrors `@hanzo/iam`'s `src/paths.ts`: there is ONE set of paths, no
//! legacy `/oauth/*`, no `/api/` prefix. Hanzo IAM is an OIDC provider served
//! per-brand from a configurable origin (`server_url`).
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
/// Revocation endpoint (RFC 7009 §2.1) — what makes `logout` reach the server.
pub const REVOKE: &str = "/v1/iam/oauth/revoke";
/// Device authorization endpoint (RFC 8628 §3.1) — the sign-in for a machine
/// with no browser of its own. IAM publishes it in its own discovery document
/// and answers it today; this is the same address written where the CLI's other
/// three live, rather than fetched.
pub const DEVICE: &str = "/v1/iam/oauth/device";

/// Resolve a brand key to its canonical IAM `server_url` origin. White-label is
/// host-based: one IAM deployment serves every brand and selects the tenant by
/// the origin it is reached on. This is the SINGLE place the mapping lives.
pub fn server_url_for_brand(brand: &str) -> Option<&'static str> {
    match brand {
        // hanzo.id, not api.hanzo.ai. The two are different services: api is
        // the API, hanzo.id is where a person signs in. IAM's authorize
        // endpoint answers on both, but it 302s to /login/oauth/authorize on
        // whatever origin it was reached on, and on api.hanzo.ai that path
        // belongs to the Cloud Console — which renders "No such page" and ends
        // the sign-in. The console answers 200 while doing it, so nothing in
        // the CLI could tell. Every other brand below already names its login
        // host; hanzo was the one that named its API.
        //
        // api.hanzo.ai's own discovery document agrees: it publishes
        // issuer = https://hanzo.id and both endpoints under it.
        "hanzo" => Some("https://hanzo.id"),
        "lux" => Some("https://lux.id"),
        "zoo" => Some("https://zoo.id"),
        "bootnode" => Some("https://id.bootno.de"),
        "pars" => Some("https://pars.id"),
        _ => None,
    }
}

/// The default brand for the `hanzo` CLI.
pub const DEFAULT_BRAND: &str = "hanzo";

/// The `--brand` suffix to suggest in a message, omitted for the default brand.
/// One place, so every hint the CLI prints is copy-pasteable.
pub fn brand_flag(brand: &str) -> String {
    if brand == DEFAULT_BRAND {
        String::new()
    } else {
        format!(" --brand {brand}")
    }
}

/// Strip trailing slashes from a server origin so paths concatenate cleanly.
pub fn trim_server_url(server_url: &str) -> &str {
    server_url.trim_end_matches('/')
}

/// Build an absolute IAM endpoint URL from a server origin and a path constant.
///
/// ```
/// # use hanzo::iam::paths::{iam_url, TOKEN};
/// assert_eq!(iam_url("https://hanzo.id", TOKEN), "https://hanzo.id/v1/iam/oauth/token");
/// ```
pub fn iam_url(server_url: &str, path: &str) -> String {
    format!("{}{}", trim_server_url(server_url), path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn brand_origins_are_canonical() {
        // The login host, not the API host — see server_url_for_brand.
        assert_eq!(server_url_for_brand("hanzo"), Some("https://hanzo.id"));
        assert_eq!(server_url_for_brand("lux"), Some("https://lux.id"));
        assert_eq!(server_url_for_brand("zoo"), Some("https://zoo.id"));
        assert_eq!(server_url_for_brand("bootnode"), Some("https://id.bootno.de"));
        assert_eq!(server_url_for_brand("pars"), Some("https://pars.id"));
        assert_eq!(server_url_for_brand("nope"), None);
    }

    /// No brand may point IAM at an `api.` host.
    ///
    /// This is the shape of the bug that broke `hanzo auth login`: the origin
    /// named api.hanzo.ai, IAM's authorize endpoint answered there and 302'd to
    /// /login/oauth/authorize on that same origin, and the Cloud Console owns
    /// that path — so the browser landed on "No such page" and the sign-in
    /// ended. The console answered 200 the whole way, so nothing could tell.
    ///
    /// An API host and a login host are different services. Asserting the shape
    /// catches the next brand added with the wrong one, which reading each
    /// entry does not.
    #[test]
    fn no_brand_signs_in_against_an_api_host() {
        for brand in ["hanzo", "lux", "zoo", "bootnode", "pars"] {
            let origin = server_url_for_brand(brand).expect("brand has an origin");
            assert!(
                !origin.contains("://api."),
                "{brand} signs in against {origin}, which is an API host, not a login host"
            );
        }
    }

    #[test]
    fn endpoints_are_hip0111_exact() {
        // No /api/ prefix, no legacy /oauth/*. Exactly the HIP-0111 paths.
        assert_eq!(iam_url("https://hanzo.id", AUTHORIZE), "https://hanzo.id/v1/iam/oauth/authorize");
        assert_eq!(iam_url("https://hanzo.id", TOKEN), "https://hanzo.id/v1/iam/oauth/token");
        assert_eq!(iam_url("https://hanzo.id", USERINFO), "https://hanzo.id/v1/iam/oauth/userinfo");
    }

    #[test]
    fn trailing_slashes_are_trimmed() {
        assert_eq!(iam_url("https://lux.id/", TOKEN), "https://lux.id/v1/iam/oauth/token");
        assert_eq!(iam_url("https://lux.id///", USERINFO), "https://lux.id/v1/iam/oauth/userinfo");
    }
}
