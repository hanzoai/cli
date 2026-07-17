//! `hanzo api <METHOD> <PATH>` — the universal authenticated call into cloud.
//!
//! Cloud serves ~1000 `/v1` operations across ~130 products; the CLI gives the
//! common ones first-class verbs (`kms`, `billing`, `wallet`, …), and THIS gives
//! every other one, unchanged, through the SAME seam. It is the `gh api` /
//! `kubectl --raw` escape hatch: any capability the backend exposes is one
//! command away, so "the CLI supports all of cloud" is true by construction, not
//! by hand-writing 1000 verbs.
//!
//! The trust boundary is the whole point. The ONLY user input is the method, the
//! path and the body. The ORIGIN comes from `network::active` and the BEARER from
//! `iam::store::active_token` — never from an argument, never from a fetched
//! spec. So a call can only ever reach YOUR active cloud with YOUR active
//! identity's token: a hostile path can at worst make you call your own server
//! with wrong arguments (a 4xx), never exfiltrate the token or redirect it. The
//! org is the gateway's to derive from the JWT `owner`; where a route names the
//! org in its path, that segment is the caller's own and the server re-checks it.

use anyhow::{anyhow, bail, Context, Result};
use reqwest::{Client, Method, StatusCode};
use serde_json::Value;
use std::io::Read;

use crate::commands::network;
use crate::http;
use crate::iam::{paths, store};
use crate::config::Config;

/// `hanzo api <METHOD> <PATH> [--data <json|->] [--query k=v]…`.
pub async fn run(
    cfg: &mut Config,
    method: String,
    path: String,
    data: Option<String>,
    query: Vec<String>,
    raw: bool,
) -> Result<()> {
    let method = parse_method(&method)?;
    let path = normalize_path(&path)?;
    let body = read_body(data, &method)?;
    call(cfg, method, path, body, query, raw).await
}

/// The authenticated dispatch backbone — THE seam `hanzo api` AND the generated
/// first-class product tree share, so the trust boundary lives in exactly one
/// place. Resolves WHERE (the active network's origin) and WHO (the active
/// identity's bearer) through the one identity seam, sends the request, prints
/// the `data`, and explains a 403 in identity terms. The org is NEVER a header:
/// where a route names it in the PATH, the caller has already addressed it (its
/// own `owner`), and the server re-checks it against the JWT it verifies.
///
/// `path` is the FINAL `/v1/…` path — literal for `hanzo api`, template-filled
/// for the generated tree. It is never derived from a fetched spec at runtime:
/// the origin comes from `network`, the bearer from `store`, and neither can be
/// smuggled in through the path.
pub(crate) async fn call(
    cfg: &mut Config,
    method: Method,
    path: String,
    body: Option<Value>,
    query: Vec<String>,
    raw: bool,
) -> Result<()> {
    let origin = network::active(cfg).api;
    let origin = origin.trim_end_matches('/');
    let (id, tok) = store::active_token(cfg, paths::DEFAULT_BRAND)?
        .ok_or_else(|| anyhow!("not signed in — run `hanzo login`"))?;
    // The identity we would suggest switching to on a 403 (SuperAdmin gate) — the
    // very identity we authenticate as, so the hint can never name someone else.
    let held = store::list(cfg, paths::DEFAULT_BRAND);
    let hint = store::refusal_hint(&id, &held);

    let url = build_url(origin, &path, &query)?;
    let http_client = Client::new();
    let (status, resp) =
        http::send(&http_client, method, &url, &tok.access_token, body.as_ref()).await?;

    if status.is_success() {
        print_body(&resp, raw);
        return Ok(());
    }

    // Non-2xx: surface the SERVER's own body, and — only on a 403 the server
    // itself returned — the identity-switch hint. The refusal is always the
    // server's, never a client-side guess; we read our identity only to explain
    // it, after the fact.
    let shown = match &resp {
        Value::Null => String::new(),
        Value::String(s) => s.trim().to_string(),
        v => v.to_string(),
    };
    if status == StatusCode::FORBIDDEN {
        if let Some(hint) = hint {
            bail!("{path} -> {status}: {shown}{hint}");
        }
    }
    bail!("{path} -> {status}: {shown}");
}

/// Accept the common HTTP methods, case-insensitively; default is GET.
pub(crate) fn parse_method(m: &str) -> Result<Method> {
    match m.to_ascii_uppercase().as_str() {
        "GET" => Ok(Method::GET),
        "POST" => Ok(Method::POST),
        "PUT" => Ok(Method::PUT),
        "PATCH" => Ok(Method::PATCH),
        "DELETE" => Ok(Method::DELETE),
        "HEAD" => Ok(Method::HEAD),
        other => bail!("unsupported method {other:?} (GET|POST|PUT|PATCH|DELETE|HEAD)"),
    }
}

/// Every Hanzo API path is `/v1/…`. Accept a leading-slash-optional path, reject
/// the `/api/` prefix (the house rule: `api.*` hosts, `/v1/` paths, never both),
/// and reject an absolute URL so the ORIGIN can only ever come from the active
/// network — a path can never smuggle in a different host.
fn normalize_path(path: &str) -> Result<String> {
    if path.contains("://") {
        bail!("path must be a /v1 path, not a full URL — the host comes from `hanzo network`");
    }
    let p = if let Some(stripped) = path.strip_prefix('/') {
        stripped.to_string()
    } else {
        path.to_string()
    };
    let p = format!("/{p}");
    if p.starts_with("/api/") {
        bail!("no `/api/` prefix — Hanzo paths are `/v1/…` (e.g. /v1/kms/orgs/<org>/secrets)");
    }
    if !p.starts_with("/v1/") && p != "/v1" {
        bail!("path must start with /v1/ (got {p})");
    }
    Ok(p)
}

/// `--data` is JSON. `-` reads stdin so a secret in a body never lands in argv,
/// `ps` or shell history — the same rule as `kms set` and `login --token -`. A
/// body on a GET/HEAD is a mistake worth naming, not silently sending.
pub(crate) fn read_body(data: Option<String>, method: &Method) -> Result<Option<Value>> {
    let Some(d) = data else { return Ok(None) };
    if matches!(*method, Method::GET | Method::HEAD) {
        bail!("--data is not sent on a {method} — did you mean POST/PUT/PATCH?");
    }
    let raw = if d == "-" {
        let mut s = String::new();
        std::io::stdin()
            .read_to_string(&mut s)
            .context("reading --data from stdin")?;
        s
    } else {
        d
    };
    let value: Value = serde_json::from_str(raw.trim())
        .context("--data must be valid JSON (use `-` to read a JSON body from stdin)")?;
    Ok(Some(value))
}

/// Build the absolute URL, appending any `--query k=v` pairs. Split out so the
/// join is unit-testable without a network. Values are percent-encoded by
/// `reqwest::Url`, so a `k=a b&c` cannot forge extra parameters.
fn build_url(origin: &str, path: &str, query: &[String]) -> Result<String> {
    let mut url = reqwest::Url::parse(&format!("{origin}{path}"))
        .with_context(|| format!("building URL {origin}{path}"))?;
    {
        let mut pairs = url.query_pairs_mut();
        for q in query {
            let (k, v) = q
                .split_once('=')
                .ok_or_else(|| anyhow!("--query must be k=v (got {q:?})"))?;
            pairs.append_pair(k, v);
        }
    }
    Ok(url.to_string())
}

/// Print the response. The cloud `/v1` envelope is `{status,msg,data}`; by
/// default we surface `data` (what a caller wants to pipe), and `--raw` prints
/// the whole envelope. A non-object or an enveloped error prints as-is.
fn print_body(resp: &Value, raw: bool) {
    let shown = if raw {
        resp
    } else {
        resp.get("data").unwrap_or(resp)
    };
    match shown {
        Value::Null => {}
        Value::String(s) => println!("{s}"),
        v => println!("{}", serde_json::to_string_pretty(v).unwrap_or_else(|_| v.to_string())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn method_parses_case_insensitively_and_rejects_junk() {
        assert_eq!(parse_method("get").unwrap(), Method::GET);
        assert_eq!(parse_method("Post").unwrap(), Method::POST);
        assert_eq!(parse_method("DELETE").unwrap(), Method::DELETE);
        assert!(parse_method("CONNECT").is_err());
        assert!(parse_method("").is_err());
    }

    #[test]
    fn path_is_normalized_to_a_v1_path() {
        assert_eq!(normalize_path("/v1/kms/orgs/hanzo/secrets").unwrap(), "/v1/kms/orgs/hanzo/secrets");
        // Leading slash optional.
        assert_eq!(normalize_path("v1/billing/balance").unwrap(), "/v1/billing/balance");
    }

    /// The house rule and the trust boundary, enforced at the value edge: no
    /// `/api/` prefix, and no absolute URL (the host is `network`'s alone).
    #[test]
    fn path_rejects_api_prefix_and_absolute_urls() {
        assert!(normalize_path("/api/v1/kms").is_err());
        assert!(normalize_path("api/v1/kms").is_err());
        assert!(normalize_path("https://evil.example/v1/kms").is_err());
        assert!(normalize_path("http://169.254.169.254/v1/x").is_err());
        // A non-/v1 path is refused rather than silently sent.
        assert!(normalize_path("/health").is_err());
        assert!(normalize_path("/v2/kms").is_err());
    }

    #[test]
    fn a_body_on_a_get_is_a_named_error_not_silently_sent() {
        assert!(read_body(Some("{}".into()), &Method::GET).is_err());
        assert!(read_body(Some("{}".into()), &Method::HEAD).is_err());
        assert!(read_body(Some(r#"{"a":1}"#.into()), &Method::POST).is_ok());
        assert!(read_body(None, &Method::GET).unwrap().is_none());
    }

    #[test]
    fn body_must_be_valid_json() {
        assert!(read_body(Some("not json".into()), &Method::POST).is_err());
        assert_eq!(
            read_body(Some(r#"{"amount":100}"#.into()), &Method::POST).unwrap().unwrap()["amount"],
            100
        );
    }

    #[test]
    fn query_pairs_are_appended_and_encoded() {
        let u = build_url("https://api.hanzo.ai", "/v1/kms/orgs/hanzo/secrets", &["env=prod".into()]).unwrap();
        assert!(u.starts_with("https://api.hanzo.ai/v1/kms/orgs/hanzo/secrets?"));
        assert!(u.contains("env=prod"));
        // A value that looks like extra params is encoded, not injected.
        let u = build_url("https://api.hanzo.ai", "/v1/x", &["q=a b&c=d".into()]).unwrap();
        assert!(u.contains("q=a+b%26c%3Dd"), "{u}");
        // Malformed --query is rejected.
        assert!(build_url("https://api.hanzo.ai", "/v1/x", &["noeq".into()]).is_err());
    }
}
