//! `hanzo share <port>` — publish a local service to a public
//! `https://<token>.share.hanzo.ai` URL over the Hanzo zero-trust fabric: the
//! port stays bound to localhost and the fabric carries the traffic, so nothing
//! is exposed to the network the machine sits on.
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

/// Who a share is published for, and who vouches for them.
///
/// ONE VALUE, because it is one decision. Naming a provider and not naming a
/// person publishes a shell to everyone that provider has ever heard of — every
/// account on hanzo.id, in any org — which is what "protected" used to mean here
/// and is not what anybody means by it. The two were separate flags on the
/// helper and it was possible, in fact usual, to pass the first without the
/// second.
pub struct Owner<'a> {
    /// The provider the frontend recognises, by the name it is configured under.
    pub provider: &'a str,
    /// The address it must recognise. The helper reads this as a glob; an
    /// address is a glob that matches one address.
    pub email: &'a str,
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

/// The variable the fabric helper reads its controller address from.
///
/// Named for the helper, not for us: ours is the `zrok2` lineage and it reads
/// `ZROK2_API_ENDPOINT` (`environment/env_v0_4/api.go`). Under any other name the
/// helper silently keeps its compiled-in default — the PUBLIC zrok service — and
/// every call goes to a controller that has never heard of this org. That failed
/// as a share which never announces a url, sixty seconds later, with nothing
/// naming the cause.
const CONTROLLER_ENV: &str = "ZROK2_API_ENDPOINT";

/// What sits behind a share, in OUR words, and the helper's word for it.
///
/// The fabric helper's fourth mode is named after an outside web server. That name
/// is the helper's business and not the customer's: what they are choosing is
/// "serve a directory of files", and a Hanzo command should say so. One function
/// translates, at the one place the value leaves for the helper — so the flag a
/// person types and the flag `--help` prints are the same word, and the helper
/// still gets the word it understands.
fn wire_backend_mode(mode: &str) -> &str {
    match mode {
        "static" => "caddy",
        other => other,
    }
}

/// `hanzo share <target> [--backend-mode M] [--name N]`.
pub async fn run(
    cfg: &mut Config,
    target: String,
    backend_mode: String,
    name: Option<String>,
) -> Result<()> {
    let mut sh = start(cfg, target, backend_mode, name, None).await?;
    println!("\n  {}  →  live\n", sh.url.green().bold());
    sh.wait().await
}

/// Provision, enable and start a public share, returning it once zrok has
/// announced the URL.
///
/// This is the ONE implementation of "publish a local port on the fabric";
/// `hanzo share` prints the URL and waits, `hanzo link` puts it on a session and
/// holds it. Two commands, one tunnel.
pub async fn start(
    cfg: &mut Config,
    target: String,
    backend_mode: String,
    name: Option<String>,
    owner: Option<Owner<'_>>,
) -> Result<Share> {
    let backend = resolve_target(&target)?;

    // 1. Provision from the login identity — one authed call, org server-derived.
    let api = network::active(cfg).api.trim_end_matches('/').to_string();
    let (_id, tok) = store::active_token(cfg, paths::DEFAULT_BRAND)
        .await?
        .ok_or_else(|| anyhow!("not signed in — run `hanzo auth login` first"))?;
    let pr = enable(&api, &tok.access_token).await?;

    // 2. Locate the zrok fabric helper.
    let zbin = zrok_bin()?;

    // 3. Enable this machine against the provisioned account (idempotent, quiet).
    let _ = Command::new(&zbin)
        .args(["enable", &pr.account_token])
        .env(CONTROLLER_ENV, &pr.controller)
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
        wire_backend_mode(&backend_mode).to_string(),
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
    // A published terminal is a shell, so it is published FOR SOMEONE. Both
    // halves travel together: the provider makes the URL insufficient on its own,
    // and the address makes the provider insufficient on its own. Without the
    // second, every account hanzo.id knows — every org — could open this shell by
    // learning the hostname, which is a smaller set than the internet and not a
    // small one.
    if let Some(o) = &owner {
        if !supports_flag(&zbin, "--oauth-provider").await {
            bail!("this zrok cannot say who a share is for (--oauth-provider unsupported); refusing to publish an open terminal");
        }
        args.extend(owner_args(o));
    }
    args.push(backend.clone());

    println!("{} sharing {}", "→".green(), backend.cyan());
    let mut child = Command::new(&zbin)
        .args(&args)
        .env(CONTROLLER_ENV, &pr.controller)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("start {zbin} share"))?;

    // zrok announces the token on one of the two streams; whichever carries it
    // first wins, and the other keeps echoing as log.
    let tmpl = pr.url_template.clone();
    let said = Said::default();
    let (tx, mut rx) = tokio::sync::mpsc::channel::<String>(2);
    if let Some(out) = child.stdout.take() {
        let (tmpl, tx, said) = (tmpl.clone(), tx.clone(), said.clone());
        tokio::spawn(async move { stream(out, tmpl, tx, said).await });
    }
    if let Some(err) = child.stderr.take() {
        let said = said.clone();
        tokio::spawn(async move { stream(err, tmpl, tx, said).await });
    }

    // A tunnel that never announces a URL is not a share; time out rather than
    // hand back one that nothing can reach.
    let url = match tokio::time::timeout(std::time::Duration::from_secs(60), rx.recv()).await {
        Ok(Some(u)) => u,
        _ => {
            let _ = child.start_kill();
            bail!("zrok did not announce a share url{}", said.tail());
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

/// The last thing the fabric helper said before it stopped saying anything.
///
/// A share that never announces is the helper refusing for a reason it already
/// printed — the wrong controller, an environment that was never enabled, a
/// namespace the controller does not know. That reason went to `debug!`, so the
/// error a person actually saw named the symptom and threw the cause away.
/// Keeping ONE line costs nothing and turns a silent timeout into a sentence.
#[derive(Clone, Default)]
struct Said(std::sync::Arc<std::sync::Mutex<Option<String>>>);

impl Said {
    fn hear(&self, line: &str) {
        if let Ok(mut g) = self.0.lock() {
            *g = Some(line.to_string());
        }
    }

    /// Rendered for the tail of an error, or empty when it never spoke.
    fn tail(&self) -> String {
        match self.0.lock().ok().and_then(|g| g.clone()) {
            Some(l) => format!("; it last said: {}", l.trim()),
            None => String::new(),
        }
    }
}

/// Relay helper output; highlight the public URL when the share token appears
/// (`… <token>.public`).
async fn stream<R: tokio::io::AsyncRead + Unpin>(
    r: R,
    url_template: String,
    tx: tokio::sync::mpsc::Sender<String>,
    said: Said,
) {
    let mut lines = BufReader::new(r).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        if let Some(tok) = share_token(&line) {
            let _ = tx.try_send(url_template.replace("{token}", &tok));
        } else {
            said.hear(&line);
            // The tunnel's own log is diagnostics, not output. A share prints one
            // URL; a link prints one URL and then holds a shell — neither wants the
            // fabric narrating every GET. `-v` turns it back on.
            tracing::debug!("{}", line);
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

/// The helper flags that say who a share is for.
///
/// One function so the pair cannot come apart. The helper's second flag is named
/// after domains and takes address globs, which is exactly the kind of name that
/// invites passing the first flag alone and calling it done.
fn owner_args(o: &Owner<'_>) -> [String; 4] {
    [
        "--oauth-provider".into(),
        o.provider.into(),
        "--oauth-email-domain".into(),
        o.email.into(),
    ]
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

/// Locate the zrok helper: `HANZO_ZROK_BIN`, then `zrok` on `PATH`.
fn zrok_bin() -> Result<String> {
    if let Ok(b) = std::env::var("HANZO_ZROK_BIN") {
        let b = b.trim().to_string();
        if !b.is_empty() {
            return Ok(b);
        }
    }
    for name in ["zrok"] {
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

    // A SHARE IS PUBLISHED FOR SOMEONE, and both halves of that say so. The
    // provider alone was the old behaviour: it makes the URL insufficient, and
    // leaves every account hanzo.id knows — every org — able to open the shell
    // by learning the hostname.
    #[test]
    fn a_published_share_names_the_person_it_is_for() {
        let args = owner_args(&Owner { provider: "hanzo", email: "z@hanzo.ai" });
        assert_eq!(
            args,
            ["--oauth-provider", "hanzo", "--oauth-email-domain", "z@hanzo.ai"]
        );
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
