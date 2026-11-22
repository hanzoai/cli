//! `hanzo daemon` — direct client for the per-machine Rust `hanzod` daemon
//! (HIP-1330), the exact same backend Hanzo Desktop uses for local Dev sessions,
//! SQLite state, checkpoints, local AI catalog, and cloud relay.
//!
//! Speaks native ZAP-HTTP over `<data>/hanzo/hanzod.zap.sock` on Unix (loopback
//! HTTP on `127.0.0.1:3690` on Windows) and auto-starts `~/.hanzo/bin/hanzod`
//! when it is not already listening.

use anyhow::{anyhow, bail, Context, Result};
use clap::{Args, Subcommand};
use colored::*;
#[cfg(not(unix))]
use hanzo_client::{Http, Request, Transport};
use hanzo_client::{Method, Reply};
use serde_json::{json, Value};
use std::path::PathBuf;

use crate::commands::host;
use crate::commands::product::envelope_error;
use crate::config::Config;
use crate::iam::{paths::DEFAULT_BRAND, store};

const HANZOD_HTTP_ADDR: &str = "127.0.0.1:3690";

#[derive(Args, Clone)]
pub struct DaemonArgs {
    #[command(subcommand)]
    pub command: Option<DaemonCommand>,
}

#[derive(Subcommand, Clone)]
pub enum DaemonCommand {
    /// Show `hanzod` version, native Rust Dev runtime health, routes, and catalog
    Status {
        /// Emit raw JSON
        #[arg(long)]
        json: bool,
    },
    /// Manage Rust-native Dev sessions persisted in `hanzod`'s SQLite store
    Sessions {
        #[command(subcommand)]
        command: Option<SessionCommand>,
    },
    /// Run a coding turn through `hanzod`'s Rust-native Dev runtime (same backend as Hanzo Desktop)
    Dev {
        /// Prompt / task to execute
        prompt: Vec<String>,
        /// Resume an existing `hanzod` session id instead of creating a new one
        #[arg(long, value_name = "SESSION_ID")]
        session: Option<String>,
        /// Workspace root for file/exec/git tools (defaults to current directory)
        #[arg(long, value_name = "DIR")]
        workspace: Option<PathBuf>,
        /// Model to use (e.g. `zen5-coder`, `enso-auto`, or a `local:*` model)
        #[arg(long)]
        model: Option<String>,
        /// Optional label when creating a new session
        #[arg(long)]
        label: Option<String>,
        /// Emit raw JSON
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand, Clone)]
pub enum SessionCommand {
    /// List local `hanzod` Dev sessions
    List {
        #[arg(long)]
        json: bool,
    },
    /// Create a new local `hanzod` Dev session
    New {
        #[arg(long)]
        label: Option<String>,
        #[arg(long)]
        workspace: Option<PathBuf>,
        #[arg(long)]
        model: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// Show session details, messages, and checkpoints
    Show {
        id: String,
        #[arg(long)]
        json: bool,
    },
    /// Delete a local `hanzod` Dev session
    #[command(alias = "delete")]
    Rm { id: String },
}

pub async fn run(cfg: &mut Config, args: DaemonArgs) -> Result<()> {
    match args.command.unwrap_or(DaemonCommand::Status { json: false }) {
        DaemonCommand::Status { json } => status(cfg, json).await,
        DaemonCommand::Sessions { command } => match command.unwrap_or(SessionCommand::List { json: false }) {
            SessionCommand::List { json } => list_sessions(cfg, json).await,
            SessionCommand::New { label, workspace, model, json } => {
                create_session(cfg, label, workspace, model, json).await
            }
            SessionCommand::Show { id, json } => show_session(cfg, &id, json).await,
            SessionCommand::Rm { id } => delete_session(cfg, &id).await,
        },
        DaemonCommand::Dev { prompt, session, workspace, model, label, json } => {
            let text = prompt.join(" ").trim().to_string();
            if text.is_empty() {
                bail!("a prompt is required: `hanzo daemon dev \"summarize this repo\"`");
            }
            run_turn(cfg, &text, session, workspace, model, label, json).await
        }
    }
}

/// Entry point shared with `hanzo dev --daemon <prompt>`.
pub async fn run_turn(
    cfg: &mut Config,
    prompt: &str,
    session_id: Option<String>,
    workspace: Option<PathBuf>,
    model: Option<String>,
    label: Option<String>,
    as_json: bool,
) -> Result<()> {
    let token = bearer(cfg).await?;
    let sid = match session_id {
        Some(id) => id,
        None => {
            let ws = resolve_workspace(workspace)?;
            let body = create_session_body(label, &ws, model.as_deref());
            let reply = call(&Method::POST, "/v1/dev/sessions", &token, Some(body)).await?;
            let val = expect_ok(reply, "create hanzod dev session")?;
            val.get("id")
                .and_then(Value::as_str)
                .map(str::to_string)
                .ok_or_else(|| anyhow!("hanzod returned a session without an id"))?
        }
    };

    let mut req_body = json!({ "prompt": prompt });
    if let Some(m) = model.as_deref().filter(|s| !s.trim().is_empty()) {
        req_body["model"] = json!(m);
    }

    let path = format!("/v1/dev/sessions/{sid}/turns");
    let reply = call(&Method::POST, &path, &token, Some(req_body)).await?;
    let val = expect_ok(reply, "run hanzod dev turn")?;

    if as_json {
        println!("{}", serde_json::to_string_pretty(&val)?);
        return Ok(());
    }

    let turn_id = val.get("turn_id").and_then(Value::as_u64).unwrap_or(0);
    let outcome = val.get("outcome").and_then(Value::as_str).unwrap_or("unknown");
    eprintln!(
        "{} session {} · turn {} · {}",
        "hanzod".cyan().bold(),
        sid.dimmed(),
        turn_id,
        outcome
    );

    if let Some(actions) = val.get("actions").and_then(Value::as_array) {
        for a in actions {
            let kind = a.get("kind").and_then(Value::as_str).unwrap_or("action");
            let summary = a.get("summary").and_then(Value::as_str).unwrap_or("");
            let failed = a.get("failed").and_then(Value::as_bool).unwrap_or(false);
            let mark = if failed { "✗".red() } else { "·".dimmed() };
            eprintln!("  {mark} {} {}", kind.dimmed(), summary);
        }
    }

    if let Some(text) = val.get("text").and_then(Value::as_str) {
        println!("{text}");
    } else if let Some(emitted) = val.get("emitted").and_then(Value::as_array) {
        for item in emitted {
            if let Some(s) = item.as_str() {
                println!("{s}");
            }
        }
    }
    Ok(())
}

async fn status(cfg: &mut Config, as_json: bool) -> Result<()> {
    let token = bearer(cfg).await?;
    let info = expect_ok(call(&Method::GET, "/v1/hanzod", &token, None).await?, "hanzod info")?;
    let dev = expect_ok(call(&Method::GET, "/v1/dev/health", &token, None).await?, "hanzod dev health")?;
    let catalog = call(&Method::GET, "/v1/engine/catalog", &token, None)
        .await
        .ok()
        .filter(|r| (200..300).contains(&r.status))
        .map(|r| r.body);

    if as_json {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "hanzod": info,
                "dev": dev,
                "catalog": catalog,
            }))?
        );
        return Ok(());
    }

    let version = info.get("version").and_then(Value::as_str).unwrap_or("unknown");
    let runtime = dev.get("runtime").and_then(Value::as_str).unwrap_or("unknown");
    let sessions = dev.get("sessions").and_then(Value::as_u64).unwrap_or(0);
    let active = dev.get("active_sessions").and_then(Value::as_u64).unwrap_or(0);
    let sqlite = dev.get("sqlite_path").and_then(Value::as_str).unwrap_or("");

    println!("{} hanzod v{version} ({runtime})", "●".green());
    #[cfg(unix)]
    if let Ok(sock) = host::zap_socket() {
        println!("  wire      zap://{}", sock.display());
    }
    println!("  http      http://{HANZOD_HTTP_ADDR}");
    println!("  sessions  {sessions} total ({active} active)");
    if !sqlite.is_empty() {
        println!("  sqlite    {sqlite}");
    }
    if let Some(routes) = info.get("routes").and_then(Value::as_array) {
        println!("  routes");
        for r in routes {
            let prefix = r.get("prefix").and_then(Value::as_str).unwrap_or("");
            let name = r.get("name").and_then(Value::as_str).unwrap_or("");
            let running = r.get("running").and_then(Value::as_bool).unwrap_or(false);
            let dot = if running { "●".green() } else { "○".dimmed() };
            println!("    {dot} {prefix:<20} {name}");
        }
    }
    Ok(())
}

async fn list_sessions(cfg: &mut Config, as_json: bool) -> Result<()> {
    let token = bearer(cfg).await?;
    let val = expect_ok(call(&Method::GET, "/v1/dev/sessions", &token, None).await?, "list sessions")?;
    if as_json {
        println!("{}", serde_json::to_string_pretty(&val)?);
        return Ok(());
    }
    let sessions = val.get("sessions").and_then(Value::as_array).cloned().unwrap_or_default();
    if sessions.is_empty() {
        println!("no local hanzod sessions — start one with `hanzo dev --daemon \"<task>\"`");
        return Ok(());
    }
    for s in sessions {
        let id = s.get("id").and_then(Value::as_str).unwrap_or("");
        let status = s.get("status").and_then(Value::as_str).unwrap_or("");
        let label = s.get("label").and_then(Value::as_str).unwrap_or("");
        let ws = s.get("workspace").and_then(Value::as_str).unwrap_or("");
        println!("{id}  {status:<10}  {label:<24}  {ws}");
    }
    Ok(())
}

async fn create_session(
    cfg: &mut Config,
    label: Option<String>,
    workspace: Option<PathBuf>,
    model: Option<String>,
    as_json: bool,
) -> Result<()> {
    let token = bearer(cfg).await?;
    let ws = resolve_workspace(workspace)?;
    let body = create_session_body(label, &ws, model.as_deref());
    let val = expect_ok(call(&Method::POST, "/v1/dev/sessions", &token, Some(body)).await?, "create session")?;
    if as_json {
        println!("{}", serde_json::to_string_pretty(&val)?);
        return Ok(());
    }
    let id = val.get("id").and_then(Value::as_str).unwrap_or("");
    println!("{} created session {id} ({ws})", "✓".green());
    Ok(())
}

async fn show_session(cfg: &mut Config, id: &str, as_json: bool) -> Result<()> {
    let token = bearer(cfg).await?;
    let val = expect_ok(
        call(&Method::GET, &format!("/v1/dev/sessions/{id}"), &token, None).await?,
        "show session",
    )?;
    if as_json {
        println!("{}", serde_json::to_string_pretty(&val)?);
        return Ok(());
    }
    println!("{}", serde_json::to_string_pretty(&val)?);
    Ok(())
}

async fn delete_session(cfg: &mut Config, id: &str) -> Result<()> {
    let token = bearer(cfg).await?;
    expect_ok(
        call(&Method::DELETE, &format!("/v1/dev/sessions/{id}"), &token, None).await?,
        "delete session",
    )?;
    println!("{} deleted session {id}", "✓".green());
    Ok(())
}

async fn bearer(cfg: &mut Config) -> Result<String> {
    if let Ok(key) = std::env::var("HANZO_API_KEY") {
        let trimmed = key.trim();
        if !trimmed.is_empty() {
            return Ok(trimmed.to_string());
        }
    }
    Ok(store::active_token(cfg, DEFAULT_BRAND)
        .await?
        .map(|(_, t)| t.access_token)
        .unwrap_or_default())
}

/// Ensure `hanzod` is running and send a request over native ZAP-HTTP (Unix) or
/// loopback HTTP (Windows).
async fn call(method: &Method, path: &str, token: &str, body: Option<Value>) -> Result<Reply> {
    host::ensure(HANZOD_HTTP_ADDR).await?;
    #[cfg(unix)]
    {
        let sock = host::zap_socket()?;
        crate::zap::send(&sock, method, path, token, body.as_ref()).await
    }
    #[cfg(not(unix))]
    {
        let url = format!("http://{HANZOD_HTTP_ADDR}{path}");
        let mut req = Request::new(method.clone(), url).token(token);
        if let Some(b) = body {
            req = req.body(b);
        }
        Ok(Http::default().send(req).await?)
    }
}

fn expect_ok(reply: Reply, action: &str) -> Result<Value> {
    if (200..300).contains(&reply.status) {
        return Ok(reply.body);
    }
    let detail = envelope_error(&reply.body)
        .or_else(|| reply.body.get("error").and_then(Value::as_str).map(str::to_string))
        .unwrap_or_else(|| reply.body.to_string());
    bail!("{action} failed ({}): {detail}", reply.status)
}

fn resolve_workspace(workspace: Option<PathBuf>) -> Result<String> {
    let dir = match workspace {
        Some(p) => p,
        None => std::env::current_dir().context("reading current directory")?,
    };
    let canonical = std::fs::canonicalize(&dir).unwrap_or(dir);
    Ok(canonical.to_string_lossy().into_owned())
}

pub(crate) fn create_session_body(label: Option<String>, workspace: &str, model: Option<&str>) -> Value {
    let mut obj = json!({ "workspace": workspace });
    if let Some(l) = label.filter(|s| !s.trim().is_empty()) {
        obj["label"] = json!(l);
    }
    if let Some(m) = model.filter(|s| !s.trim().is_empty()) {
        obj["model"] = json!(m);
    }
    obj
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_session_body_omits_blank_fields() {
        let b = create_session_body(Some("  ".into()), "/tmp/work", None);
        assert_eq!(b, json!({ "workspace": "/tmp/work" }));

        let full = create_session_body(Some("sprint".into()), "/tmp/work", Some("zen5-coder"));
        assert_eq!(
            full,
            json!({ "workspace": "/tmp/work", "label": "sprint", "model": "zen5-coder" })
        );
    }

    #[test]
    fn expect_ok_accepts_201_created_and_surfaces_error_body() {
        let ok = expect_ok(
            Reply { status: 201, body: json!({ "id": "dev_1" }) },
            "create",
        )
        .unwrap();
        assert_eq!(ok["id"], "dev_1");

        let err = expect_ok(
            Reply { status: 401, body: json!({ "error": "a bearer is required" }) },
            "create",
        )
        .unwrap_err();
        assert!(err.to_string().contains("a bearer is required"));
    }
}
