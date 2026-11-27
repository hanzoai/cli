//! Shared `hanzod` `/v1/lifecycle/*` ZAP/HTTP wire client with automatic
//! daemon bootstrap and seamless fallback when `hanzod` is absent or older.

use anyhow::{bail, Result};
#[cfg(not(unix))]
use hanzo_client::{Http, Request, Transport};
use hanzo_client::{Method, Reply};
use serde::{Deserialize, Serialize};
use serde_json::Value;
#[cfg(unix)]
use std::path::Path;
use std::time::Duration;

use crate::commands::host;
use crate::commands::product::envelope_error;
use crate::config::Config;

const HANZOD_HTTP_ADDR: &str = "127.0.0.1:3690";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PsRow {
    #[serde(alias = "name")]
    pub component: String,
    #[serde(default)]
    pub layer: String,
    #[serde(alias = "state")]
    pub status: String,
    #[serde(default)]
    pub detail: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub namespace: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub endpoint: Option<String>,
}

impl PsRow {
    pub fn display_detail(&self) -> String {
        let mut parts = Vec::new();
        if !self.detail.trim().is_empty() {
            parts.push(self.detail.trim().to_string());
        }
        if let Some(ep) = self.endpoint.as_deref().filter(|s| !s.trim().is_empty()) {
            if !self.detail.contains(ep) {
                parts.push(ep.to_string());
            }
        }
        if parts.is_empty() {
            if let Some(ns) = self.namespace.as_deref().filter(|s| !s.trim().is_empty()) {
                parts.push(format!("ns/{ns}"));
            }
        }
        parts.join(" · ")
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PsResponse {
    #[serde(default)]
    pub rows: Vec<PsRow>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DoctorCheck {
    #[serde(default)]
    pub layer: String,
    pub name: String,
    pub status: String,
    #[serde(alias = "message", default)]
    pub detail: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DoctorResponse {
    #[serde(default = "default_true")]
    pub ok: bool,
    #[serde(default)]
    pub checks: Vec<DoctorCheck>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimeRequest {
    pub action: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cpus: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub memory_mb: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub disk_mb: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub backend: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub strict_registry: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnvironmentRequest {
    pub action: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub no_ui: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project_dir: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cloud_image: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub staged: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub iam_issuer: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ai_base_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub strict_registry: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServiceRequest {
    pub action: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppRequest {
    pub action: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project_dir: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SandboxRequest {
    pub action: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub image: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResetRequest {
    pub all: bool,
}

/// Outcome of calling `hanzod` `/v1/lifecycle/*`.
#[derive(Debug)]
pub enum HanzodOutcome {
    /// `hanzod` supports `/v1/lifecycle/*` and returned `2xx`.
    Handled(Value),
    /// `hanzod` is not installed, unreachable, unsigned on a bearer-protected route,
    /// or runs an older version (`404`/`405`/`501`) — caller reconciles locally.
    Unsupported,
}

/// Attempt to ensure `hanzod` is running and send a lifecycle request.
pub async fn call_lifecycle(
    cfg: Option<&mut Config>,
    method: Method,
    subpath: &str,
    body: Option<Value>,
    auto_start: bool,
) -> Result<HanzodOutcome> {
    let token = match cfg {
        Some(c) => crate::commands::daemon::bearer(c).await.unwrap_or_default(),
        None => std::env::var("HANZO_API_KEY").unwrap_or_default(),
    };
    let path = format!("/v1/lifecycle/{}", subpath.trim_start_matches('/'));
    if auto_start {
        let _ = host::ensure(HANZOD_HTTP_ADDR).await;
    }

    #[cfg(unix)]
    {
        let Ok(sock) = host::zap_socket() else {
            return Ok(HanzodOutcome::Unsupported);
        };
        call_over_zap_socket(&sock, &method, &path, &token, body.as_ref()).await
    }
    #[cfg(not(unix))]
    {
        let url = format!("http://{HANZOD_HTTP_ADDR}{path}");
        let mut req = Request::new(method, url).token(&token);
        if let Some(b) = body {
            req = req.body(b);
        }
        let Ok(Ok(reply)) =
            tokio::time::timeout(Duration::from_secs(10), Http::default().send(req)).await
        else {
            return Ok(HanzodOutcome::Unsupported);
        };
        classify_reply(reply, &path)
    }
}

#[cfg(unix)]
pub(crate) async fn call_over_zap_socket(
    sock: &Path,
    method: &Method,
    path: &str,
    token: &str,
    body: Option<&Value>,
) -> Result<HanzodOutcome> {
    if !sock.exists() {
        return Ok(HanzodOutcome::Unsupported);
    }
    let send_fut = crate::zap::send(sock, method, path, token, body);
    let Ok(Ok(reply)) = tokio::time::timeout(Duration::from_secs(10), send_fut).await else {
        return Ok(HanzodOutcome::Unsupported);
    };
    classify_reply(reply, path)
}

pub(crate) fn classify_reply(reply: Reply, path: &str) -> Result<HanzodOutcome> {
    if (200..300).contains(&reply.status) {
        return Ok(HanzodOutcome::Handled(reply.body));
    }
    if matches!(reply.status, 401 | 403 | 404 | 405 | 501 | 502 | 503) {
        return Ok(HanzodOutcome::Unsupported);
    }
    let detail = envelope_error(&reply.body)
        .or_else(|| {
            reply
                .body
                .get("error")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .unwrap_or_else(|| reply.body.to_string());
    bail!("hanzod {path} failed ({}): {detail}", reply.status)
}
