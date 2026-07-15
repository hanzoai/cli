//! The ONE HTTPS call into the cloud agents control plane.
//!
//! Both the session client (`/v1/agents/sessions`) and the run-target client
//! (`/v1/agents/targets`) speak through this single seam, so the wire contract —
//! bearer auth, optional JSON body, and "a non-2xx response is an error, never a
//! silent success" — lives in exactly one place.
//!
//! The org is derived SERVER-SIDE from the JWT `owner` claim: this transport sends
//! only the bearer, so no caller can send (or forge) an org and cross-tenant
//! attribution is refused at the gateway, not trusted from here.

use anyhow::{bail, Context, Result};
use reqwest::{Client, Method};
use serde::Serialize;
use serde_json::Value;

/// Send one request with the hanzo.id bearer and an optional JSON body, returning
/// the parsed response (or `Value::Null` for an empty 2xx). A non-2xx status is an
/// error carrying the URL + status + trimmed body — never a silent success.
pub(crate) async fn send_json<B: Serialize>(
    http: &Client,
    method: Method,
    url: &str,
    token: &str,
    body: Option<&B>,
) -> Result<Value> {
    let mut req = http.request(method, url).bearer_auth(token);
    if let Some(b) = body {
        req = req.json(b);
    }
    let resp = req.send().await.with_context(|| format!("request {url}"))?;
    let status = resp.status();
    let text = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        bail!("{} -> {}: {}", url, status, text.trim());
    }
    if text.is_empty() {
        return Ok(Value::Null);
    }
    serde_json::from_str(&text).with_context(|| format!("parsing {url} response"))
}
