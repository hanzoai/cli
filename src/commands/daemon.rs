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
    /// Drive `hanzod`'s local computer-control route (read and act ops)
    Computer {
        /// Action to run (default: info), e.g. screenshot, list_windows, click
        #[arg(value_name = "ACTION")]
        action: Option<String>,
        /// JSON object of action arguments, e.g. '{"x":100,"y":200}'
        #[arg(long, value_name = "JSON")]
        data: Option<String>,
        /// Action argument as key=value (repeatable; values parse as JSON when possible)
        #[arg(long = "arg", value_name = "KEY=VALUE")]
        args: Vec<String>,
        /// Approve an act op (sends `{"approval": true}` with the request)
        #[arg(long)]
        approve: bool,
        /// Emit the raw JSON body on one line
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
    match args
        .command
        .unwrap_or(DaemonCommand::Status { json: false })
    {
        DaemonCommand::Status { json } => status(cfg, json).await,
        DaemonCommand::Sessions { command } => {
            match command.unwrap_or(SessionCommand::List { json: false }) {
                SessionCommand::List { json } => list_sessions(cfg, json).await,
                SessionCommand::New {
                    label,
                    workspace,
                    model,
                    json,
                } => create_session(cfg, label, workspace, model, json).await,
                SessionCommand::Show { id, json } => show_session(cfg, &id, json).await,
                SessionCommand::Rm { id } => delete_session(cfg, &id).await,
            }
        }
        DaemonCommand::Dev {
            prompt,
            session,
            workspace,
            model,
            label,
            json,
        } => {
            let text = prompt.join(" ").trim().to_string();
            if text.is_empty() {
                bail!("a prompt is required: `hanzo daemon dev \"summarize this repo\"`");
            }
            run_turn(cfg, &text, session, workspace, model, label, json).await
        }
        DaemonCommand::Computer {
            action,
            data,
            args,
            approve,
            json,
        } => computer(cfg, action, data, &args, approve, json).await,
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
    let outcome = val
        .get("outcome")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
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
    let info = expect_ok(
        call(&Method::GET, "/v1/hanzod", &token, None).await?,
        "hanzod info",
    )?;
    let dev = expect_ok(
        call(&Method::GET, "/v1/dev/health", &token, None).await?,
        "hanzod dev health",
    )?;
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

    let version = info
        .get("version")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let runtime = dev
        .get("runtime")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let sessions = dev.get("sessions").and_then(Value::as_u64).unwrap_or(0);
    let active = dev
        .get("active_sessions")
        .and_then(Value::as_u64)
        .unwrap_or(0);
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
            let dot = if running {
                "●".green()
            } else {
                "○".dimmed()
            };
            println!("    {dot} {prefix:<20} {name}");
        }
    }
    Ok(())
}

async fn list_sessions(cfg: &mut Config, as_json: bool) -> Result<()> {
    let token = bearer(cfg).await?;
    let val = expect_ok(
        call(&Method::GET, "/v1/dev/sessions", &token, None).await?,
        "list sessions",
    )?;
    if as_json {
        println!("{}", serde_json::to_string_pretty(&val)?);
        return Ok(());
    }
    let sessions = val
        .get("sessions")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
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
    let val = expect_ok(
        call(&Method::POST, "/v1/dev/sessions", &token, Some(body)).await?,
        "create session",
    )?;
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
        call(
            &Method::GET,
            &format!("/v1/dev/sessions/{id}"),
            &token,
            None,
        )
        .await?,
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
        call(
            &Method::DELETE,
            &format!("/v1/dev/sessions/{id}"),
            &token,
            None,
        )
        .await?,
        "delete session",
    )?;
    println!("{} deleted session {id}", "✓".green());
    Ok(())
}

/// Build `/v1/computer/{action}`, defaulting the action to `info`. Actions are
/// single path segments: reject anything else rather than letting an argument
/// rewrite the URL.
fn computer_path(action: Option<&str>) -> Result<String> {
    let action = action
        .map(str::trim)
        .filter(|a| !a.is_empty())
        .unwrap_or("info");
    if !action
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    {
        bail!("invalid computer action `{action}`: use letters, digits, `_` or `-`");
    }
    Ok(format!("/v1/computer/{action}"))
}

/// Values in `--arg key=value` are JSON when they parse as such (`5`, `true`,
/// `{"x":1}`), plain strings otherwise (`hello`).
fn parse_arg_value(raw: &str) -> Value {
    serde_json::from_str(raw).unwrap_or_else(|_| Value::String(raw.to_string()))
}

/// Merge `--data` and the repeatable `--arg` pairs into one request body, then
/// stamp approval on it when asked. The body field is accepted by both wires
/// (ZAP on Unix, HTTP on Windows), so no transport-specific header is needed.
fn computer_body(data: Option<&str>, args: &[String], approve: bool) -> Result<Value> {
    let mut body = match data {
        Some(raw) => match serde_json::from_str(raw)
            .with_context(|| format!("parsing --data as JSON: {raw}"))?
        {
            Value::Object(obj) => Value::Object(obj),
            other => bail!("--data must be a JSON object, got {other}"),
        },
        None => json!({}),
    };
    for arg in args {
        let (key, raw) = arg
            .split_once('=')
            .ok_or_else(|| anyhow!("invalid --arg `{arg}`: expected key=value"))?;
        if key.is_empty() {
            bail!("invalid --arg `{arg}`: key is empty");
        }
        body[key] = parse_arg_value(raw);
    }
    if approve {
        body["approval"] = json!(true);
    }
    Ok(body)
}

async fn computer(
    cfg: &mut Config,
    action: Option<String>,
    data: Option<String>,
    args: &[String],
    approve: bool,
    as_json: bool,
) -> Result<()> {
    let path = computer_path(action.as_deref())?;
    let body = computer_body(data.as_deref(), args, approve)?;
    let token = bearer(cfg).await?;
    let label = format!("computer {}", path.rsplit('/').next().unwrap_or("action"));
    let val = expect_ok(
        call(&Method::POST, &path, &token, Some(body)).await?,
        &label,
    )?;

    // The body is the result: screenshot paths and all other fields stay in it.
    // `--json` is the compact, pipeable form; the default is the same JSON,
    // indented for a person.
    if as_json {
        println!("{}", serde_json::to_string(&val)?);
    } else {
        println!("{}", serde_json::to_string_pretty(&val)?);
    }
    Ok(())
}

pub(crate) async fn bearer(cfg: &mut Config) -> Result<String> {
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

/// Read-only probe against a running `hanzod` without auto-spawning it.
pub(crate) async fn probe_no_spawn(method: &Method, path: &str) -> Option<Reply> {
    #[cfg(unix)]
    {
        let sock = host::zap_socket().ok()?;
        if !sock.exists() {
            return None;
        }
        tokio::time::timeout(
            std::time::Duration::from_millis(400),
            crate::zap::send(&sock, method, path, "", None),
        )
        .await
        .ok()?
        .ok()
    }
    #[cfg(not(unix))]
    {
        let url = format!("http://{HANZOD_HTTP_ADDR}{path}");
        let req = Request::new(method.clone(), url);
        tokio::time::timeout(
            std::time::Duration::from_millis(400),
            Http::default().send(req),
        )
        .await
        .ok()?
        .ok()
    }
}

/// Probe `hanzod` `/v1/dev/health` if `hanzod` is running (never spawns).
pub(crate) async fn probe_dev_health() -> Option<Value> {
    probe_no_spawn(&Method::GET, "/v1/dev/health")
        .await
        .filter(|r| (200..300).contains(&r.status))
        .map(|r| r.body)
}

/// Probe `hanzod` `/v1/engine/catalog` (spawning `hanzod` if available, else `None`).
pub(crate) async fn probe_engine_catalog() -> Option<Value> {
    let _ = host::ensure(HANZOD_HTTP_ADDR).await;
    probe_no_spawn(&Method::GET, "/v1/engine/catalog")
        .await
        .filter(|r| (200..300).contains(&r.status))
        .map(|r| r.body)
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
        .or_else(|| {
            reply
                .body
                .get("error")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
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

pub(crate) fn create_session_body(
    label: Option<String>,
    workspace: &str,
    model: Option<&str>,
) -> Value {
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
    use clap::Parser;

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
            Reply {
                status: 201,
                body: json!({ "id": "dev_1" }),
            },
            "create",
        )
        .unwrap();
        assert_eq!(ok["id"], "dev_1");

        let err = expect_ok(
            Reply {
                status: 401,
                body: json!({ "error": "a bearer is required" }),
            },
            "create",
        )
        .unwrap_err();
        assert!(err.to_string().contains("a bearer is required"));
    }

    #[test]
    fn computer_path_defaults_to_info_and_stays_one_segment() {
        assert_eq!(computer_path(None).unwrap(), "/v1/computer/info");
        assert_eq!(computer_path(Some("  ")).unwrap(), "/v1/computer/info");
        assert_eq!(
            computer_path(Some(" list_windows ")).unwrap(),
            "/v1/computer/list_windows"
        );
        assert_eq!(
            computer_path(Some("set-failsafe_2")).unwrap(),
            "/v1/computer/set-failsafe_2"
        );
        assert!(computer_path(Some("../dev/sessions")).is_err());
        assert!(computer_path(Some("a/b")).is_err());
    }

    #[test]
    fn computer_body_merges_data_and_args() {
        let body = computer_body(
            Some(r#"{"x":1,"label":"old"}"#),
            &[
                "x=5".into(),
                "enabled=true".into(),
                "label=hello world".into(),
                r#"payload={"k":1}"#.into(),
            ],
            false,
        )
        .unwrap();
        assert_eq!(
            body,
            json!({"x": 5, "enabled": true, "label": "hello world", "payload": {"k": 1}})
        );
    }

    #[test]
    fn computer_body_rejects_bad_data_and_args() {
        assert!(computer_body(Some("[1,2]"), &[], false).is_err());
        assert!(computer_body(Some("{"), &[], false).is_err());
        assert!(computer_body(None, &["no-equals".into()], false).is_err());
        assert!(computer_body(None, &["=v".into()], false).is_err());
    }

    #[test]
    fn computer_body_approval_is_explicit_and_wins() {
        assert_eq!(computer_body(None, &[], false).unwrap(), json!({}));
        assert_eq!(
            computer_body(None, &[], true).unwrap(),
            json!({"approval": true})
        );
        let overridden = computer_body(
            Some(r#"{"approval":false}"#),
            &["approval=false".into()],
            true,
        )
        .unwrap();
        assert_eq!(overridden, json!({"approval": true}));
    }

    #[test]
    fn daemon_computer_clap_parses_action_args_and_flags() {
        #[derive(Parser)]
        struct Cli {
            #[command(flatten)]
            daemon: DaemonArgs,
        }

        let parsed = Cli::try_parse_from([
            "hanzo",
            "computer",
            "click",
            "--data",
            r#"{"x":1}"#,
            "--arg",
            "y=2",
            "--arg",
            "text=hi",
            "--approve",
            "--json",
        ])
        .unwrap();
        match parsed.daemon.command.unwrap() {
            DaemonCommand::Computer {
                action,
                data,
                args,
                approve,
                json,
            } => {
                assert_eq!(action.as_deref(), Some("click"));
                assert_eq!(data.as_deref(), Some(r#"{"x":1}"#));
                assert_eq!(args, ["y=2", "text=hi"]);
                assert!(approve && json);
            }
            _ => panic!("expected a computer command"),
        }
    }
}
