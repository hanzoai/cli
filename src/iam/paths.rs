//! The single source of truth for Hanzo IAM OIDC endpoint paths (HIP-0111).
//!
//! This mirrors `@hanzo/iam`'s `src/paths.ts`: there is ONE set of paths, no
//! legacy `/oauth/*`, no `/api/` prefix. Hanzo IAM is an OIDC provider served
//! per-brand from a configurable origin (`server_url`).
//!
//! CRITICAL GOTCHA (from the SDK): IAM serves a `200 text/html` SPA catch-all
//! for ANY unregistered path. A wrong path is silent breakage, not a 404. The
//! endpoints and the issuer come from the origin's OIDC discovery document
//! ([`endpoint`], [`trusts`]); these paths are what stands in for it only when
//! discovery was never reachable.

/// Authorization endpoint (RFC 6749 §3.1).
pub const AUTHORIZE: &str = "/v1/iam/oauth/authorize";
/// Token endpoint (RFC 6749 §3.2).
pub const TOKEN: &str = "/v1/iam/oauth/token";
/// UserInfo endpoint (OIDC Core §5.3).
pub const USERINFO: &str = "/v1/iam/oauth/userinfo";
/// Revocation endpoint (RFC 7009 §2.1) — what makes `logout` reach the server.
pub const REVOKE: &str = "/v1/iam/oauth/revoke";
/// End-session endpoint (OIDC RP-Initiated Logout 1.0 §2) — what ends the
/// browser's session, so the next sign-in asks instead of reusing it.
pub const LOGOUT: &str = "/v1/iam/oauth/logout";
/// Device authorization endpoint (RFC 8628 §3.1) — the sign-in for a machine
/// with no browser of its own. IAM publishes it in its own discovery document
/// and answers it today; this is the same address written where the CLI's other
/// three live, rather than fetched.
pub const DEVICE: &str = "/v1/iam/oauth/device";
/// An application's public descriptor (`?clientId=&responseType=code`): whether
/// IAM serves that client, and the org it signs a person in to.
pub const APPLICATION: &str = "/v1/iam/auth/application";

/// Resolve a realm to the IAM origin it signs in at.
///
/// A realm is a brand — hanzo, lux, zoo, pars, bootnode — or a brand qualified
/// by the issuer of the network it signs in on, `hanzo@id-dev.hanzo.ai`, when
/// that issuer is not the brand's own ([`realm`]). The realm is also the first
/// segment of every credential's store key and the key of its active pointer,
/// so identities from two issuers never share a slot.
pub fn server_url_for_brand(realm: &str) -> Option<String> {
    match realm.split_once('@') {
        None => brand_origin(realm).map(str::to_string),
        Some((brand, authority)) => {
            brand_origin(brand)?;
            authority_origin(authority)
        }
    }
}

/// A brand's own IAM origin. White-label is host-based: one IAM deployment
/// serves every brand and selects the tenant by the origin it is reached on.
/// This is the SINGLE place the mapping lives.
fn brand_origin(brand: &str) -> Option<&'static str> {
    match brand {
        // hanzo.ai is where a person signs in, not api.hanzo.ai: IAM's
        // authorize endpoint 302s to /login/oauth/authorize on whatever origin
        // it was reached on, and on the API host that path belongs to the Cloud
        // Console, which renders "No such page" and ends the sign-in while
        // answering 200. Sign-in starts at hanzo.ai; the issuer and every
        // endpoint come from its discovery document, never from this table.
        "hanzo" => Some("https://hanzo.ai"),
        "lux" => Some("https://lux.id"),
        "zoo" => Some("https://zoo.id"),
        "bootnode" => Some("https://id.bootno.de"),
        "pars" => Some("https://pars.id"),
        _ => None,
    }
}

/// The origin a realm's `host[:port]` names: https, or http on loopback.
/// `None` for anything that is not a lowercase DNS name with an optional port.
fn authority_origin(authority: &str) -> Option<String> {
    let (host, port) = match authority.split_once(':') {
        Some((h, p)) if !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()) => (h, Some(p)),
        Some(_) => return None,
        None => (authority, None),
    };
    let dns = !host.is_empty()
        && host.split('.').all(|l| {
            !l.is_empty()
                && !l.starts_with('-')
                && !l.ends_with('-')
                && l.bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        });
    if !dns {
        return None;
    }
    let loopback = host == "localhost"
        || host
            .parse::<std::net::Ipv4Addr>()
            .is_ok_and(|a| a.is_loopback());
    let scheme = if loopback { "http" } else { "https" };
    Some(match port {
        Some(p) => format!("{scheme}://{host}:{p}"),
        None => format!("{scheme}://{host}"),
    })
}

/// Origins a brand signed in at before it moved. Credentials they issued stay
/// valid until they expire, so they are still accepted and still name the
/// brand's own realm; a re-login replaces them.
fn former_origins(brand: &str) -> &'static [&'static str] {
    match brand {
        "hanzo" => &["https://hanzo.ai"],
        _ => &[],
    }
}

/// The brand a realm belongs to: the realm itself, or what precedes its `@`.
pub fn brand_of(realm: &str) -> &str {
    realm.split_once('@').map_or(realm, |(brand, _)| brand)
}

/// The realm `brand` signs in to on a network whose API trusts `issuer`.
///
/// A network on hanzo's own issuer (mainnet, testnet) trusts every brand's issuer beside
/// it — one API validates hanzo and its white-label brands — so each brand keeps
/// its own realm, and its credentials. A network with an issuer of its own
/// (devnet's id-dev.hanzo.ai) trusts that issuer alone: the default brand signs
/// in there under `hanzo@id-dev.hanzo.ai`, and every other brand is refused,
/// because its credential would be sent to an API that trusts none of them. An
/// issuer the realm could not name exactly — not https (or http on loopback),
/// carrying a path, a query or credentials — is refused rather than read as
/// hanzo.ai.
pub fn realm(brand: &str, issuer: &str) -> anyhow::Result<String> {
    let issuer = trim_server_url(issuer.trim());
    if Some(issuer) == brand_origin(DEFAULT_BRAND)
        || former_origins(DEFAULT_BRAND).contains(&issuer)
    {
        return Ok(brand.to_string());
    }
    if brand != DEFAULT_BRAND {
        anyhow::bail!(
            "{brand} does not sign in on a network whose API trusts {issuer} alone; \
             sign in as {DEFAULT_BRAND} there"
        );
    }
    let named = reqwest::Url::parse(issuer).ok().and_then(|url| {
        let host = url.host_str()?;
        Some(match url.port() {
            Some(port) => format!("{brand}@{host}:{port}"),
            None => format!("{brand}@{host}"),
        })
    });
    match named {
        Some(realm) if server_url_for_brand(&realm).as_deref() == Some(issuer) => Ok(realm),
        _ => anyhow::bail!(
            "the network's issuer {issuer:?} is not an origin the CLI signs in at \
             (https://<host>[:port], or http on loopback, and nothing after it)"
        ),
    }
}

/// The default brand for the `hanzo` CLI.
pub const DEFAULT_BRAND: &str = "hanzo";

/// The `--brand` suffix to suggest in a message, omitted for the default brand.
/// One place, so every hint the CLI prints is copy-pasteable.
pub fn brand_flag(realm: &str) -> String {
    match brand_of(realm) {
        DEFAULT_BRAND => String::new(),
        brand => format!(" --brand {brand}"),
    }
}

/// Strip trailing slashes from a server origin so paths concatenate cleanly.
pub fn trim_server_url(server_url: &str) -> &str {
    server_url.trim_end_matches('/')
}

/// Whether `iss` is the issuer of the IAM `realm` signs in at: what its
/// discovery names, or an issuer its brand held before it moved.
pub fn trusts(realm: &str, iss: &str) -> bool {
    let Some(origin) = server_url_for_brand(realm) else {
        return false;
    };
    let former = if realm.contains('@') {
        &[][..]
    } else {
        former_origins(brand_of(realm))
    };
    hanzo_identity::discovery::accepts(&origin, former, iss)
}

/// The address of an IAM endpoint at `origin`: the one its discovery document
/// names for `path`, else `path` on the origin.
pub fn endpoint(origin: &str, path: &str) -> String {
    let named = hanzo_identity::discovery::lookup(origin).and_then(|d| match path {
        AUTHORIZE => Some(d.authorization_endpoint),
        TOKEN => Some(d.token_endpoint),
        USERINFO => d.userinfo_endpoint,
        REVOKE => d.revocation_endpoint,
        LOGOUT => d.end_session_endpoint,
        DEVICE => d.device_authorization_endpoint,
        _ => None,
    });
    named.unwrap_or_else(|| iam_url(origin, path))
}

/// Build an absolute IAM endpoint URL from a server origin and a path constant.
///
/// ```
/// # use hanzo::iam::paths::{iam_url, TOKEN};
/// assert_eq!(iam_url("https://hanzo.ai", TOKEN), "https://hanzo.ai/v1/iam/oauth/token");
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
        assert_eq!(
            server_url_for_brand("hanzo").as_deref(),
            Some("https://hanzo.ai")
        );
        assert_eq!(
            server_url_for_brand("lux").as_deref(),
            Some("https://lux.id")
        );
        assert_eq!(
            server_url_for_brand("zoo").as_deref(),
            Some("https://zoo.id")
        );
        assert_eq!(
            server_url_for_brand("bootnode").as_deref(),
            Some("https://id.bootno.de")
        );
        assert_eq!(
            server_url_for_brand("pars").as_deref(),
            Some("https://pars.id")
        );
        assert_eq!(server_url_for_brand("nope"), None);
    }

    /// hanzo accepts the issuer it signs in at and the one it signed in at before;
    /// no other host is hanzo's, and another brand takes its own issuer alone.
    #[test]
    fn hanzo_trusts_its_two_issuers_and_nobody_else() {
        for iss in ["https://hanzo.ai", "https://hanzo.ai/", "https://hanzo.ai"] {
            assert!(trusts("hanzo", iss), "{iss}");
        }
        for iss in [
            "https://hanzo.evil.example",
            "https://id-dev.hanzo.ai",
            "https://api.hanzo.ai",
            "http://hanzo.ai",
            "https://lux.id",
        ] {
            assert!(!trusts("hanzo", iss), "{iss}");
        }
        assert!(trusts("lux", "https://lux.id") && !trusts("lux", "https://hanzo.ai"));
        assert!(
            trusts("hanzo@id-dev.hanzo.ai", "https://id-dev.hanzo.ai")
                && !trusts("hanzo@id-dev.hanzo.ai", "https://hanzo.ai")
        );
    }

    /// A network with its own issuer signs the default brand in there, under a
    /// realm of its own, and refuses every other brand; a network on hanzo.ai
    /// keeps each brand's own realm, and so the credentials it already holds.
    #[test]
    fn a_network_with_its_own_issuer_is_a_realm_of_its_own() {
        for own in [
            "https://hanzo.ai",
            "https://hanzo.ai/",
            "https://hanzo.ai",
            "https://hanzo.ai/",
        ] {
            assert_eq!(realm("hanzo", own).unwrap(), "hanzo", "{own}");
        }
        assert_eq!(realm("lux", "https://hanzo.ai").unwrap(), "lux");
        assert_eq!(realm("lux", "https://hanzo.ai").unwrap(), "lux");
        let dev = realm("hanzo", "https://id-dev.hanzo.ai").unwrap();
        assert_eq!(dev, "hanzo@id-dev.hanzo.ai");
        assert_eq!(
            server_url_for_brand(&dev).as_deref(),
            Some("https://id-dev.hanzo.ai")
        );
        assert_eq!(brand_of(&dev), "hanzo");
        assert_eq!(brand_flag(&dev), "");
        let local = realm("hanzo", "http://localhost:8000").unwrap();
        assert_eq!(
            server_url_for_brand(&local).as_deref(),
            Some("http://localhost:8000")
        );
        for brand in ["lux", "zoo", "pars", "bootnode"] {
            assert!(
                realm(brand, "https://id-dev.hanzo.ai").is_err(),
                "{brand} signs in on devnet"
            );
        }
    }

    /// An issuer the realm cannot name exactly is refused, never read as the
    /// brand's own: that would hand a hanzo.ai credential to another network.
    #[test]
    fn an_issuer_the_realm_cannot_name_is_refused() {
        for bad in [
            "http://id-dev.hanzo.ai",
            "https://id-dev.hanzo.ai/v1",
            "https://id-dev.hanzo.ai?x=1",
            "https://u:p@id-dev.hanzo.ai",
            "ftp://id-dev.hanzo.ai",
            "id-dev.hanzo.ai",
            "http://127.evil.example",
        ] {
            assert!(realm("hanzo", bad).is_err(), "{bad} was accepted");
        }
        for bad in [
            "hanzo@",
            "hanzo@ID.example",
            "hanzo@a/b",
            "hanzo@a:x",
            "nope@id.example",
            "hanzo@-a.example",
        ] {
            assert_eq!(server_url_for_brand(bad), None, "{bad} resolved");
        }
    }

    /// Plain http only on an address that is loopback, parsed — never on a name
    /// that merely starts like one.
    #[test]
    fn only_a_loopback_address_signs_in_over_http() {
        assert_eq!(
            server_url_for_brand("hanzo@127.0.0.1:8000").as_deref(),
            Some("http://127.0.0.1:8000")
        );
        assert_eq!(
            server_url_for_brand("hanzo@localhost").as_deref(),
            Some("http://localhost")
        );
        assert_eq!(
            server_url_for_brand("hanzo@127.evil.example").as_deref(),
            Some("https://127.evil.example")
        );
        assert_eq!(
            server_url_for_brand("hanzo@127.0.0.1.example").as_deref(),
            Some("https://127.0.0.1.example")
        );
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
            let origin = origin.as_str();
            assert!(
                !origin.contains("://api."),
                "{brand} signs in against {origin}, which is an API host, not a login host"
            );
        }
    }

    #[test]
    fn endpoints_are_hip0111_exact() {
        // No /api/ prefix, no legacy /oauth/*. Exactly the HIP-0111 paths.
        assert_eq!(
            iam_url("https://hanzo.ai", AUTHORIZE),
            "https://hanzo.ai/v1/iam/oauth/authorize"
        );
        assert_eq!(
            iam_url("https://hanzo.ai", TOKEN),
            "https://hanzo.ai/v1/iam/oauth/token"
        );
        assert_eq!(
            iam_url("https://hanzo.ai", USERINFO),
            "https://hanzo.ai/v1/iam/oauth/userinfo"
        );
    }

    #[test]
    fn trailing_slashes_are_trimmed() {
        assert_eq!(
            iam_url("https://lux.id/", TOKEN),
            "https://lux.id/v1/iam/oauth/token"
        );
        assert_eq!(
            iam_url("https://lux.id///", USERINFO),
            "https://lux.id/v1/iam/oauth/userinfo"
        );
    }
}
