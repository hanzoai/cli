//! The ONE HTTPS call into cloud.
//!
//! The session client (`/v1/agents/sessions`), the run-target client
//! (`/v1/agents/targets`) and the secret store (`/v1/kms/...`) all speak through
//! this single seam, so the wire contract — bearer auth, optional JSON body, and
//! "a non-2xx response is an error, never a silent success" — lives in exactly
//! one place. It is transport ONLY: it knows nothing of the plane it is calling,
//! which is why three unrelated concerns can share it without braiding.
//!
//! It sends the BEARER AND NOTHING ELSE — no org header, ever. Cloud decides the
//! tenant from the JWT `owner` claim it verifies, so a caller cannot assert (or
//! forge) one from here. Where a route names the org in its PATH (KMS does), that
//! segment is the caller's OWN `owner` and the server re-checks it against the
//! same claim — the value is addressed, never trusted.

use anyhow::{bail, Context, Result};
use reqwest::{Client, Method, StatusCode};
use serde::Serialize;
use serde_json::Value;

/// Send one request with the hanzo.id bearer and an optional JSON body, returning
/// the raw `(status, parsed-body)`. The status is HANDED BACK, never flattened:
/// some planes explain a specific status (billing turns a 403 into a switch-
/// identity hint; the product tree prints the server's body verbatim), so the
/// seam must not decide for them what a non-2xx means. The body is `Value::Null`
/// an empty response, the parsed JSON when it parses, and a JSON string of the
/// raw text when it does not (an error page is still the caller's to show).
pub(crate) async fn send<B: Serialize>(
    http: &Client,
    method: Method,
    url: &str,
    token: &str,
    body: Option<&B>,
) -> Result<(StatusCode, Value)> {
    let mut req = http.request(method, url).bearer_auth(token);
    if let Some(b) = body {
        req = req.json(b);
    }
    let resp = req.send().await.with_context(|| format!("request {url}"))?;
    let status = resp.status();
    let text = resp.text().await.unwrap_or_default();
    if text.is_empty() {
        return Ok((status, Value::Null));
    }
    let value = serde_json::from_str(&text).unwrap_or(Value::String(text));
    Ok((status, value))
}

/// Send one request, returning the parsed 2xx body (or `Value::Null` for an empty
/// 2xx). A non-2xx status is an error carrying the URL + status + trimmed body —
/// never a silent success. The "fail on non-2xx" wrapper over [`send`], for the
/// callers that have nothing to add to a bad status.
pub(crate) async fn send_json<B: Serialize>(
    http: &Client,
    method: Method,
    url: &str,
    token: &str,
    body: Option<&B>,
) -> Result<Value> {
    let (status, body) = send(http, method, url, token, body).await?;
    if !status.is_success() {
        let shown = match &body {
            Value::Null => String::new(),
            Value::String(s) => s.trim().to_string(),
            v => v.to_string(),
        };
        bail!("{} -> {}: {}", url, status, shown);
    }
    Ok(body)
}
