//! `hanzo share <port>` — publish a local service to a public
//! `https://<token>.share.hanzo.ai` URL: ngrok on our own zero-trust fabric.
//!
//! DX: the per-org zrok account is provisioned SERVER-SIDE from your
//! `hanzo login` identity (`POST /v1/share/enable`) — no separate signup, no
//! manual `zrok enable`, no config file. `hanzo share 3000` provisions, runs the
//! fabric tunnel, and prints the public URL. The command sends only the bearer;
//! the org is the gateway's to derive from the JWT (never a client field), the
//! same rule as `hanzo billing` / `hanzo usage`.
//!
//! The heavy fabric client is the `zrok` helper (located on `PATH` or via
//! `HANZO_ZROK_BIN`); every credential + endpoint it needs comes from the cloud,
//! so the user never touches zrok directly.

use crate::config::Config;
use crate::iam::{paths, store};
use crate::commands::network;
use anyhow::{anyhow, bail, Context, Result};
use colored::*;
use reqwest::Client;
use serde::Deserialize;
use std::process::Stdio;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;

/// What the cloud hands us to run the tunnel.
#[derive(Debug, Deserialize)]
struct EnableResp {
    #[serde(rename = "accountToken")]
    account_token: String,
    controller: String,
    #[serde(default)]
    namespace: String,
    #[serde(rename = "urlTemplate", default)]
    url_template: String,
}

/// `hanzo share <target> [--backend-mode M] [--name N]`.
pub async fn run(
    cfg: &mut Config,
    target: String,
    backend_mode: String,
    name: Option<String>,
) -> Result<()> {
    let backend = resolve_target(&target)?;

    // 1. Provision from the login identity — one authed call, org server-derived.
    let api = network::active(cfg).api.trim_end_matches('/').to_string();
    let (_id, tok) = store::active_token(cfg, paths::DEFAULT_BRAND)?
        .ok_or_else(|| anyhow!("not signed in — run `hanzo login` first"))?;
    let pr = enable(&api, &tok.access_token).await?;

    // 2. Locate the zrok fabric helper.
    let zbin = zrok_bin()?;

    // 3. Enable this machine against the provisioned account (idempotent, quiet).
    let _ = Command::new(&zbin)
        .args(["enable", &pr.account_token])
        .env("ZROK2_API_ENDPOINT", &pr.controller)
        .env("ZROK_API_ENDPOINT", &pr.controller)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .await;

    // 4. Share and stream, surfacing the public URL.
    let mut args = vec![
        "share".into(),
        "public".into(),
        "--headless".into(),
        "--backend-mode".into(),
        backend_mode,
    ];
    if !pr.namespace.is_empty() {
        args.push("--name-selection".into());
        args.push(pr.namespace.clone());
    }
    if let Some(n) = &name {
        args.push("--name".into());
        args.push(n.clone());
    }
    args.push(backend.clone());

    println!("{} sharing {}", "→".green(), backend.cyan());
    let mut child = Command::new(&zbin)
        .args(&args)
        .env("ZROK2_API_ENDPOINT", &pr.controller)
        .env("ZROK_API_ENDPOINT", &pr.controller)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("start {zbin} share"))?;

    let tmpl = pr.url_template.clone();
    if let Some(out) = child.stdout.take() {
        let tmpl = tmpl.clone();
        tokio::spawn(async move { stream(out, tmpl).await });
    }
    if let Some(err) = child.stderr.take() {
        tokio::spawn(async move { stream(err, tmpl).await });
    }
    let status = child.wait().await?;
    if !status.success() {
        bail!("share ended: {status}");
    }
    Ok(())
}

/// One authenticated provisioning call. Sends ONLY the bearer — no org.
async fn enable(api: &str, token: &str) -> Result<EnableResp> {
    let url = format!("{api}/v1/share/enable");
    let resp = Client::new()
        .post(&url)
        .bearer_auth(token)
        .json(&serde_json::json!({}))
        .send()
        .await
        .with_context(|| format!("request {url}"))?;
    let status = resp.status();
    let text = resp.text().await.unwrap_or_default();
    if status == reqwest::StatusCode::SERVICE_UNAVAILABLE {
        bail!("sharing is not enabled on this deployment yet");
    }
    if !status.is_success() {
        bail!("provision share: HTTP {status}: {}", text.trim());
    }
    serde_json::from_str(&text).with_context(|| "decode share credential")
}

/// Accept "3000", "localhost:3000", or a full URL → a backend URL to proxy.
fn resolve_target(arg: &str) -> Result<String> {
    let arg = arg.trim();
    if arg.is_empty() {
        bail!("a port, host:port, or url is required");
    }
    if arg.contains("://") {
        return Ok(arg.to_string());
    }
    if let Ok(port) = arg.parse::<u32>() {
        if !(1..=65535).contains(&port) {
            bail!("port out of range: {port}");
        }
        return Ok(format!("http://localhost:{port}"));
    }
    if arg.contains(':') {
        return Ok(format!("http://{arg}"));
    }
    bail!("could not parse target {arg:?} (want a port, host:port, or url)")
}

/// Relay helper output; highlight the public URL when the share token appears
/// (`… <token>.public`).
async fn stream<R: tokio::io::AsyncRead + Unpin>(r: R, url_template: String) {
    let mut lines = BufReader::new(r).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        if let Some(tok) = share_token(&line) {
            let url = url_template.replace("{token}", &tok);
            println!("\n  {}  →  live\n", url.green().bold());
        } else {
            eprintln!("{}", line.dimmed());
        }
    }
}

/// Pull the share token from a "`<token>.public`" announcement.
fn share_token(line: &str) -> Option<String> {
    let i = line.find(".public")?;
    let bytes = line.as_bytes();
    let mut start = i;
    while start > 0 {
        let b = bytes[start - 1];
        if b.is_ascii_lowercase() || b.is_ascii_digit() {
            start -= 1;
        } else {
            break;
        }
    }
    let tok = &line[start..i];
    (tok.len() >= 6).then(|| tok.to_string())
}

/// Locate the zrok helper: `HANZO_ZROK_BIN`, then `zrok2`/`zrok` on `PATH`.
fn zrok_bin() -> Result<String> {
    if let Ok(b) = std::env::var("HANZO_ZROK_BIN") {
        let b = b.trim().to_string();
        if !b.is_empty() {
            return Ok(b);
        }
    }
    for name in ["zrok2", "zrok"] {
        if which(name).is_some() {
            return Ok(name.to_string());
        }
    }
    bail!(
        "the zrok fabric helper was not found on PATH; install it or set \
         HANZO_ZROK_BIN=/path/to/zrok"
    )
}

/// Minimal PATH lookup (no extra dependency).
fn which(name: &str) -> Option<String> {
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        let cand = dir.join(name);
        if cand.is_file() {
            return Some(cand.to_string_lossy().into_owned());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_targets() {
        assert_eq!(resolve_target("3000").unwrap(), "http://localhost:3000");
        assert_eq!(resolve_target("localhost:8080").unwrap(), "http://localhost:8080");
        assert_eq!(resolve_target("https://x.local").unwrap(), "https://x.local");
        assert!(resolve_target("0").is_err());
        assert!(resolve_target("").is_err());
    }

    #[test]
    fn extracts_share_token() {
        assert_eq!(
            share_token("access your zrok share at:\n g3q84fzbgfpy.public").as_deref(),
            Some("g3q84fzbgfpy")
        );
        assert_eq!(share_token("no token here"), None);
        assert_eq!(share_token("ab.public"), None); // too short
    }
}
