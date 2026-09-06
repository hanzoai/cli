//! Pinning a container image to what it actually is.
//!
//! A tag is a name someone can move; a digest is the content. `hanzo up`
//! measures the workload it deploys, so the reference that reaches the cluster
//! is resolved to `registry/repository@sha256:…` first. What the manifest
//! names, what containerd verifies on pull, and what the measurement covers
//! are then the same bytes by construction — a tag moved afterwards changes
//! nothing about a cluster already running.
//!
//! Only what pinning needs is implemented: the registry's manifest endpoint,
//! the anonymous token dance every registry answers with, and platform
//! selection. There is no pull here — the cluster does that.

use anyhow::{anyhow, bail, Context, Result};
use reqwest::header::{HeaderMap, ACCEPT, AUTHORIZATION, WWW_AUTHENTICATE};
use reqwest::{Client, StatusCode};
use serde_json::Value;

/// Every manifest media type a registry might answer with — the OCI pair and
/// the Docker pair, since a repository can hold either.
const MANIFESTS: &str = "application/vnd.oci.image.index.v1+json, \
     application/vnd.docker.distribution.manifest.list.v2+json, \
     application/vnd.oci.image.manifest.v1+json, \
     application/vnd.docker.distribution.manifest.v2+json";

/// A reference split into the three things a registry call needs.
#[derive(Debug, PartialEq, Eq)]
pub struct Reference {
    pub registry: String,
    pub repository: String,
    /// A tag, or `sha256:…` when the reference is already pinned.
    pub version: String,
    pub pinned: bool,
}

impl Reference {
    /// The reference as a pin on `digest`.
    fn at(&self, digest: &str) -> String {
        format!("{}/{}@{}", self.registry, self.repository, digest)
    }

    /// `https://registry/v2/repository/<kind>/<version>`.
    fn url(&self, kind: &str, version: &str) -> String {
        format!(
            "{}://{}/v2/{}/{kind}/{version}",
            scheme(&self.registry),
            self.registry,
            self.repository
        )
    }
}

/// `http` for a loopback registry, `https` for the rest. The rule every
/// registry client follows, and the reason a `localhost:5000` registry works
/// without anyone minting it a certificate.
fn scheme(registry: &str) -> &'static str {
    match registry.split(':').next().unwrap_or(registry) {
        "localhost" | "127.0.0.1" | "[::1]" => "http",
        _ => "https",
    }
}

/// Split `registry/repository:tag` or `registry/repository@sha256:…`.
///
/// The registry has to be named. Inferring one is how a reference comes to
/// mean different images on different machines, and a measurement of an image
/// nobody can name again is not worth taking.
pub fn parse(reference: &str) -> Result<Reference> {
    let (name, version, pinned) = match reference.split_once('@') {
        Some((name, digest)) => (name, digest.to_string(), true),
        None => match name_and_tag(reference) {
            (name, Some(tag)) => (name, tag.to_string(), false),
            (name, None) => (name, "latest".to_string(), false),
        },
    };
    let (registry, repository) = name
        .split_once('/')
        .ok_or_else(|| anyhow!("{reference}: name the registry, as in ghcr.io/owner/image"))?;
    if !registry.contains('.') && !registry.contains(':') && registry != "localhost" {
        bail!("{reference}: name the registry, as in ghcr.io/owner/image");
    }
    if repository.is_empty() || version.is_empty() {
        bail!("{reference}: not an image reference");
    }
    Ok(Reference {
        registry: registry.to_string(),
        repository: repository.to_string(),
        version,
        pinned,
    })
}

/// A `:` after the last `/` is a tag; before it, a registry port.
fn name_and_tag(reference: &str) -> (&str, Option<&str>) {
    match reference.rsplit_once(':') {
        Some((name, tag)) if !tag.contains('/') => (name, Some(tag)),
        _ => (reference, None),
    }
}

/// The platform string an OCI index uses for this machine. The guest runs the
/// host's architecture — the vm emulates nothing — so the host's is the one to
/// ask for.
pub fn architecture() -> &'static str {
    match std::env::consts::ARCH {
        "aarch64" => "arm64",
        "x86_64" => "amd64",
        other => other,
    }
}

/// Resolve `reference` to the digest of the image for `architecture`.
///
/// An already-pinned reference is returned untouched: the caller named the
/// content, and asking a registry to confirm its own answer proves nothing.
pub async fn pin(http: &Client, reference: &str, architecture: &str) -> Result<String> {
    let it = parse(reference)?;
    if it.pinned {
        return Ok(it.at(&it.version));
    }

    let (body, digest) = manifest(http, &it, &it.version).await?;
    // An index names one manifest per platform; a lone manifest names none, and
    // its architecture is in the config it points at.
    match select(&body, architecture) {
        Some(chosen) => Ok(it.at(&chosen)),
        None if body.get("manifests").is_some() => bail!(
            "{reference} publishes no linux/{architecture} image (it has {})",
            platforms(&body).join(", ")
        ),
        None => {
            let (config, _) = blob(http, &it, config_digest(&body)?).await?;
            let (os, arch) = (text(&config, "os"), text(&config, "architecture"));
            if (os.as_str(), arch.as_str()) != ("linux", architecture) {
                bail!("{reference} is a {os}/{arch} image; this guest is linux/{architecture}");
            }
            Ok(it.at(&digest))
        }
    }
}

/// The digest of the entry in an index that matches this platform. `None` when
/// the document is not an index, or has no such entry — a buildkit attestation
/// rides in the same list as `unknown/unknown` and must never be selected.
fn select(body: &Value, architecture: &str) -> Option<String> {
    body.get("manifests")?.as_array()?.iter().find_map(|m| {
        let p = m.get("platform")?;
        (text(p, "os") == "linux" && text(p, "architecture") == architecture)
            .then(|| text(m, "digest"))
            .filter(|d| !d.is_empty())
    })
}

/// What an index does offer, for an error message that says so.
fn platforms(body: &Value) -> Vec<String> {
    body.get("manifests")
        .and_then(Value::as_array)
        .map(|ms| {
            ms.iter()
                .filter_map(|m| m.get("platform"))
                .map(|p| format!("{}/{}", text(p, "os"), text(p, "architecture")))
                .collect()
        })
        .unwrap_or_default()
}

fn config_digest(body: &Value) -> Result<String> {
    let digest = text(body.get("config").unwrap_or(&Value::Null), "digest");
    if digest.is_empty() {
        bail!("the registry answered with neither an index nor an image manifest");
    }
    Ok(digest)
}

fn text(v: &Value, key: &str) -> String {
    v.get(key).and_then(Value::as_str).unwrap_or_default().to_string()
}

/// GET a manifest, with the digest the registry itself computed for it.
async fn manifest(http: &Client, it: &Reference, version: &str) -> Result<(Value, String)> {
    let url = it.url("manifests", version);
    let resp = get(http, it, &url, MANIFESTS).await?;
    let digest = resp
        .headers()
        .get("docker-content-digest")
        .and_then(|v| v.to_str().ok())
        .map(str::to_string)
        .unwrap_or_default();
    let body: Value = resp.json().await.context("reading the image manifest")?;
    Ok((body, digest))
}

async fn blob(http: &Client, it: &Reference, digest: String) -> Result<(Value, String)> {
    let url = it.url("blobs", &digest);
    let resp = get(http, it, &url, "application/json").await?;
    let body: Value = resp.json().await.context("reading the image config")?;
    Ok((body, digest))
}

/// One GET, with the token dance a registry asks for: an anonymous request,
/// and on a 401 the same request again carrying a token from the realm the
/// registry named.
async fn get(
    http: &Client,
    it: &Reference,
    url: &str,
    accept: &str,
) -> Result<reqwest::Response> {
    let resp = http
        .get(url)
        .header(ACCEPT, accept)
        .send()
        .await
        .with_context(|| format!("fetching {url}"))?;
    let resp = if resp.status() == StatusCode::UNAUTHORIZED {
        let token = token(http, it, resp.headers()).await?;
        http.get(url)
            .header(ACCEPT, accept)
            .header(AUTHORIZATION, format!("Bearer {token}"))
            .send()
            .await
            .with_context(|| format!("fetching {url}"))?
    } else {
        resp
    };
    if !resp.status().is_success() {
        bail!("{url}: HTTP {}", resp.status());
    }
    Ok(resp)
}

/// A pull token from the realm the 401 named, scoped to this repository alone.
async fn token(http: &Client, it: &Reference, headers: &HeaderMap) -> Result<String> {
    let challenge = headers
        .get(WWW_AUTHENTICATE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default();
    let realm = field(challenge, "realm")
        .ok_or_else(|| anyhow!("{}: refused the request and named no realm", it.registry))?;
    let service = field(challenge, "service").unwrap_or_else(|| it.registry.clone());
    let scope = format!("repository:{}:pull", it.repository);
    let body: Value = http
        .get(&realm)
        .query(&[("service", service.as_str()), ("scope", scope.as_str())])
        .send()
        .await
        .with_context(|| format!("asking {realm} for a pull token"))?
        .json()
        .await
        .context("reading the token response")?;
    // Registries answer with `token`; some also answer with `access_token`.
    let token = text(&body, "token");
    let token = if token.is_empty() {
        text(&body, "access_token")
    } else {
        token
    };
    if token.is_empty() {
        bail!("{realm} issued no pull token for {}", it.repository);
    }
    Ok(token)
}

/// One `key="value"` out of a `Bearer realm="…",service="…"` challenge.
fn field(challenge: &str, key: &str) -> Option<String> {
    let at = challenge.find(&format!("{key}=\""))? + key.len() + 2;
    let rest = &challenge[at..];
    Some(rest[..rest.find('"')?].to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_reference_splits_into_registry_repository_and_version() {
        assert_eq!(
            parse("ghcr.io/hanzoai/cloud:latest").unwrap(),
            Reference {
                registry: "ghcr.io".into(),
                repository: "hanzoai/cloud".into(),
                version: "latest".into(),
                pinned: false,
            }
        );
        // No tag is `latest`, exactly as a registry reads it.
        assert_eq!(parse("ghcr.io/hanzoai/cloud").unwrap().version, "latest");
        // A port in the registry is not a tag.
        let local = parse("localhost:5000/cloud").unwrap();
        assert_eq!((local.registry.as_str(), local.version.as_str()), ("localhost:5000", "latest"));
    }

    #[test]
    fn a_digest_reference_is_already_pinned() {
        let it = parse("ghcr.io/hanzoai/cloud@sha256:abc").unwrap();
        assert!(it.pinned);
        assert_eq!(it.version, "sha256:abc");
        assert_eq!(it.at("sha256:abc"), "ghcr.io/hanzoai/cloud@sha256:abc");
    }

    #[test]
    fn an_unnamed_registry_is_refused() {
        // Inferring docker.io would make the same string mean different images
        // on different machines.
        let err = parse("hanzoai/cloud:latest").unwrap_err();
        assert!(err.to_string().contains("name the registry"), "{err}");
        assert!(parse("cloud").is_err());
    }

    #[test]
    fn an_index_is_selected_by_platform() {
        let index = json!({"manifests": [
            {"digest": "sha256:amd", "platform": {"os": "linux", "architecture": "amd64"}},
            {"digest": "sha256:arm", "platform": {"os": "linux", "architecture": "arm64"}},
            // What buildkit attaches for attestations: never a runnable image.
            {"digest": "sha256:att", "platform": {"os": "unknown", "architecture": "unknown"}},
        ]});
        assert_eq!(select(&index, "arm64").as_deref(), Some("sha256:arm"));
        assert_eq!(select(&index, "amd64").as_deref(), Some("sha256:amd"));
        assert_eq!(select(&index, "riscv64"), None);
        assert_eq!(select(&json!({"config": {"digest": "sha256:c"}}), "amd64"), None);
    }

    #[test]
    fn an_index_reports_what_it_does_have() {
        let index = json!({"manifests": [
            {"digest": "sha256:amd", "platform": {"os": "linux", "architecture": "amd64"}},
        ]});
        assert_eq!(platforms(&index), vec!["linux/amd64"]);
        assert_eq!(config_digest(&json!({"config": {"digest": "sha256:c"}})).unwrap(), "sha256:c");
        assert!(config_digest(&json!({"manifests": []})).is_err());
    }

    #[test]
    fn the_architecture_is_the_one_a_registry_names() {
        assert!(matches!(architecture(), "arm64" | "amd64"));
        if cfg!(target_arch = "aarch64") {
            assert_eq!(architecture(), "arm64");
        }
        if cfg!(target_arch = "x86_64") {
            assert_eq!(architecture(), "amd64");
        }
    }

    #[test]
    fn a_challenge_names_its_realm_and_service() {
        let challenge = r#"Bearer realm="https://ghcr.io/token",service="ghcr.io",scope="x""#;
        assert_eq!(field(challenge, "realm").as_deref(), Some("https://ghcr.io/token"));
        assert_eq!(field(challenge, "service").as_deref(), Some("ghcr.io"));
        assert_eq!(field(challenge, "missing"), None);
    }

    #[test]
    fn a_loopback_registry_is_plain_http() {
        assert_eq!(scheme("localhost:5000"), "http");
        assert_eq!(scheme("127.0.0.1:5000"), "http");
        assert_eq!(scheme("ghcr.io"), "https");
        assert_eq!(scheme("registry.example.com:443"), "https");
    }

    // ---- against a registry ---------------------------------------------------
    //
    // A registry, spoken to over TCP the way a registry speaks: an anonymous
    // request refused with a challenge, a token fetched from the realm it
    // names, and the request again with the token. Hand-rolled (the same
    // approach as `commands::code::testmock`) so no test-only HTTP dependency
    // is pulled in, and reachable because a loopback registry is plain HTTP.

    use std::sync::{Arc, Mutex};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    struct Registry {
        port: u16,
        seen: Arc<Mutex<Vec<String>>>,
    }

    impl Registry {
        /// Serve `routes` (path → body) behind a token challenge.
        async fn start(routes: Vec<(&'static str, &'static str)>) -> Registry {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let port = listener.local_addr().unwrap().port();
            let seen = Arc::new(Mutex::new(Vec::new()));
            let log = seen.clone();
            tokio::spawn(async move {
                loop {
                    let Ok((mut stream, _)) = listener.accept().await else { break };
                    let routes = routes.clone();
                    let log = log.clone();
                    tokio::spawn(async move {
                        let mut buf = [0u8; 4096];
                        let Ok(n) = stream.read(&mut buf).await else { return };
                        let head = String::from_utf8_lossy(&buf[..n]).to_string();
                        let mut lines = head.lines();
                        let path = lines
                            .next()
                            .and_then(|l| l.split_whitespace().nth(1))
                            .unwrap_or_default()
                            .to_string();
                        let authorized = head.to_ascii_lowercase().contains("authorization:");
                        log.lock().unwrap().push(path.clone());

                        let resp = if path.starts_with("/token") {
                            body("200 OK", r#"{"token":"T"}"#, "")
                        } else if !authorized {
                            let realm = format!("http://127.0.0.1:{port}/token");
                            body(
                                "401 Unauthorized",
                                r#"{"errors":[]}"#,
                                &format!(
                                    "WWW-Authenticate: Bearer realm=\"{realm}\",service=\"r\"\r\n"
                                ),
                            )
                        } else {
                            match routes.iter().find(|(p, _)| *p == path) {
                                Some((_, b)) => body(
                                    "200 OK",
                                    b,
                                    "Docker-Content-Digest: sha256:whatever\r\n",
                                ),
                                None => body("404 Not Found", r#"{"errors":[]}"#, ""),
                            }
                        };
                        let _ = stream.write_all(resp.as_bytes()).await;
                        let _ = stream.flush().await;
                    });
                }
            });
            Registry { port, seen }
        }

        fn reference(&self, tag: &str) -> String {
            format!("127.0.0.1:{}/hanzoai/cloud:{tag}", self.port)
        }
    }

    fn body(status: &str, payload: &str, extra: &str) -> String {
        format!(
            "HTTP/1.1 {status}\r\nContent-Type: application/json\r\n{extra}Content-Length: {}\r\nConnection: close\r\n\r\n{payload}",
            payload.len()
        )
    }

    const INDEX: &str = r#"{"manifests":[
        {"digest":"sha256:amd","platform":{"os":"linux","architecture":"amd64"}},
        {"digest":"sha256:arm","platform":{"os":"linux","architecture":"arm64"}},
        {"digest":"sha256:att","platform":{"os":"unknown","architecture":"unknown"}}]}"#;

    /// The whole dance: refused, token from the named realm, the index again
    /// with it, and the digest for THIS architecture — never the attestation.
    #[tokio::test]
    async fn an_index_pins_to_this_architecture_through_the_token_dance() {
        let r = Registry::start(vec![("/v2/hanzoai/cloud/manifests/main", INDEX)]).await;
        let http = Client::new();

        let pinned = pin(&http, &r.reference("main"), "arm64").await.unwrap();
        assert_eq!(pinned, format!("127.0.0.1:{}/hanzoai/cloud@sha256:arm", r.port));

        let seen = r.seen.lock().unwrap().clone();
        assert_eq!(seen[0], "/v2/hanzoai/cloud/manifests/main", "asked anonymously first");
        assert!(seen[1].starts_with("/token?"), "then the realm: {:?}", seen);
        assert!(seen[1].contains("scope=repository"), "scoped to the repository: {}", seen[1]);
        assert_eq!(seen[2], "/v2/hanzoai/cloud/manifests/main", "then again, with it");
    }

    /// An index without this architecture says what it does have, rather than
    /// deploying something the guest cannot execute.
    #[tokio::test]
    async fn an_index_without_this_architecture_is_refused() {
        let r = Registry::start(vec![("/v2/hanzoai/cloud/manifests/main", INDEX)]).await;
        let err = pin(&Client::new(), &r.reference("main"), "riscv64").await.unwrap_err();
        assert!(err.to_string().contains("no linux/riscv64"), "{err}");
        assert!(err.to_string().contains("linux/amd64"), "{err}");
    }

    /// A lone manifest names no platform, so the config it points at is read —
    /// and an image built for another architecture is refused there.
    #[tokio::test]
    async fn a_lone_manifest_is_checked_against_its_config() {
        let r = Registry::start(vec![
            (
                "/v2/hanzoai/cloud/manifests/latest",
                r#"{"config":{"digest":"sha256:cfg"}}"#,
            ),
            (
                "/v2/hanzoai/cloud/blobs/sha256:cfg",
                r#"{"os":"linux","architecture":"amd64"}"#,
            ),
        ])
        .await;
        let http = Client::new();

        let pinned = pin(&http, &r.reference("latest"), "amd64").await.unwrap();
        assert_eq!(
            pinned,
            format!("127.0.0.1:{}/hanzoai/cloud@sha256:whatever", r.port),
            "pinned to the digest the registry computed for the manifest"
        );

        let err = pin(&http, &r.reference("latest"), "arm64").await.unwrap_err();
        assert!(err.to_string().contains("is a linux/amd64 image"), "{err}");
    }

    /// A reference that already names its content asks the registry nothing.
    #[tokio::test]
    async fn a_pinned_reference_never_reaches_the_registry() {
        let r = Registry::start(vec![]).await;
        let reference = format!("127.0.0.1:{}/hanzoai/cloud@sha256:abc", r.port);
        assert_eq!(pin(&Client::new(), &reference, "arm64").await.unwrap(), reference);
        assert!(r.seen.lock().unwrap().is_empty(), "nothing was asked");
    }
}
