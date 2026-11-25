//! `hanzo sources` — HIP-1350 unified machine sources, provider accounts, and
//! cross-source usage (`/v1/sources`, `/v1/sources/accounts`, `/v1/sources/usage`).
//!
//! Links this machine under the signed-in person with a persistent Ed25519
//! device keypair, signs heartbeats using the exact `hanzo-source-heartbeat/1`
//! canonical wire format verified by `hanzo-inc/cloud` (`apps/source`), and
//! exposes unified provider accounts (KMS-sealed keys + linked machine logins)
//! and unified usage right from the CLI.

use anyhow::{anyhow, bail, Context, Result};
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use chrono::Utc;
use clap::{Args, Subcommand};
use colored::*;
use hanzo_client::{Method, Reply};
use ring::rand::SystemRandom;
use ring::signature::{Ed25519KeyPair, KeyPair};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::commands::product::{envelope_error, Seam};
use crate::config::Config;
use crate::iam::{paths::DEFAULT_BRAND, provider};

#[derive(Args, Clone)]
pub struct SourcesArgs {
    #[command(subcommand)]
    pub command: Option<SourcesCommand>,
}

#[derive(Subcommand, Clone)]
pub enum SourcesCommand {
    /// List your linked machines / sources (`GET /v1/sources`)
    List {
        /// Emit raw JSON
        #[arg(long)]
        json: bool,
    },
    /// Link this machine as a HIP-1350 source and send signed Ed25519 heartbeats
    Link {
        /// Display name for this machine (defaults to hostname)
        #[arg(long)]
        name: Option<String>,
        /// Reachable ZAP endpoint (`https://...` or `wss://...`)
        #[arg(long)]
        zap: Option<String>,
        /// Mark discovered local provider logins as shared with your other surfaces
        #[arg(long)]
        share_providers: bool,
        /// Register + send one signed online heartbeat, then exit (for cron/tests)
        #[arg(long)]
        once: bool,
        /// Emit raw JSON
        #[arg(long)]
        json: bool,
    },
    /// Revoke a linked source (`DELETE /v1/sources/:id`)
    #[command(alias = "rm")]
    Revoke {
        /// Source id (`src_...`)
        id: String,
    },
    /// Manage unified provider accounts (KMS-sealed keys + logins reported by sources)
    Accounts {
        #[command(subcommand)]
        command: Option<AccountsCommand>,
    },
    /// Show unified cross-source usage (`GET /v1/sources/usage`)
    Usage {
        /// Grouping dimension: source | provider | model | user | day
        #[arg(long, default_value = "provider", value_parser = ["source", "provider", "model", "user", "day"])]
        by: String,
        /// Start timestamp (RFC 3339)
        #[arg(long)]
        from: Option<String>,
        /// End timestamp (RFC 3339)
        #[arg(long)]
        to: Option<String>,
        /// Emit raw JSON
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand, Clone)]
pub enum AccountsCommand {
    /// List unified provider accounts (`GET /v1/sources/accounts`)
    List {
        #[arg(long)]
        json: bool,
    },
    /// Seal a provider API key into Hanzo KMS (`POST /v1/sources/accounts`)
    Add {
        /// Provider catalog name (e.g. `anthropic`, `openai`, `google`, `openrouter`)
        #[arg(long)]
        provider: String,
        /// Human label for this key (e.g. `work-anthropic`)
        #[arg(long)]
        label: String,
        /// Secret key value, or `-` to read from stdin (recommended to keep secrets off argv)
        #[arg(long, default_value = "-")]
        key: String,
        #[arg(long)]
        json: bool,
    },
    /// Delete a KMS-sealed provider account (`DELETE /v1/sources/accounts/:id`)
    #[command(alias = "delete")]
    Rm { id: String },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SourceProvider {
    pub provider: String,
    pub instance: String,
    pub kind: String,
    pub state: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub plan: String,
    pub shared: bool,
}

pub async fn run(cfg: &mut Config, args: SourcesArgs) -> Result<()> {
    match args.command.unwrap_or(SourcesCommand::List { json: false }) {
        SourcesCommand::List { json } => list(cfg, json).await,
        SourcesCommand::Link {
            name,
            zap,
            share_providers,
            once,
            json,
        } => link(cfg, name, zap, share_providers, once, json).await,
        SourcesCommand::Revoke { id } => revoke(cfg, &id).await,
        SourcesCommand::Accounts { command } => {
            match command.unwrap_or(AccountsCommand::List { json: false }) {
                AccountsCommand::List { json } => list_accounts(cfg, json).await,
                AccountsCommand::Add {
                    provider,
                    label,
                    key,
                    json,
                } => add_account(cfg, &provider, &label, &key, json).await,
                AccountsCommand::Rm { id } => delete_account(cfg, &id).await,
            }
        }
        SourcesCommand::Usage { by, from, to, json } => usage(cfg, &by, from, to, json).await,
    }
}

async fn list(cfg: &mut Config, as_json: bool) -> Result<()> {
    let seam = Seam::open(cfg, DEFAULT_BRAND).await?;
    let val = expect_ok(
        seam.send(Method::GET, "/v1/sources", &[], None).await?,
        "list sources",
    )?;
    if as_json {
        println!("{}", serde_json::to_string_pretty(&val)?);
        return Ok(());
    }
    let sources = val
        .get("sources")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if sources.is_empty() {
        println!("no linked sources — run `hanzo sources link` to link this machine");
        return Ok(());
    }
    for s in sources {
        let id = s.get("id").and_then(Value::as_str).unwrap_or("");
        let name = s.get("name").and_then(Value::as_str).unwrap_or("");
        let state = s.get("state").and_then(Value::as_str).unwrap_or("offline");
        let os = s.get("os").and_then(Value::as_str).unwrap_or("");
        let arch = s.get("arch").and_then(Value::as_str).unwrap_or("");
        let version = s.get("version").and_then(Value::as_str).unwrap_or("");
        let dot = match state {
            "online" => "●".green(),
            "revoked" => "✗".red(),
            _ => "○".dimmed(),
        };
        let provs: Vec<String> = s
            .get("providers")
            .and_then(Value::as_array)
            .map(|arr| {
                arr.iter()
                    .filter_map(|p| {
                        let pn = p.get("provider").and_then(Value::as_str)?;
                        let st = p.get("state").and_then(Value::as_str).unwrap_or("");
                        Some(format!("{pn}({st})"))
                    })
                    .collect()
            })
            .unwrap_or_default();
        println!(
            "{dot} {id}  {name:<20}  {state:<8}  {os}/{arch}  v{version}  {}",
            provs.join(", ").dimmed()
        );
    }
    Ok(())
}

async fn link(
    cfg: &mut Config,
    name: Option<String>,
    zap: Option<String>,
    share_providers: bool,
    once: bool,
    as_json: bool,
) -> Result<()> {
    let mut seam = Seam::open(cfg, DEFAULT_BRAND).await?;
    let key_path = source_key_path()?;
    let kp = load_or_create_keypair(&key_path)?;
    let pub_b64 = URL_SAFE_NO_PAD.encode(kp.public_key().as_ref());

    let host_name = name
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(default_hostname);
    let version = env!("CARGO_PKG_VERSION").to_string();
    let os = canonical_os();
    let arch = canonical_arch();

    let mut link_body = json!({
        "kind": "desktop",
        "name": host_name,
        "os": os,
        "arch": arch,
        "version": version,
        "key": pub_b64,
    });
    if let Some(z) = zap.as_deref().filter(|s| !s.trim().is_empty()) {
        link_body["zap"] = json!(z);
    }

    let linked = expect_ok(
        seam.send(Method::POST, "/v1/sources", &[], Some(link_body))
            .await?,
        "link source",
    )?;
    let id = linked
        .get("id")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("cloud did not return a source id"))?
        .to_string();
    let interval_secs = linked
        .get("interval")
        .and_then(Value::as_u64)
        .unwrap_or(30)
        .max(5);
    let effective_zap = zap.unwrap_or_else(|| format!("https://zap.hanzo.ai/v1/dev/zap/{id}"));

    let providers = discover_local_providers(share_providers);
    let beat_reply =
        send_heartbeat(&seam, &kp, &id, &version, &effective_zap, &providers, 0).await?;

    if as_json {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "source": linked,
                "heartbeat": beat_reply,
                "zap": effective_zap,
                "providers": providers,
            }))?
        );
    } else {
        println!(
            "{} linked {} ({}) — online with {} provider(s)",
            "✓".green(),
            host_name.bold(),
            id.cyan(),
            providers.len()
        );
        println!("  zap       {effective_zap}");
        for p in &providers {
            println!(
                "  provider  {:<12} {:<16} {} ({}{})",
                p.provider,
                p.instance.dimmed(),
                p.state,
                p.kind,
                if p.shared { ", shared" } else { "" }
            );
        }
    }

    if once {
        return Ok(());
    }

    eprintln!("  heartbeating every {interval_secs}s (Ctrl-C to stop)...");
    let mut ticker = tokio::time::interval(Duration::from_secs(interval_secs));
    ticker.tick().await; // consume immediate tick
    let mut last_auth_refresh = tokio::time::Instant::now();
    // OAuth access tokens typically live 45–60m; refresh proactively every 35m and on any 401.
    const PROACTIVE_REFRESH: Duration = Duration::from_secs(35 * 60);
    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {
                eprintln!("  stopped heartbeating {id}");
                return Ok(());
            }
            _ = ticker.tick() => {
                if last_auth_refresh.elapsed() >= PROACTIVE_REFRESH {
                    if let Ok(fresh) = Seam::open(cfg, DEFAULT_BRAND).await {
                        seam = fresh;
                        last_auth_refresh = tokio::time::Instant::now();
                    }
                }
                let provs = discover_local_providers(share_providers);
                match send_heartbeat(&seam, &kp, &id, &version, &effective_zap, &provs, 0).await {
                    Ok(_) => {}
                    Err(e) if is_auth_failure(&e) => {
                        match Seam::open(cfg, DEFAULT_BRAND).await {
                            Ok(fresh) => {
                                seam = fresh;
                                last_auth_refresh = tokio::time::Instant::now();
                                if let Err(retry_err) = send_heartbeat(&seam, &kp, &id, &version, &effective_zap, &provs, 0).await {
                                    eprintln!("  {} heartbeat failed after token refresh: {retry_err:#}", "!".yellow());
                                }
                            }
                            Err(refresh_err) => {
                                eprintln!("  {} heartbeat auth refresh failed: {refresh_err:#}", "!".yellow());
                            }
                        }
                    }
                    Err(e) => eprintln!("  {} heartbeat failed: {e:#}", "!".yellow()),
                }
            }
        }
    }
}

fn is_auth_failure(err: &anyhow::Error) -> bool {
    let msg = format!("{err:#}");
    msg.contains("(401)")
        || msg.contains("401 Unauthorized")
        || msg.contains("validated principal is required")
}

async fn send_heartbeat(
    seam: &Seam,
    kp: &Ed25519KeyPair,
    id: &str,
    version: &str,
    zap: &str,
    providers: &[SourceProvider],
    busy: i32,
) -> Result<Value> {
    let at = Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let msg = heartbeat_signing_message(id, &at, version, zap, busy, providers);
    let sig = URL_SAFE_NO_PAD.encode(kp.sign(msg.as_bytes()).as_ref());
    let body = json!({
        "at": at,
        "version": version,
        "zap": zap,
        "providers": providers,
        "busy": busy,
        "sig": sig,
    });
    expect_ok(
        seam.send(
            Method::POST,
            &format!("/v1/sources/{id}/heartbeat"),
            &[],
            Some(body),
        )
        .await?,
        "source heartbeat",
    )
}

async fn revoke(cfg: &mut Config, id: &str) -> Result<()> {
    let seam = Seam::open(cfg, DEFAULT_BRAND).await?;
    expect_ok(
        seam.send(Method::DELETE, &format!("/v1/sources/{id}"), &[], None)
            .await?,
        "revoke source",
    )?;
    println!("{} revoked source {id}", "✓".green());
    Ok(())
}

async fn list_accounts(cfg: &mut Config, as_json: bool) -> Result<()> {
    let seam = Seam::open(cfg, DEFAULT_BRAND).await?;
    let val = expect_ok(
        seam.send(Method::GET, "/v1/sources/accounts", &[], None)
            .await?,
        "list provider accounts",
    )?;
    if as_json {
        println!("{}", serde_json::to_string_pretty(&val)?);
        return Ok(());
    }
    let accounts = val
        .get("accounts")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if accounts.is_empty() {
        println!("no unified provider accounts yet — run `hanzo sources link --share-providers` or `hanzo sources accounts add`");
        return Ok(());
    }
    for a in accounts {
        let id = a.get("id").and_then(Value::as_str).unwrap_or("");
        let prov = a.get("provider").and_then(Value::as_str).unwrap_or("");
        let kind = a.get("kind").and_then(Value::as_str).unwrap_or("");
        let label = a.get("label").and_then(Value::as_str).unwrap_or("");
        let state = a.get("state").and_then(Value::as_str).unwrap_or("sealed");
        let classes: Vec<&str> = a
            .get("classes")
            .and_then(Value::as_array)
            .map(|c| c.iter().filter_map(Value::as_str).collect())
            .unwrap_or_default();
        println!(
            "{id}  {prov:<12}  {kind:<6}  {label:<18}  {state:<8}  [{}]",
            classes.join(",")
        );
    }
    Ok(())
}

async fn add_account(
    cfg: &mut Config,
    provider_name: &str,
    label: &str,
    key_arg: &str,
    as_json: bool,
) -> Result<()> {
    let value = if key_arg == "-" {
        let mut buf = String::new();
        std::io::stdin()
            .read_to_string(&mut buf)
            .context("reading API key from stdin")?;
        buf.trim().to_string()
    } else {
        key_arg.trim().to_string()
    };
    if value.is_empty() {
        bail!("key value is empty");
    }
    let seam = Seam::open(cfg, DEFAULT_BRAND).await?;
    let body = json!({
        "provider": provider_name,
        "kind": "key",
        "label": label,
        "value": value,
    });
    let val = expect_ok(
        seam.send(Method::POST, "/v1/sources/accounts", &[], Some(body))
            .await?,
        "add provider account",
    )?;
    if as_json {
        println!("{}", serde_json::to_string_pretty(&val)?);
        return Ok(());
    }
    let id = val.get("id").and_then(Value::as_str).unwrap_or("");
    let fp = val.get("fingerprint").and_then(Value::as_str).unwrap_or("");
    println!(
        "{} sealed {provider_name} key '{label}' ({id}, fp {fp})",
        "✓".green()
    );
    Ok(())
}

async fn delete_account(cfg: &mut Config, id: &str) -> Result<()> {
    let seam = Seam::open(cfg, DEFAULT_BRAND).await?;
    expect_ok(
        seam.send(
            Method::DELETE,
            &format!("/v1/sources/accounts/{id}"),
            &[],
            None,
        )
        .await?,
        "delete provider account",
    )?;
    println!("{} removed provider account {id}", "✓".green());
    Ok(())
}

async fn usage(
    cfg: &mut Config,
    by: &str,
    from: Option<String>,
    to: Option<String>,
    as_json: bool,
) -> Result<()> {
    let seam = Seam::open(cfg, DEFAULT_BRAND).await?;
    let mut query = vec![format!("by={by}")];
    if let Some(f) = from {
        query.push(format!("from={f}"));
    }
    if let Some(t) = to {
        query.push(format!("to={t}"));
    }
    let val = expect_ok(
        seam.send(Method::GET, "/v1/sources/usage", &query, None)
            .await?,
        "sources usage",
    )?;
    if as_json {
        println!("{}", serde_json::to_string_pretty(&val)?);
        return Ok(());
    }
    println!("{}", serde_json::to_string_pretty(&val)?);
    Ok(())
}

fn expect_ok(reply: Reply, action: &str) -> Result<Value> {
    if (200..300).contains(&reply.status) {
        return Ok(reply.body);
    }
    let detail = envelope_error(&reply.body)
        .or_else(|| {
            reply
                .body
                .get("detail")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .unwrap_or_else(|| reply.body.to_string());
    bail!("{action} failed ({}): {detail}", reply.status)
}

/// Canonical `hanzo-source-heartbeat/1` signing payload, matching `hanzo-inc/cloud`
/// (`apps/source/source.go::beatMessage`) byte-for-byte.
pub fn heartbeat_signing_message(
    id: &str,
    at: &str,
    version: &str,
    zap: &str,
    busy: i32,
    providers: &[SourceProvider],
) -> String {
    let mut out = String::new();
    out.push_str("hanzo-source-heartbeat/1\n");
    out.push_str(id);
    out.push('\n');
    out.push_str(at);
    out.push('\n');
    out.push_str(version);
    out.push('\n');
    out.push_str(zap);
    out.push('\n');
    out.push_str(&busy.to_string());
    out.push('\n');
    for p in providers {
        out.push_str(&format!(
            "{},{},{},{},{},{}\n",
            p.provider, p.instance, p.kind, p.state, p.plan, p.shared
        ));
    }
    out
}

fn source_key_path() -> Result<PathBuf> {
    let dir = dirs::config_dir()
        .or_else(|| dirs::home_dir().map(|h| h.join(".config")))
        .ok_or_else(|| anyhow!("cannot locate config directory"))?
        .join("hanzo");
    std::fs::create_dir_all(&dir)?;
    Ok(dir.join("source-key.ed25519"))
}

pub fn load_or_create_keypair(path: &Path) -> Result<Ed25519KeyPair> {
    // Two first-time `hanzo sources link` processes must share one device key.
    // The lock lives beside the key, so it also guards a not-yet-created file.
    let _guard = crate::config::Lock::acquire(path)?;
    if path.is_file() {
        let pkcs8 = std::fs::read(path)
            .with_context(|| format!("reading source device key {}", path.display()))?;
        return Ed25519KeyPair::from_pkcs8(&pkcs8)
            .map_err(|_| anyhow!("invalid Ed25519 keypair in {}", path.display()));
    }
    let rng = SystemRandom::new();
    let doc = Ed25519KeyPair::generate_pkcs8(&rng)
        .map_err(|_| anyhow!("generating Ed25519 source keypair"))?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    write_private(path, doc.as_ref())?;
    Ed25519KeyPair::from_pkcs8(doc.as_ref())
        .map_err(|_| anyhow!("loading generated Ed25519 source keypair"))
}

fn write_private(path: &Path, bytes: &[u8]) -> Result<()> {
    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .mode(0o600)
            .open(path)
            .with_context(|| format!("writing {}", path.display()))?;
        f.write_all(bytes)?;
        Ok(())
    }
    #[cfg(not(unix))]
    {
        std::fs::write(path, bytes).with_context(|| format!("writing {}", path.display()))
    }
}

/// Discover local AI provider state on this machine (metadata/state only —
/// never credentials, tokens, or emails).
fn discover_local_providers(shared: bool) -> Vec<SourceProvider> {
    let mut out = vec![SourceProvider {
        provider: "hanzo".into(),
        instance: "hanzo-gateway".into(),
        kind: "login".into(),
        state: "ready".into(),
        plan: "unified".into(),
        shared,
    }];

    // Local provider API keys stored in the CLI's portable Vault.
    for (prov, name) in [
        (provider::Provider::Anthropic, "anthropic"),
        (provider::Provider::OpenAI, "openai"),
    ] {
        if provider::key(prov).ok().flatten().is_some() {
            out.push(SourceProvider {
                provider: name.into(),
                instance: format!("{name}-vault"),
                kind: "key".into(),
                state: "ready".into(),
                plan: String::new(),
                shared,
            });
        }
    }

    // Local coding-agent CLIs installed on this machine, using the canonical
    // HIP-1350 provider catalog names (`policy.go`: claude, codex, copilot get
    // `desktop` + `local` dispatch classes when shared).
    for (bin, prov) in [
        ("claude", "claude"),
        ("codex", "codex"),
        ("copilot", "copilot"),
        ("gemini", "gemini"),
        ("opencode", "opencode"),
    ] {
        if which::which(bin).is_ok() {
            out.push(SourceProvider {
                provider: prov.into(),
                instance: prov.into(),
                kind: "login".into(),
                state: "ready".into(),
                plan: String::new(),
                shared,
            });
        }
    }

    out
}

fn default_hostname() -> String {
    std::env::var("HOSTNAME")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .or_else(|| {
            std::fs::read_to_string("/etc/hostname")
                .ok()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
        })
        .unwrap_or_else(|| "hanzo-machine".to_string())
}

fn canonical_os() -> &'static str {
    match std::env::consts::OS {
        "macos" => "darwin",
        other => other,
    }
}

fn canonical_arch() -> &'static str {
    match std::env::consts::ARCH {
        "aarch64" => "arm64",
        "x86_64" => "amd64",
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ring::signature::{UnparsedPublicKey, ED25519};

    #[test]
    fn heartbeat_signing_message_matches_cloud_wire_format() {
        let provs = vec![
            SourceProvider {
                provider: "anthropic".into(),
                instance: "claude-1".into(),
                kind: "login".into(),
                state: "ready".into(),
                plan: "max".into(),
                shared: true,
            },
            SourceProvider {
                provider: "openai".into(),
                instance: "codex-cli".into(),
                kind: "login".into(),
                state: "ready".into(),
                plan: String::new(),
                shared: false,
            },
        ];
        let msg = heartbeat_signing_message(
            "src_0123",
            "2026-11-22T05:00:00Z",
            "8.5.1336",
            "https://zap.hanzo.ai/v1/dev/zap/src_0123",
            2,
            &provs,
        );
        assert_eq!(
            msg,
            concat!(
                "hanzo-source-heartbeat/1\n",
                "src_0123\n",
                "2026-11-22T05:00:00Z\n",
                "8.5.1336\n",
                "https://zap.hanzo.ai/v1/dev/zap/src_0123\n",
                "2\n",
                "anthropic,claude-1,login,ready,max,true\n",
                "openai,codex-cli,login,ready,,false\n",
            )
        );
    }

    #[test]
    fn concurrent_first_source_links_share_one_persisted_keypair() {
        use std::sync::{Arc, Barrier};

        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("source-key.ed25519");
        const WORKERS: usize = 12;
        let barrier = Arc::new(Barrier::new(WORKERS));
        let keys = std::thread::scope(|scope| {
            let tasks: Vec<_> = (0..WORKERS)
                .map(|_| {
                    let path = path.clone();
                    let start = barrier.clone();
                    scope.spawn(move || {
                        start.wait();
                        load_or_create_keypair(&path)
                            .unwrap()
                            .public_key()
                            .as_ref()
                            .to_vec()
                    })
                })
                .collect();
            tasks
                .into_iter()
                .map(|task| task.join().unwrap())
                .collect::<Vec<_>>()
        });
        let persisted = load_or_create_keypair(&path).unwrap();
        assert!(keys
            .iter()
            .all(|key| key.as_slice() == persisted.public_key().as_ref()));
        assert_eq!(keys.len(), WORKERS);
    }

    #[test]
    fn keypair_persists_and_verifies_signed_heartbeat() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("source-key.ed25519");
        let kp1 = load_or_create_keypair(&path).unwrap();
        let kp2 = load_or_create_keypair(&path).unwrap();
        assert_eq!(kp1.public_key().as_ref(), kp2.public_key().as_ref());
        assert_eq!(kp1.public_key().as_ref().len(), 32);

        let msg =
            heartbeat_signing_message("src_test", "2026-11-22T05:00:00Z", "8.5.1336", "", 0, &[]);
        let sig = kp1.sign(msg.as_bytes());
        let pubkey = UnparsedPublicKey::new(&ED25519, kp2.public_key().as_ref());
        pubkey.verify(msg.as_bytes(), sig.as_ref()).unwrap();
    }

    #[test]
    fn detects_401_auth_failure_for_heartbeat_refresh() {
        assert!(is_auth_failure(&anyhow!(
            "source heartbeat failed (401): a validated principal is required: send Authorization: Bearer <API key>"
        )));
        assert!(is_auth_failure(&anyhow!("401 Unauthorized")));
        assert!(!is_auth_failure(&anyhow!(
            "source heartbeat failed (503): no instance running"
        )));
    }
}
