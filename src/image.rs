//! Pinning a container image to what it actually is — and speaking the
//! registry's credential dance when the image is not anonymous.
//!
//! A tag is a name someone can move; a digest is the content. `hanzo up`
//! measures the workload it deploys, so the reference that reaches the cluster
//! is resolved to `registry/repository@sha256:…` first. What the manifest
//! names, what containerd verifies on pull, and what the measurement covers
//! are then the same bytes by construction — a tag moved afterwards changes
//! nothing about a cluster already running.
//!
//! Public repositories answer the anonymous token dance; private ones
//! (`oci.hanzo.ai`, `pkg.hanzo.ai`, `forge.hanzo.ai`, `git.hanzo.ai`,
//! `ghcr.io`, or anything in `~/.docker/config.json`) ask for a Basic
//! credential on the token realm — or, on Forgejo, issue an anonymous token
//! first and refuse the manifest until a Basic credential mints a user token.
//! Both shapes are handled here, and the same resolved credential is formatted
//! as a transient `dockerconfigjson` pull secret for the guest kubelet.

use anyhow::{anyhow, bail, Context, Result};
use base64::Engine;
use reqwest::header::{ACCEPT, AUTHORIZATION, WWW_AUTHENTICATE};
use reqwest::{Client, StatusCode};
use serde_json::{json, Value};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::config::Config;
use crate::iam::{paths, provider, store};

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

/// A Basic registry credential (`username:secret`), held in memory only long
/// enough to mint a pull token or write a transient tmpfs pull secret.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Credential {
    pub username: String,
    pub secret: String,
}

impl Credential {
    pub fn new(username: impl Into<String>, secret: impl Into<String>) -> Option<Self> {
        let username = username.into();
        let secret = secret.into().trim().to_string();
        if username.is_empty() || secret.is_empty() {
            return None;
        }
        Some(Self { username, secret })
    }

    /// Standard `base64(username:secret)` used in `Authorization: Basic …` and
    /// Kubernetes `.dockerconfigjson`.
    pub fn basic(&self) -> String {
        base64::engine::general_purpose::STANDARD.encode(format!("{}:{}", self.username, self.secret))
    }
}

/// Format a Kubernetes `kubernetes.io/dockerconfigjson` payload for `registry`.
pub fn dockerconfigjson(registry: &str, cred: &Credential) -> String {
    json!({
        "auths": {
            registry: {
                "username": cred.username,
                "password": cred.secret,
                "auth": cred.basic(),
            }
        }
    })
    .to_string()
}

/// Resolve `reference` to the digest of the image for `architecture`, with no
/// ambient host credentials (used by hermetic callers and tests).
pub async fn pin(http: &Client, reference: &str, architecture: &str) -> Result<String> {
    pin_with(http, reference, architecture, &[]).await
}

/// Resolve `reference` to the digest of the image for `architecture`, using
/// Docker config, environment variables, and the active CLI identity in `cfg`
/// when the registry requires authentication.
pub async fn pin_with_config(
    http: &Client,
    reference: &str,
    architecture: &str,
    cfg: &mut Config,
) -> Result<String> {
    let it = parse(reference)?;
    if it.pinned {
        return Ok(it.at(&it.version));
    }
    let creds = credentials_for(&it.registry, cfg).await;
    pin_with(http, reference, architecture, &creds).await
}

/// Resolve `reference` using the explicit candidate `creds`.
///
/// An already-pinned reference is returned untouched: the caller named the
/// content, and asking a registry to confirm its own answer proves nothing.
pub async fn pin_with(
    http: &Client,
    reference: &str,
    architecture: &str,
    creds: &[Credential],
) -> Result<String> {
    let it = parse(reference)?;
    if it.pinned {
        return Ok(it.at(&it.version));
    }

    let (body, digest) = manifest(http, &it, &it.version, creds).await?;
    // An index names one manifest per platform; a lone manifest names none, and
    // its architecture is in the config it points at.
    match select(&body, architecture) {
        Some(chosen) => Ok(it.at(&chosen)),
        None if body.get("manifests").is_some() => bail!(
            "{reference} publishes no linux/{architecture} image (it has {})",
            platforms(&body).join(", ")
        ),
        None => {
            let (config, _) = blob(http, &it, config_digest(&body)?, creds).await?;
            let (os, arch) = (text(&config, "os"), text(&config, "architecture"));
            if (os.as_str(), arch.as_str()) != ("linux", architecture) {
                bail!("{reference} is a {os}/{arch} image; this guest is linux/{architecture}");
            }
            Ok(it.at(&digest))
        }
    }
}

/// Resolve a `dockerconfigjson` pull secret for `reference` if pulling it from
/// inside the guest cluster requires authentication.
///
/// Public images yield `None` so no secret is planted when none is needed; for
/// private images on `oci.hanzo.ai`, `pkg.hanzo.ai`, `forge.hanzo.ai`,
/// `git.hanzo.ai`, `ghcr.io` or a custom registry, the credential verified
/// against the manifest endpoint (or the first configured credential on a
/// network hiccup) is returned.
pub async fn pull_secret_for(
    http: &Client,
    reference: &str,
    cfg: &mut Config,
) -> Option<String> {
    let it = parse(reference).ok()?;
    let creds = credentials_for(&it.registry, cfg).await;
    if creds.is_empty() {
        return None;
    }
    let url = it.url("manifests", &it.version);
    match get_with(http, &it, &url, MANIFESTS, &creds).await {
        // Anonymous pull succeeded (`used == None`): no secret needed in k3s.
        Ok((_, None)) => None,
        Ok((_, Some(cred))) => Some(dockerconfigjson(&it.registry, &cred)),
        // If probing failed transiently, still plant the best local credential
        // so kubelet's own retry loop has it.
        Err(_) => creds.first().map(|cred| dockerconfigjson(&it.registry, cred)),
    }
}

/// Candidate credentials for `registry`, in precedence order:
/// 1. `$DOCKER_CONFIG/config.json` / `~/.docker/config.json` (`credHelpers`,
///    `credsStore`, and `auths`) — honours `docker login <registry>` everywhere.
/// 2. Registry-specific environment variables (`HANZO_REGISTRY_TOKEN`,
///    `OCI_TOKEN`, `HANZO_API_KEY`, `FORGE_TOKEN`, `GIT_TOKEN`, `GHCR_TOKEN`,
///    `GH_TOKEN`, `GITHUB_TOKEN`).
/// 3. Native CLI credentials:
///    - `oci.hanzo.ai` / `pkg.hanzo.ai`: stored Hanzo `sk-*` key, then the
///      active `hanzo auth login` OIDC access token (accepted for pull-only
///      registry tokens at `hanzo.id/v1/iam/registry/token`).
///    - `forge.hanzo.ai` / `git.hanzo.ai`: active Hanzo identity token.
///    - `ghcr.io`: `gh auth token`.
pub async fn credentials_for(registry: &str, cfg: &mut Config) -> Vec<Credential> {
    let mut out = Vec::new();
    let mut push = |cand: Option<Credential>| {
        if let Some(c) = cand {
            if !out.contains(&c) {
                out.push(c);
            }
        }
    };

    if let Some(path) = docker_config_path() {
        for c in docker_config_credentials(&path, registry) {
            push(Some(c));
        }
    }

    let host = registry_host(registry);
    match host {
        "oci.hanzo.ai" | "pkg.hanzo.ai" => {
            let env_user = env_nonempty("HANZO_REGISTRY_USER")
                .or_else(|| env_nonempty("OCI_USER"))
                .unwrap_or_else(|| "token".into());
            if let Some(tok) =
                env_nonempty("HANZO_REGISTRY_TOKEN").or_else(|| env_nonempty("OCI_TOKEN"))
            {
                push(Credential::new(env_user, tok));
            }
            if let Some(key) = env_nonempty("HANZO_API_KEY").filter(|k| k.starts_with("sk-")) {
                push(Credential::new("token", key));
            }
            if let Ok(Some(key)) = provider::key(provider::Provider::Hanzo) {
                if key.starts_with("sk-") {
                    push(Credential::new("token", key));
                }
            }
            if let Ok(Some((_id, tok))) = store::active_token(cfg, paths::DEFAULT_BRAND).await {
                push(Credential::new("token", tok.access_token));
            }
        }
        "forge.hanzo.ai" | "git.hanzo.ai" => {
            let active = store::active_token(cfg, paths::DEFAULT_BRAND).await.ok().flatten();
            let default_user = env_nonempty("FORGE_USER")
                .or_else(|| env_nonempty("GIT_USER"))
                .or_else(|| active.as_ref().map(|(id, _)| id.name.clone()))
                .unwrap_or_else(|| "token".into());
            for var in ["FORGE_TOKEN", "GIT_TOKEN", "GITEA_TOKEN"] {
                if let Some(tok) = env_nonempty(var) {
                    push(Credential::new(&default_user, &tok));
                    push(Credential::new("token", tok));
                }
            }
            if let Some((id, tok)) = active {
                push(Credential::new(id.name, &tok.access_token));
                push(Credential::new("token", tok.access_token));
            }
        }
        "ghcr.io" => {
            let gh_user = env_nonempty("GHCR_USER")
                .or_else(|| env_nonempty("GITHUB_ACTOR"))
                .unwrap_or_else(|| "x-access-token".into());
            for var in ["GHCR_TOKEN", "GH_TOKEN", "GITHUB_TOKEN"] {
                if let Some(tok) = env_nonempty(var) {
                    push(Credential::new(&gh_user, tok));
                }
            }
            if let Some(tok) = gh_cli_token() {
                push(Credential::new("x-access-token", tok));
            }
        }
        _ => {}
    }

    out
}

fn env_nonempty(name: &str) -> Option<String> {
    std::env::var(name).ok().map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

fn registry_host(registry: &str) -> &str {
    registry.split(':').next().unwrap_or(registry)
}

fn docker_config_path() -> Option<PathBuf> {
    if let Some(dir) = env_nonempty("DOCKER_CONFIG") {
        return Some(PathBuf::from(dir).join("config.json"));
    }
    dirs::home_dir().map(|h| h.join(".docker").join("config.json"))
}

fn docker_config_credentials(path: &Path, registry: &str) -> Vec<Credential> {
    let Ok(raw) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    docker_credentials_from_str(&raw, registry, docker_credential_helper)
}

fn docker_credentials_from_str<F>(raw: &str, registry: &str, mut helper: F) -> Vec<Credential>
where
    F: FnMut(&str, &str) -> Option<Credential>,
{
    let Ok(doc) = serde_json::from_str::<Value>(raw) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let mut push = |cand: Option<Credential>| {
        if let Some(c) = cand {
            if !out.contains(&c) {
                out.push(c);
            }
        }
    };

    if let Some(helpers) = doc.get("credHelpers").and_then(Value::as_object) {
        for (server, name) in helpers {
            if docker_server_matches(server, registry) {
                if let Some(h) = name.as_str().filter(|s| !s.is_empty()) {
                    push(helper(h, server));
                }
            }
        }
    }

    if let Some(auths) = doc.get("auths").and_then(Value::as_object) {
        for (server, entry) in auths {
            if docker_server_matches(server, registry) {
                push(parse_docker_auth_entry(entry));
            }
        }
    }

    if let Some(store_name) = doc.get("credsStore").and_then(Value::as_str).filter(|s| !s.is_empty()) {
        // Query under any matching `auths` key first (e.g. `https://index.docker.io/v1/`),
        // then the bare registry host.
        if let Some(auths) = doc.get("auths").and_then(Value::as_object) {
            for server in auths.keys() {
                if docker_server_matches(server, registry) {
                    push(helper(store_name, server));
                }
            }
        }
        push(helper(store_name, registry));
    }

    out
}

fn docker_server_matches(server: &str, registry: &str) -> bool {
    let normalize = |s: &str| -> String {
        let s = s.trim();
        let s = s
            .strip_prefix("https://")
            .or_else(|| s.strip_prefix("http://"))
            .unwrap_or(s);
        let host = s.split('/').next().unwrap_or(s).to_ascii_lowercase();
        match host.as_str() {
            "index.docker.io" | "registry-1.docker.io" => "docker.io".into(),
            _ => host,
        }
    };
    normalize(server) == normalize(registry)
}

fn parse_docker_auth_entry(entry: &Value) -> Option<Credential> {
    if let Some(b64) = entry.get("auth").and_then(Value::as_str).filter(|s| !s.is_empty()) {
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(b64.trim())
            .or_else(|_| base64::engine::general_purpose::STANDARD_NO_PAD.decode(b64.trim()))
            .ok()?;
        let decoded = String::from_utf8(bytes).ok()?;
        if let Some((u, p)) = decoded.split_once(':') {
            if let Some(c) = Credential::new(u, p) {
                return Some(c);
            }
        }
    }
    let user = entry.get("username").and_then(Value::as_str).unwrap_or("token");
    let secret = entry
        .get("password")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .or_else(|| entry.get("identitytoken").and_then(Value::as_str))?;
    Credential::new(user, secret)
}

fn docker_credential_helper(helper: &str, server: &str) -> Option<Credential> {
    // Only clean helper suffixes (`osxkeychain`, `desktop`, `secretservice`, …)
    // may form a binary name.
    if !helper
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    {
        return None;
    }
    let bin = which::which(format!("docker-credential-{helper}")).ok()?;
    let mut child = Command::new(bin)
        .arg("get")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    if let Some(mut stdin) = child.stdin.take() {
        let _ = writeln!(stdin, "{server}");
    }
    let out = child.wait_with_output().ok()?;
    if !out.status.success() {
        return None;
    }
    let body: Value = serde_json::from_slice(&out.stdout).ok()?;
    let user = body.get("Username").and_then(Value::as_str)?;
    let secret = body.get("Secret").and_then(Value::as_str)?;
    if user == "<token>" {
        Credential::new("token", secret)
    } else {
        Credential::new(user, secret)
    }
}

fn gh_cli_token() -> Option<String> {
    let gh = which::which("gh").ok()?;
    let out = Command::new(gh)
        .args(["auth", "token"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    String::from_utf8(out.stdout)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn auth_hint(registry: &str) -> String {
    match registry_host(registry) {
        "oci.hanzo.ai" | "pkg.hanzo.ai" => {
            format!("sign in with `hanzo auth login` (or `docker login {registry}`)")
        }
        "forge.hanzo.ai" | "git.hanzo.ai" => {
            format!("run `docker login {registry}` or set FORGE_TOKEN (with read:package scope)")
        }
        "ghcr.io" => "run `gh auth login` or `docker login ghcr.io`, or set GH_TOKEN".into(),
        _ => format!("run `docker login {registry}`"),
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
async fn manifest(
    http: &Client,
    it: &Reference,
    version: &str,
    creds: &[Credential],
) -> Result<(Value, String)> {
    let url = it.url("manifests", version);
    let (resp, _) = get_with(http, it, &url, MANIFESTS, creds).await?;
    let digest = resp
        .headers()
        .get("docker-content-digest")
        .and_then(|v| v.to_str().ok())
        .map(str::to_string)
        .unwrap_or_default();
    let body: Value = resp.json().await.context("reading the image manifest")?;
    Ok((body, digest))
}

async fn blob(
    http: &Client,
    it: &Reference,
    digest: String,
    creds: &[Credential],
) -> Result<(Value, String)> {
    let url = it.url("blobs", &digest);
    let (resp, _) = get_with(http, it, &url, "application/json", creds).await?;
    let body: Value = resp.json().await.context("reading the image config")?;
    Ok((body, digest))
}

/// One GET, with the token dance a registry asks for:
/// 1. Try anonymously.
/// 2. On `401 Unauthorized`, inspect `WWW-Authenticate`:
///    - `Basic`: retry directly with each candidate `Credential`.
///    - `Bearer`: ask the named `realm` anonymously first. If the realm refuses
///      (`oci.hanzo.ai`, `pkg.hanzo.ai`, `ghcr.io`) OR issues an anonymous
///      token whose subsequent manifest GET is refused/hidden with `401`/`403`/`404`
///      (`forge.hanzo.ai`, `git.hanzo.ai`), ask the realm again carrying each
///      candidate `Credential` as `Authorization: Basic …`.
async fn get_with(
    http: &Client,
    it: &Reference,
    url: &str,
    accept: &str,
    creds: &[Credential],
) -> Result<(reqwest::Response, Option<Credential>)> {
    let resp = http
        .get(url)
        .header(ACCEPT, accept)
        .send()
        .await
        .with_context(|| format!("fetching {url}"))?;
    if resp.status() != StatusCode::UNAUTHORIZED {
        if !resp.status().is_success() {
            bail!("{url}: HTTP {}", resp.status());
        }
        return Ok((resp, None));
    }

    let challenge = resp
        .headers()
        .get(WWW_AUTHENTICATE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_string();

    if challenge.trim_start().to_ascii_lowercase().starts_with("basic") {
        for cred in creds {
            let retry = http
                .get(url)
                .header(ACCEPT, accept)
                .header(AUTHORIZATION, format!("Basic {}", cred.basic()))
                .send()
                .await
                .with_context(|| format!("fetching {url}"))?;
            if retry.status().is_success() {
                return Ok((retry, Some(cred.clone())));
            }
        }
        bail!(
            "{}: authentication required for {} — {}",
            it.registry,
            it.repository,
            auth_hint(&it.registry)
        );
    }

    let realm = field(&challenge, "realm")
        .ok_or_else(|| anyhow!("{}: refused the request and named no realm", it.registry))?;
    let service = field(&challenge, "service").unwrap_or_else(|| it.registry.clone());
    let scope = format!("repository:{}:pull", it.repository);

    let mut anon_status: Option<StatusCode> = None;
    if let Ok(anon_token) = mint_token(http, &realm, &service, &scope, None).await {
        let retry = http
            .get(url)
            .header(ACCEPT, accept)
            .header(AUTHORIZATION, format!("Bearer {anon_token}"))
            .send()
            .await
            .with_context(|| format!("fetching {url}"))?;
        if retry.status().is_success() {
            return Ok((retry, None));
        }
        anon_status = Some(retry.status());
        // If no credentials exist and the anonymous token yielded a non-auth HTTP
        // error (e.g. a genuine 404 on a public repo), report that status unless
        // it was 401/403.
        if creds.is_empty()
            && retry.status() != StatusCode::UNAUTHORIZED
            && retry.status() != StatusCode::FORBIDDEN
        {
            bail!("{url}: HTTP {}", retry.status());
        }
    }

    for cred in creds {
        let Ok(tok) = mint_token(http, &realm, &service, &scope, Some(cred)).await else {
            continue;
        };
        let retry = http
            .get(url)
            .header(ACCEPT, accept)
            .header(AUTHORIZATION, format!("Bearer {tok}"))
            .send()
            .await
            .with_context(|| format!("fetching {url}"))?;
        if retry.status().is_success() {
            return Ok((retry, Some(cred.clone())));
        }
        anon_status = Some(retry.status());
    }

    if let Some(status) = anon_status.filter(|s| {
        *s != StatusCode::UNAUTHORIZED && *s != StatusCode::FORBIDDEN && !creds.is_empty()
    }) {
        bail!("{url}: HTTP {status}");
    }

    bail!(
        "{realm} issued no pull token for {} — {}",
        it.repository,
        auth_hint(&it.registry)
    )
}

/// Ask `realm` for a pull token, optionally authenticating with `cred`.
async fn mint_token(
    http: &Client,
    realm: &str,
    service: &str,
    scope: &str,
    cred: Option<&Credential>,
) -> Result<String> {
    let mut req = http.get(realm).query(&[("service", service), ("scope", scope)]);
    if let Some(c) = cred {
        req = req.header(AUTHORIZATION, format!("Basic {}", c.basic()));
    }
    let resp = req
        .send()
        .await
        .with_context(|| format!("asking {realm} for a pull token"))?;
    if !resp.status().is_success() {
        bail!("{realm}: HTTP {}", resp.status());
    }
    let body: Value = resp.json().await.context("reading the token response")?;
    // Registries answer with `token`; some also answer with `access_token`.
    let token = text(&body, "token");
    let token = if token.is_empty() {
        text(&body, "access_token")
    } else {
        token
    };
    if token.is_empty() {
        bail!("{realm} issued no pull token");
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
    // approach as `hanzo_gen`'s testmock) so no test-only HTTP dependency
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

    #[test]
    fn docker_config_parses_auths_helpers_and_normalized_hosts() {
        let auth = base64::engine::general_purpose::STANDARD.encode("token:secret-jwt");
        let cfg = json!({
            "auths": {
                "https://oci.hanzo.ai/v2/": { "auth": auth },
                "pkg.hanzo.ai": { "username": "u", "password": "p" },
            },
            "credHelpers": {
                "forge.hanzo.ai": "osxkeychain"
            },
            "credsStore": "desktop"
        })
        .to_string();

        let oci = docker_credentials_from_str(&cfg, "oci.hanzo.ai", |_, _| None);
        assert_eq!(oci, vec![Credential { username: "token".into(), secret: "secret-jwt".into() }]);

        let pkg = docker_credentials_from_str(&cfg, "pkg.hanzo.ai", |_, _| None);
        assert_eq!(pkg, vec![Credential { username: "u".into(), secret: "p".into() }]);

        let forge = docker_credentials_from_str(&cfg, "forge.hanzo.ai", |h, s| {
            Credential::new(format!("{h}:{s}"), "forge-pat")
        });
        assert_eq!(
            forge[0],
            Credential { username: "osxkeychain:forge.hanzo.ai".into(), secret: "forge-pat".into() }
        );
    }

    /// Private `oci.hanzo.ai` / `pkg.hanzo.ai` / `ghcr.io` shape: anonymous
    /// `/token` returns `401`, and retrying `/token` with `Authorization: Basic`
    /// mints the pull token.
    #[tokio::test]
    async fn private_realm_refusing_anonymous_token_retries_with_basic_credential() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let expected = format!(
            "authorization: basic {}",
            Credential::new("token", "oidc-jwt").unwrap().basic().to_ascii_lowercase()
        );
        tokio::spawn(async move {
            loop {
                let Ok((mut stream, _)) = listener.accept().await else { break };
                let expected = expected.clone();
                tokio::spawn(async move {
                    let mut buf = [0u8; 4096];
                    let Ok(n) = stream.read(&mut buf).await else { return };
                    let head = String::from_utf8_lossy(&buf[..n]).to_string();
                    let lower = head.to_ascii_lowercase();
                    let path = head
                        .lines()
                        .next()
                        .and_then(|l| l.split_whitespace().nth(1))
                        .unwrap_or_default();
                    let resp = if path.starts_with("/token") {
                        if lower.contains(&expected) {
                            body("200 OK", r#"{"token":"SIGNED"}"#, "")
                        } else {
                            body("401 Unauthorized", r#"{"error":"unauthorized"}"#, "")
                        }
                    } else if lower.contains("authorization: bearer signed") {
                        body("200 OK", INDEX, "Docker-Content-Digest: sha256:idx\r\n")
                    } else {
                        let realm = format!("http://127.0.0.1:{port}/token");
                        body(
                            "401 Unauthorized",
                            r#"{"errors":[]}"#,
                            &format!("WWW-Authenticate: Bearer realm=\"{realm}\",service=\"oci.hanzo.ai\"\r\n"),
                        )
                    };
                    let _ = stream.write_all(resp.as_bytes()).await;
                    let _ = stream.flush().await;
                });
            }
        });

        let reference = format!("127.0.0.1:{port}/hanzoai/cloud:main");
        let http = Client::new();
        assert!(pin_with(&http, &reference, "arm64", &[]).await.is_err());

        let creds = vec![
            Credential::new("token", "wrong").unwrap(),
            Credential::new("token", "oidc-jwt").unwrap(),
        ];
        let pinned = pin_with(&http, &reference, "arm64", &creds).await.unwrap();
        assert_eq!(pinned, format!("127.0.0.1:{port}/hanzoai/cloud@sha256:arm"));
    }

    /// Private Forgejo (`forge.hanzo.ai` / `git.hanzo.ai`) shape: `/v2/token`
    /// returns `200 OK` with an anonymous JWT (`uid=-1`), and the manifest
    /// returns `404` until `/v2/token` is re-asked with Basic credentials.
    #[tokio::test]
    async fn forgejo_anonymous_token_404_retries_with_authenticated_token() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            loop {
                let Ok((mut stream, _)) = listener.accept().await else { break };
                tokio::spawn(async move {
                    let mut buf = [0u8; 4096];
                    let Ok(n) = stream.read(&mut buf).await else { return };
                    let head = String::from_utf8_lossy(&buf[..n]).to_string();
                    let lower = head.to_ascii_lowercase();
                    let path = head
                        .lines()
                        .next()
                        .and_then(|l| l.split_whitespace().nth(1))
                        .unwrap_or_default();
                    let resp = if path.starts_with("/v2/token") {
                        if lower.contains("authorization: basic ") {
                            body("200 OK", r#"{"token":"USER_JWT"}"#, "")
                        } else {
                            body("200 OK", r#"{"token":"ANON_JWT"}"#, "")
                        }
                    } else if lower.contains("authorization: bearer user_jwt") {
                        body("200 OK", INDEX, "Docker-Content-Digest: sha256:idx\r\n")
                    } else if lower.contains("authorization: bearer anon_jwt") {
                        body("404 Not Found", r#"{"errors":[{"code":"MANIFEST_UNKNOWN"}]}"#, "")
                    } else {
                        let realm = format!("http://127.0.0.1:{port}/v2/token");
                        body(
                            "401 Unauthorized",
                            r#"{"errors":[]}"#,
                            &format!("WWW-Authenticate: Bearer realm=\"{realm}\",service=\"container_registry\"\r\n"),
                        )
                    };
                    let _ = stream.write_all(resp.as_bytes()).await;
                    let _ = stream.flush().await;
                });
            }
        });

        let reference = format!("127.0.0.1:{port}/hanzoai/cloud:main");
        let creds = vec![Credential::new("a", "forge-pat").unwrap()];
        let pinned = pin_with(&Client::new(), &reference, "arm64", &creds).await.unwrap();
        assert_eq!(pinned, format!("127.0.0.1:{port}/hanzoai/cloud@sha256:arm"));
    }
}
