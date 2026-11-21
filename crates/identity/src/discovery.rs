//! OIDC discovery (RFC 8414 / OpenID Connect Discovery 1.0): where an origin's
//! IAM issues tokens and which endpoints it serves.
//!
//! The origin a person signs in at and the issuer that signs the tokens are
//! different facts, and may be different hosts. The origin is configuration;
//! the issuer, the authorize, token, JWKS and the other endpoints are what the
//! origin's `/.well-known/openid-configuration` says they are. `hanzo` and
//! `hanzod` both ask here, so there is one reading of it.
//!
//! The document is cached on disk for [`TTL`]. When a refresh fails the last
//! good copy is kept and used, however old: an IAM outage must not turn into
//! every stored credential being refused.

use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// How long a fetched document is trusted without asking again.
pub const TTL: Duration = Duration::from_secs(3600);
const TIMEOUT: Duration = Duration::from_secs(5);

/// The parts of a discovery document the Hanzo programs act on.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Document {
    pub issuer: String,
    pub authorization_endpoint: String,
    pub token_endpoint: String,
    pub jwks_uri: String,
    #[serde(default)]
    pub userinfo_endpoint: Option<String>,
    #[serde(default)]
    pub revocation_endpoint: Option<String>,
    #[serde(default)]
    pub end_session_endpoint: Option<String>,
    #[serde(default)]
    pub device_authorization_endpoint: Option<String>,
}

#[derive(Serialize, Deserialize)]
struct Cached {
    fetched_at: u64,
    document: Document,
}

/// `https://host[:port]` — or http on a loopback host — and nothing after it
/// but a path. Credentials are only ever sent to such an address.
fn sendable(url: &str) -> bool {
    let Some((scheme, rest)) = url.split_once("://") else { return false };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    if authority.is_empty() || authority.contains('@') {
        return false;
    }
    let host = authority.rsplit_once(':').map_or(authority, |(h, _)| h);
    match scheme {
        "https" => true,
        "http" => matches!(host, "localhost" | "127.0.0.1" | "[::1]"),
        _ => false,
    }
}

/// Read a discovery document. An issuer or endpoint a credential could not be
/// sent to is refused, not passed along.
pub fn parse(body: &str) -> Result<Document> {
    let doc: Document = serde_json::from_str(body).context("not an OpenID configuration")?;
    for (name, url) in [
        ("issuer", &doc.issuer),
        ("authorization_endpoint", &doc.authorization_endpoint),
        ("token_endpoint", &doc.token_endpoint),
        ("jwks_uri", &doc.jwks_uri),
    ]
    .into_iter()
    .chain(
        [
            ("userinfo_endpoint", &doc.userinfo_endpoint),
            ("revocation_endpoint", &doc.revocation_endpoint),
            ("end_session_endpoint", &doc.end_session_endpoint),
            ("device_authorization_endpoint", &doc.device_authorization_endpoint),
        ]
        .into_iter()
        .filter_map(|(n, u)| u.as_ref().map(|u| (n, u))),
    ) {
        if !sendable(url) {
            bail!("the OpenID configuration's {name} {url:?} is not an https address");
        }
    }
    Ok(doc)
}

/// The address an origin publishes its configuration at.
fn well_known(origin: &str) -> String {
    format!("{}/.well-known/openid-configuration", origin.trim_end_matches('/'))
}

/// Fetch and read the document `origin` publishes. BLOCKING, on a thread of its
/// own, so it is safe to call from inside an async runtime.
pub fn fetch(origin: &str) -> Result<Document> {
    let url = well_known(origin);
    let body = std::thread::scope(|s| {
        s.spawn(|| -> Result<String> {
            let client = reqwest::blocking::Client::builder().timeout(TIMEOUT).build()?;
            let resp = client.get(&url).send()?.error_for_status()?;
            Ok(resp.text()?)
        })
        .join()
        .map_err(|_| anyhow!("discovery thread panicked"))?
    })
    .with_context(|| format!("fetching {url}"))?;
    parse(&body).with_context(|| format!("reading {url}"))
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn cache_file(dir: &Path, origin: &str) -> PathBuf {
    let name: String = origin
        .trim_end_matches('/')
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '.' || c == '-' { c } else { '_' })
        .collect();
    dir.join(format!("{name}.json"))
}

/// The document for `origin`: the cached copy if it is younger than `ttl`, else
/// a fresh fetch, else the last good copy whatever its age. Errors only when
/// nothing was ever fetched and the origin cannot be reached.
pub fn resolve_in(dir: &Path, origin: &str, ttl: Duration) -> Result<Document> {
    let file = cache_file(dir, origin);
    let held: Option<Cached> =
        std::fs::read_to_string(&file).ok().and_then(|s| serde_json::from_str(&s).ok());
    if let Some(c) = &held {
        if now().saturating_sub(c.fetched_at) < ttl.as_secs() {
            return Ok(c.document.clone());
        }
    }
    match fetch(origin) {
        Ok(document) => {
            // A cache that cannot be written costs a refetch, not the answer.
            let _ = std::fs::create_dir_all(dir).and_then(|()| {
                let tmp = file.with_extension("tmp");
                std::fs::write(&tmp, serde_json::to_vec(&Cached { fetched_at: now(), document: document.clone() })?)?;
                std::fs::rename(&tmp, &file)
            });
            Ok(document)
        }
        Err(e) => held.map(|c| c.document).ok_or(e),
    }
}

/// [`resolve_in`] in the user's cache directory, for [`TTL`].
pub fn resolve(origin: &str) -> Result<Document> {
    let dir = dirs::cache_dir().ok_or_else(|| anyhow!("no cache directory"))?.join("hanzo").join("discovery");
    resolve_in(&dir, origin, TTL)
}

/// Whether an origin publishes discovery for a person to rely on: https. A
/// loopback http IAM is a development server that stands on its own address.
pub fn published(origin: &str) -> bool {
    origin.starts_with("https://")
}

/// [`resolve`] for an origin that [`published`] a document, else `None`.
pub fn lookup(origin: &str) -> Option<Document> {
    published(origin).then(|| resolve(origin).ok()).flatten()
}

/// Whether a token claiming `iss` was issued by the IAM at `origin`, whose
/// discovery document is `doc`: the issuer it names. `former` are earlier
/// issuers of the same IAM, whose still-valid tokens stay accepted until they
/// expire. With no document the origin itself is the expected issuer.
pub fn issued_by(doc: Option<&Document>, origin: &str, former: &[&str], iss: &str) -> bool {
    let iss = iss.trim_end_matches('/');
    let expected = doc.map_or(origin, |d| d.issuer.as_str()).trim_end_matches('/');
    iss == expected || former.iter().any(|f| iss == f.trim_end_matches('/'))
}

/// [`issued_by`] against the document [`lookup`] finds for `origin`.
pub fn accepts(origin: &str, former: &[&str], iss: &str) -> bool {
    issued_by(lookup(origin).as_ref(), origin, former, iss)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;

    /// Serve `body` as the discovery document, `hits` times, from loopback.
    fn serve(body: String, hits: usize) -> (String, std::thread::JoinHandle<()>) {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let origin = format!("http://{}", l.local_addr().unwrap());
        let h = std::thread::spawn(move || {
            for _ in 0..hits {
                let (mut s, _) = l.accept().unwrap();
                let mut buf = [0u8; 2048];
                let _ = s.read(&mut buf);
                let _ = write!(
                    s,
                    "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                );
            }
        });
        (origin, h)
    }

    /// A document whose issuer and endpoints are on another host than the origin
    /// it was fetched from: the login origin is not the issuer.
    fn doc(issuer: &str) -> String {
        format!(
            r#"{{"issuer":"{issuer}","authorization_endpoint":"{issuer}/v1/iam/oauth/authorize","token_endpoint":"{issuer}/v1/iam/oauth/token","jwks_uri":"{issuer}/v1/iam/.well-known/jwks","userinfo_endpoint":"{issuer}/v1/iam/oauth/userinfo"}}"#
        )
    }

    #[test]
    fn the_issuer_may_be_another_host_than_the_origin() {
        let (origin, h) = serve(doc("https://issuer.example"), 1);
        let dir = tempfile::tempdir().unwrap();
        let d = resolve_in(dir.path(), &origin, TTL).unwrap();
        h.join().unwrap();
        assert_eq!(d.issuer, "https://issuer.example");
        assert_eq!(d.token_endpoint, "https://issuer.example/v1/iam/oauth/token");
        assert_ne!(d.issuer, origin);
    }

    /// A fresh copy is not refetched (the server answers once), and a stale one
    /// survives the origin going away.
    #[test]
    fn cached_for_the_ttl_and_kept_when_a_refresh_fails() {
        let (origin, h) = serve(doc("https://issuer.example"), 1);
        let dir = tempfile::tempdir().unwrap();
        let first = resolve_in(dir.path(), &origin, TTL).unwrap();
        h.join().unwrap(); // the server is gone now
        assert_eq!(resolve_in(dir.path(), &origin, TTL).unwrap(), first);
        // Zero TTL forces a refresh, which fails: the last good copy answers.
        assert_eq!(resolve_in(dir.path(), &origin, Duration::ZERO).unwrap(), first);
        // Never fetched, unreachable: an error, not a guess.
        assert!(resolve_in(tempfile::tempdir().unwrap().path(), &origin, TTL).is_err());
    }

    #[test]
    fn an_address_a_credential_cannot_go_to_is_refused() {
        for bad in ["http://issuer.example", "ftp://x.example", "https://u:p@x.example", "x.example", "http://127.evil.example"] {
            assert!(parse(&doc(bad)).is_err(), "{bad}");
        }
        assert!(parse(&doc("http://localhost:8000")).is_ok());
        assert!(parse("<html>").is_err());
    }

    /// A token is accepted by the issuer discovery names, even on another host
    /// than the login origin, and by a former issuer; never by the origin alone
    /// once discovery says otherwise.
    #[test]
    fn the_discovered_issuer_is_the_one_accepted() {
        let d = parse(&doc("https://issuer.example")).unwrap();
        let origin = "https://login.example";
        assert!(issued_by(Some(&d), origin, &[], "https://issuer.example"));
        assert!(issued_by(Some(&d), origin, &[], "https://issuer.example/"));
        assert!(!issued_by(Some(&d), origin, &[], origin));
        assert!(issued_by(Some(&d), origin, &["https://old.example"], "https://old.example"));
        assert!(!issued_by(Some(&d), origin, &[], "https://old.example"));
        assert!(issued_by(None, origin, &[], origin));
    }
}
