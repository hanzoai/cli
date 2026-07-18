//! `hanzo connector` — connect an external provider account to your org so Hanzo
//! can act on it (manage Cloudflare DNS / zones / Pages, …).
//!
//! A connector is per-org DELEGATED AUTHORITY, and its CREDENTIAL lives in Hanzo
//! KMS server-side — never in the CLI, never in argv, never in a log. This is the
//! CLI half of cloud's `/v1/integrations` connector plane; the server verifies
//! the credential and seals it into the org's KMS namespace.
//!
//! ## A credential has exactly one way in
//! `add` reads the token from STDIN (`--token -`, or a pipe) — a literal
//! `--token <value>` is REFUSED, the SAME law as `hanzo kms set` and
//! `hanzo login --token -` (shared in `iam::secret`), so no argv, `ps`, shell
//! history or CI log can ever hold it. It rides the request BODY over TLS (never
//! a URL, never a log) with the identity bearer, and the CLI forgets it the
//! moment the call returns. `list`/`verify`/`rm` never carry a credential at all.
//!
//! ## The org is derived, never asserted
//! `http` sends the bearer AND NOTHING ELSE — no org header. Cloud derives the
//! tenant from the JWT `owner` it verifies, so `hanzo switch` moves connectors for
//! free, exactly as it moves secrets and billing, with no `--org` and no new
//! machinery. Connecting/disconnecting is an ORG-ADMIN action, enforced by the
//! server against the token it verifies — the CLI states the intent, the gateway
//! decides.

use anyhow::{anyhow, bail, Result};
use colored::*;
use reqwest::{Client, Method};
use serde_json::{json, Value};

use crate::commands::network;
use crate::config::Config;
use crate::http::send_json;
use crate::iam::secret;
use crate::iam::{paths, store};

/// The providers `hanzo connector` speaks to. A closed set so an unknown
/// `--provider` fails HERE with the supported list, not as an opaque server 404.
/// The server stays the authority; this is a helpful front door. (OAuth providers
/// — Cloudflare OAuth, GitHub, … — join this list as they are wired.)
const PROVIDERS: &[&str] = &["cloudflare"];

fn check_provider(p: &str) -> Result<()> {
    if PROVIDERS.contains(&p) {
        return Ok(());
    }
    bail!("unknown provider {p:?} — supported: {}", PROVIDERS.join(", "))
}

/// One authenticated conversation with the connector plane: WHERE (api) and WHO
/// (the bearer). The org is deliberately NOT held — cloud derives it from the
/// token's `owner`, so there is nothing here to drift or forge.
struct Session {
    api: String,
    token: String,
    http: Client,
}

impl Session {
    fn new(api: String, token: String) -> Self {
        Self { api, token, http: Client::new() }
    }

    fn base(&self) -> String {
        format!("{}/v1/integrations", self.api.trim_end_matches('/'))
    }

    async fn call(&self, method: Method, url: &str, body: Option<&Value>) -> Result<Value> {
        send_json(&self.http, method, url, &self.token, body).await
    }
}

/// Resolve the ACTIVE identity + network into a session. The org is a projection
/// of the token (server-side), so the session never carries one. Not signed in
/// means NOT SIGNED IN — it never falls through to another identity.
fn open(cfg: &mut Config) -> Result<Session> {
    let api = network::active(cfg).api;
    let (_, tok) = store::active_token(cfg, paths::DEFAULT_BRAND)?
        .ok_or_else(|| anyhow!("not signed in — run `hanzo login` first"))?;
    Ok(Session::new(api, tok.access_token))
}

/// Prompt for the token with a hidden (non-echoing) input — the interactive path
/// (a terminal, no `--token`). The stdin/refuse paths are the shared law.
fn prompt_token(provider: &str) -> Result<String> {
    use dialoguer::{theme::SimpleTheme, Password};
    let tok = Password::with_theme(&SimpleTheme)
        .with_prompt(format!("Paste your {provider} API token"))
        .interact()
        .map_err(|e| anyhow!("reading token: {e}"))?;
    let tok = tok.trim().to_string();
    if tok.is_empty() {
        bail!("no token entered");
    }
    Ok(tok)
}

// ---- the four verbs ---------------------------------------------------------

/// `hanzo connector add --provider cloudflare [--account-id ID] --token -`
///
/// The token comes from STDIN only (`--token -` or a pipe); a literal is refused
/// BEFORE any network I/O. It is POSTed in the request body; the server verifies
/// it against the provider and, only on success, seals it into the org's KMS.
pub async fn add(
    cfg: &mut Config,
    provider: String,
    account_id: Option<String>,
    token: Option<String>,
) -> Result<()> {
    check_provider(&provider)?;
    // The credential: stdin (`--token -`/pipe) or a hidden prompt — never argv.
    let token = secret::resolve_token(token, || prompt_token(&provider))?;
    let s = open(cfg)?;
    let body = connect_body(&token, account_id.as_deref());
    let url = format!("{}/{}/connect", s.base(), provider);
    let resp = s.call(Method::POST, &url, Some(&body)).await?;
    // The confirmation names the account/scopes — never the token.
    let label = resp
        .get("account")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .or_else(|| resp.get("externalId").and_then(Value::as_str))
        .unwrap_or("");
    let scopes = resp
        .get("scopes")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(", "))
        .unwrap_or_default();
    println!(
        "{} connected {}{}",
        "✓".green(),
        provider.cyan().bold(),
        if label.is_empty() { String::new() } else { format!(" ({label})") }
    );
    if !scopes.is_empty() {
        println!("{}", format!("  scopes: {scopes}").dimmed());
    }
    Ok(())
}

/// Build the connect body. `accountId` is a NON-secret hint (safe on argv), sent
/// only when present so the server's discovery stands otherwise. The token is the
/// only secret and it rides the body, never a URL. Pure — so the wire shape is
/// unit-testable without a network.
fn connect_body(token: &str, account_id: Option<&str>) -> Value {
    let mut body = json!({ "token": token });
    if let Some(acct) = account_id.map(str::trim).filter(|a| !a.is_empty()) {
        body["accountId"] = json!(acct);
    }
    body
}

/// List your org's connectors and their status — never the credential. Only the
/// credential connectors (`PROVIDERS`) are shown; OAuth social/chat integrations
/// live under their own surface.
pub async fn list(cfg: &mut Config) -> Result<()> {
    let s = open(cfg)?;
    let resp = s.call(Method::GET, &s.base(), None).await?;
    let providers = resp.get("providers").and_then(Value::as_array).cloned().unwrap_or_default();
    let mut shown = 0;
    for p in &providers {
        let id = p.get("id").and_then(Value::as_str).unwrap_or("");
        if !PROVIDERS.contains(&id) {
            continue;
        }
        shown += 1;
        let connected = p.get("connected").and_then(Value::as_bool).unwrap_or(false);
        let status = if connected { "connected".green() } else { "not connected".dimmed() };
        let account = p
            .get("connection")
            .and_then(|c| c.get("account"))
            .and_then(Value::as_str)
            .unwrap_or("");
        println!(
            "{:<14} {}{}",
            id.cyan().bold(),
            status,
            if account.is_empty() { String::new() } else { format!("  ({account})") }
        );
    }
    if shown == 0 {
        eprintln!(
            "{}",
            "no connectors — add one, e.g. `… | hanzo connector add --provider cloudflare --token -`".dimmed()
        );
    }
    Ok(())
}

/// Re-verify a connected credential against the provider, live. The server reads
/// the stored credential from KMS and checks it; the value is never returned.
pub async fn verify(cfg: &mut Config, provider: String) -> Result<()> {
    check_provider(&provider)?;
    let s = open(cfg)?;
    let url = format!("{}/{}/verify", s.base(), provider);
    let resp = s.call(Method::POST, &url, None).await?;
    if resp.get("active").and_then(Value::as_bool).unwrap_or(false) {
        println!("{} {} credential is active", "✓".green(), provider.cyan().bold());
    } else {
        let reason = resp.get("reason").and_then(Value::as_str).unwrap_or("not active");
        println!("{} {} credential is NOT active ({reason})", "✗".red(), provider.cyan().bold());
    }
    Ok(())
}

/// Disconnect a provider: the server deletes its KMS credential and forgets the
/// connection. Idempotent.
pub async fn rm(cfg: &mut Config, provider: String) -> Result<()> {
    check_provider(&provider)?;
    let s = open(cfg)?;
    let url = format!("{}/{}/disconnect", s.base(), provider);
    s.call(Method::POST, &url, None).await?;
    println!("{} removed {} connector", "✓".green(), provider.cyan().bold());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::code::testmock::MockCloud;
    use crate::iam::identity::testjwt::jwt;

    fn session(api: &str) -> (Session, String) {
        let tok = jwt("hanzo", "z");
        (Session::new(api.to_string(), tok.clone()), tok)
    }

    #[test]
    fn an_unknown_provider_is_refused_with_the_supported_list() {
        assert!(check_provider("cloudflare").is_ok());
        let e = check_provider("bogus").unwrap_err().to_string();
        assert!(e.contains("cloudflare"), "the error must name the supported providers: {e}");
    }

    /// The connect body carries the token and (optionally) the non-secret account
    /// hint — and nothing else. The account hint is omitted when blank.
    #[test]
    fn the_connect_body_carries_the_token_and_optional_account_hint() {
        let b = connect_body("cf-secret", Some("acc-1"));
        assert_eq!(b["token"], "cf-secret");
        assert_eq!(b["accountId"], "acc-1");
        let b2 = connect_body("cf-secret", None);
        assert_eq!(b2["token"], "cf-secret");
        assert!(b2.get("accountId").is_none(), "a blank account hint is omitted");
        let b3 = connect_body("cf-secret", Some("  "));
        assert!(b3.get("accountId").is_none(), "a whitespace account hint is omitted");
    }

    /// THE CLI security property: the token rides the BODY over TLS — never the
    /// URL, never a query, never an org header. Only the bearer goes out of band.
    #[tokio::test]
    async fn add_sends_the_token_in_the_body_never_the_url_or_an_org_header() {
        let mock = MockCloud::start().await;
        let (s, tok) = session(&mock.base_url());
        let url = format!("{}/cloudflare/connect", s.base());
        let cred = "cf-scoped-token-SECRET-deadbeef";
        s.call(Method::POST, &url, Some(&connect_body(cred, Some("acc-1")))).await.unwrap();

        let reqs = mock.requests();
        assert_eq!(reqs.len(), 1);
        let r = &reqs[0];
        assert_eq!(r.path, "/v1/integrations/cloudflare/connect");
        assert!(!r.path.contains(cred), "the token must NEVER appear in the URL");
        assert_eq!(r.header("authorization"), Some(format!("Bearer {tok}")));
        for (k, _) in &r.headers {
            assert!(
                !k.to_ascii_lowercase().contains("org"),
                "the CLI must never send an org header, found {k}"
            );
        }
        assert_eq!(r.json()["token"], cred);
        assert_eq!(r.json()["accountId"], "acc-1");
    }

    /// list → GET /v1/integrations; verify → POST …/verify; rm → POST …/disconnect.
    #[tokio::test]
    async fn the_verbs_map_to_the_connector_endpoints() {
        let mock = MockCloud::start().await;
        let (s, _) = session(&mock.base_url());
        s.call(Method::GET, &s.base(), None).await.unwrap();
        s.call(Method::POST, &format!("{}/cloudflare/verify", s.base()), None).await.unwrap();
        s.call(Method::POST, &format!("{}/cloudflare/disconnect", s.base()), None).await.unwrap();

        let reqs = mock.requests();
        assert_eq!(reqs[0].path, "/v1/integrations");
        assert_eq!(reqs[1].path, "/v1/integrations/cloudflare/verify");
        assert_eq!(reqs[2].path, "/v1/integrations/cloudflare/disconnect");
        // Nothing asserts an org out of band, on any verb.
        for r in &reqs {
            for (k, _) in &r.headers {
                assert!(!k.to_ascii_lowercase().contains("org"), "no org header: {k}");
            }
        }
    }

    /// A non-2xx is an error, never a silent success — a connect that reports OK
    /// while the credential never landed is the worst outcome for a secrets tool.
    #[tokio::test]
    async fn a_refused_connect_is_an_error_not_a_silent_success() {
        let mock = MockCloud::start_status(403).await;
        let (s, _) = session(&mock.base_url());
        let r = s
            .call(Method::POST, &format!("{}/cloudflare/connect", s.base()), Some(&connect_body("x", None)))
            .await;
        assert!(r.is_err(), "a 403 must surface");
    }
}
