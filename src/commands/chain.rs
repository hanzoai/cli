//! `hanzo chain` — opt in to the chain, or out of it (HIP-1330 phase 6).
//!
//! The chain is `hanzo-chain`, a child of `hanzod`, fetched on opt-in and absent
//! otherwise. This command is a client of `hanzod`'s `/v1/chain`:
//!
//!   GET    /v1/chain                  status
//!   PUT    /v1/chain {"network": n}   enable for a network (fetch, verify, start)
//!   DELETE /v1/chain                  disable (stop, release the ports)
//!
//! Nothing here fetches, verifies or spawns anything. `hanzod` owns the digest
//! table, the version check and the keys; the CLI states what the user asked for.

use crate::config::Config;
use anyhow::{bail, Context, Result};
use colored::*;
use hanzo_client::Method;
use serde_json::{json, Value};

/// The networks `hanzo-chain` knows. `hanzod` checks again; this is the usage line.
const NETWORKS: [&str; 3] = ["mainnet", "testnet", "devnet"];

/// Send one request to `hanzod` and return its JSON body. A non-2xx status is the
/// daemon's refusal, and its own message is what the user reads.
async fn call(method: Method, body: Option<&Value>) -> Result<Value> {
    let reply = send(method, body).await?;
    if !(200..300).contains(&reply.status) {
        let why = reply
            .body
            .get("error")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .unwrap_or_else(|| reply.body.to_string());
        bail!("hanzod refused ({}): {}", reply.status, why);
    }
    Ok(reply.body)
}

#[cfg(unix)]
async fn send(method: Method, body: Option<&Value>) -> Result<hanzo_client::Reply> {
    let sock = dirs::data_local_dir()
        .unwrap_or_default()
        .join("hanzo")
        .join("hanzod.zap.sock");
    crate::zap::send(&sock, &method, "/v1/chain", "", body)
        .await
        .context("hanzod is not running; start it with `hanzo host start`")
}

/// Windows has no UNIX socket, so `hanzod`'s loopback view is the only wire.
#[cfg(not(unix))]
async fn send(method: Method, body: Option<&Value>) -> Result<hanzo_client::Reply> {
    use hanzo_client::{Http, Request, Transport};
    let mut req = Request::new(method, "http://127.0.0.1:3690/v1/chain".to_string());
    if let Some(b) = body {
        req = req.body(b.clone());
    }
    Http::default()
        .send(req)
        .await
        .context("hanzod is not running; start it with `hanzo host start`")
}

fn show(s: &Value) {
    if !s["enabled"].as_bool().unwrap_or(false) {
        println!(
            "{} chain is off: no ports, no peers, no state",
            "-".dimmed()
        );
        return;
    }
    let up = s["running"].as_bool().unwrap_or(false);
    let mark = if up { "ok".green() } else { "stopped".red() };
    println!(
        "{} chain {} on {}",
        mark,
        s["version"].as_str().unwrap_or("?"),
        s["network"].as_str().unwrap_or("?")
    );
    if let Some(h) = s["height"].as_u64() {
        println!("  height {h}");
    }
    if let Some(p) = s["peers"].as_u64() {
        println!("  peers  {p}");
    }
}

pub async fn status(_config: &Config) -> Result<()> {
    show(&call(Method::GET, None).await?);
    Ok(())
}

pub async fn up(_config: &Config, network: String) -> Result<()> {
    if !NETWORKS.contains(&network.as_str()) {
        bail!(
            "unknown network {network:?}; one of {}",
            NETWORKS.join(", ")
        );
    }
    show(&call(Method::PUT, Some(&json!({ "network": network }))).await?);
    Ok(())
}

pub async fn down(_config: &Config) -> Result<()> {
    show(&call(Method::DELETE, None).await?);
    Ok(())
}
