//! `hanzo share <port>` — publish a local service to a public
//! `https://<token>.share.hanzo.ai` URL: ngrok on our own zero-trust fabric.
//!
//! DX: the per-org zrok account is provisioned SERVER-SIDE from your
//! `hanzo auth login` identity (`POST /v1/share/enable`) — no separate signup, no
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

/// A running share: the zrok child and the public URL it announced.
///
/// Dropping it kills the tunnel, so a caller that holds one cannot leave a share
/// alive after it has stopped caring about it.
pub struct Share {
    pub url: String,
    child: tokio::process::Child,
}

impl Drop for Share {
    fn drop(&mut self) {
        let _ = self.child.start_kill();
    }
}

impl Share {
    /// Wait for the tunnel to end, echoing zrok's log as it goes.
    pub async fn wait(&mut self) -> Result<()> {
        let status = self.child.wait().await?;
        if !status.success() {
            bail!("share ended: {status}");
        }
        Ok(())
    }
}

/// `hanzo share <target> [--backend-mode M] [--name N]`.
pub async fn run(
    cfg: &mut Config,
    target: String,
    backend_mode: String,
    name: Option<String>,
) -> Result<()> {
    let mut sh = start(cfg, target, backend_mode, name).await?;
    println!("\n  {}  →  live\n", sh.url.green().bold());
    sh.wait().await
}

/// Provision, enable and start a public share, returning it once zrok has
/// announced the URL.
///
/// This is the ONE implementation of "publish a local port on the fabric";
/// `hanzo share` prints the URL and waits, `hanzo link` puts it on a session and
/// holds it. Two front doors, one tunnel.
pub async fn start(
    cfg: &mut Config,
    target: String,
    backend_mode: String,
    name: Option<String>,
) -> Result<Share> {
    let backend = resolve_target(&target)?;

    // 1. Provision from the login identity — one authed call, org server-derived.
    let api = network::active(cfg).api.trim_end_matches('/').to_string();
    let (_id, tok) = store::active_token(cfg, paths::DEFAULT_BRAND)?
        .ok_or_else(|| anyhow!("not signed in — run `hanzo auth login` first"))?;
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
    // --name-selection is OUR fork's flag. A machine with upstream zrok on PATH
    // has a binary that rejects it outright, which killed the share rather than
    // degrading — so ask the binary what it takes before assuming.
    if !pr.namespace.is_empty() {
        if supports_flag(&zbin, "--name-selection").await {
            args.push("--name-selection".into());
            args.push(pr.namespace.clone());
        } else {
            eprintln!(
                "{} zrok has no --name-selection; sharing without the {} namespace",
                "note:".yellow(),
                pr.namespace
            );
        }
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

    // zrok announces the token on one of the two streams; whichever carries it
    // first wins, and the other keeps echoing as log.
    let tmpl = pr.url_template.clone();
    let (tx, mut rx) = tokio::sync::mpsc::channel::<String>(2);
    if let Some(out) = child.stdout.take() {
        let (tmpl, tx) = (tmpl.clone(), tx.clone());
        tokio::spawn(async move { stream(out, tmpl, tx).await });
    }
    if let Some(err) = child.stderr.take() {
        tokio::spawn(async move { stream(err, tmpl, tx).await });
    }

    // A tunnel that never announces a URL is not a share; time out rather than
    // hand back one that nothing can reach.
    let url = match tokio::time::timeout(std::time::Duration::from_secs(60), rx.recv()).await {
        Ok(Some(u)) => u,
        _ => {
            let _ = child.start_kill();
            bail!("zrok did not announce a share url");
        }
    };
    Ok(Share { url, child })
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
async fn stream<R: tokio::io::AsyncRead + Unpin>(
    r: R,
    url_template: String,
    tx: tokio::sync::mpsc::Sender<String>,
) {
    let mut lines = BufReader::new(r).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        if let Some(tok) = share_token(&line) {
            let _ = tx.try_send(url_template.replace("{token}", &tok));
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

/// Does this zrok accept `flag` on `share public`?
///
/// Asked rather than assumed because two zrok lineages are in the wild — ours and
/// upstream — and the difference is a hard failure at share time, not a warning.
async fn supports_flag(zbin: &str, flag: &str) -> bool {
    Command::new(zbin)
        .args(["share", "public", "--help"])
        .output()
        .await
        .map(|o| {
            let text = String::from_utf8_lossy(&o.stdout).into_owned()
                + &String::from_utf8_lossy(&o.stderr);
            text.contains(flag)
        })
        .unwrap_or(false)
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
