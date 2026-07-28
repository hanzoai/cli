//! The ONE authenticated cloud call for the hand-written resource commands
//! (`cluster`, `node`). It resolves WHERE (the active network's api origin) and
//! WHO (the active identity's bearer) through the same identity seam every other
//! command uses, then sends through [`crate::http::send_json`]. The CLI sends the
//! bearer AND NOTHING ELSE — cloud derives the tenant from the JWT `owner` it
//! verifies, so there is no `--org` and none can be forged.
//!
//! The generated product tree has its own in-module seam (`product::call`); this
//! is its hand-written twin, for resource verbs whose arg SHAPE (a positional
//! `NAME`, a friendly flag set) does not match a generated leaf one-to-one — the
//! exact same reason `billing`/`connector`/`usage` are hand-written.

use anyhow::{anyhow, Result};
use reqwest::{Client, Method};
use serde_json::Value;

use crate::commands::host::{self, Origin};
use crate::config::Config;
use crate::iam::{paths, store};

/// Send one authenticated request to a FINAL `/v1/…` path and return the parsed
/// 2xx body (a non-2xx is an error carrying the server's own message). `path` is
/// already template-filled — never a user-supplied host.
pub async fn call(cfg: &mut Config, method: Method, path: &str, body: Option<&Value>) -> Result<Value> {
    // WHERE — the same seam the generated tree uses, so a LOCAL origin gets a host
    // started (or reused) behind it here too, and is spoken to over ZAP. WHO — a
    // local host may have no IAM, so a missing credential goes out as no bearer and
    // the server decides.
    let origin = host::origin(cfg).await?;
    let token = match store::active_token(cfg, paths::DEFAULT_BRAND)? {
        Some((_id, tok)) => tok.access_token,
        None if matches!(origin, Origin::LocalZap(_)) => String::new(),
        None => return Err(not_signed_in()),
    };
    let (status, resp) = match &origin {
        Origin::LocalZap(sock) => crate::zap::send(sock, &method, path, &token, body).await?,
        Origin::Http(base) => {
            crate::http::send(&Client::new(), method, &format!("{base}{path}"), &token, body).await?
        }
    };
    // The "fail on a non-2xx" rule this seam has always had, applied once for both
    // wires rather than duplicated per transport.
    if !status.is_success() {
        let shown = match &resp {
            Value::Null => String::new(),
            Value::String(s) => s.trim().to_string(),
            v => v.to_string(),
        };
        anyhow::bail!("{path} -> {status}: {shown}");
    }
    Ok(resp)
}

/// The active identity's org (its `owner` claim) — for the org-scoped paths
/// (`paas/cluster/doks/{org}/…`). Addressed, never asserted: the server re-checks
/// it against the JWT it verifies, so a wrong one 403s you against yourself.
pub fn owner(cfg: &mut Config) -> Result<String> {
    let (id, _) = store::active_token(cfg, paths::DEFAULT_BRAND)?.ok_or_else(not_signed_in)?;
    Ok(id.owner)
}

fn not_signed_in() -> anyhow::Error {
    anyhow!("not signed in — run `hanzo auth login`")
}

/// Print a cloud response: surface the `/v1` envelope's `data` (what a caller
/// pipes) when present, else the raw body. Pretty JSON for structures, the bare
/// string for a scalar.
pub fn print(v: &Value) {
    let shown = match v.get("data") {
        Some(d) if !d.is_null() => d,
        _ => v,
    };
    match shown {
        Value::Null => {}
        Value::String(s) => println!("{s}"),
        other => println!("{}", serde_json::to_string_pretty(other).unwrap_or_else(|_| other.to_string())),
    }
}

/// Percent-encode ONE URL path segment (RFC 3986 unreserved kept), so an org or
/// id value addresses the segment the user named, never a different route.
pub fn enc(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => out.push(b as char),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}
