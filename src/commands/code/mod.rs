//! `hanzo code` — wrap a local coding agent (Claude Code or `dev`) so a
//! developer's terminal session is (opt-in) linked, live-streamed and tracked in
//! Hanzo cloud, with the Hanzo MCP toolset attached and model usage metered
//! universally through the Hanzo gateway.
//!
//! Three things are wired natively:
//!   1. Session link + live stream — register on `/v1/agents/sessions`, forward
//!      the backend's structured events, and mark the terminal status. OPT-IN
//!      (`--link` or persisted `code.link`); default off.
//!   2. Hanzo MCP — attached in-session (Claude `--mcp-config`, `dev` `-c`).
//!   3. hanzo.id auth + universal usage — model calls route through
//!      api.hanzo.ai so tokens/cost meter into cloud_usage/o11y regardless of
//!      which account/machine the dev is on.
//!
//! Sessions are PORTABLE: the register carries a no-secret context snapshot, the
//! backend's own resume handle + a transcript pointer are persisted, and
//! `--resume <sessionId>` restores cwd/repo/ref and relaunches the backend with
//! its native resume against the same cloud session.

mod backend;
mod claude;
mod context;
mod dev;
mod event;
mod session;
#[cfg(test)]
pub(crate) mod testmock;

use anyhow::{anyhow, Context, Result};
use colored::*;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncReadExt, AsyncSeekExt};

use crate::config::Config;
use crate::{commands::network, iam::token};

use backend::{resolve, resolve_mcp, BackendKind, Backend, Launch, Mode, Routing, Spec};
use context::{ResumeRecord, Snapshot};
use event::{Kind, Mapped, Status, Usage};
use session::SessionClient;

/// Parsed `hanzo code` invocation.
pub struct Options {
    pub backend: String,
    pub link: bool,
    pub no_link: bool,
    pub route: bool,
    pub mcp: bool,
    pub resume: Option<String>,
    pub brand: String,
    pub task: Option<String>,
    pub passthrough: Vec<String>,
}

/// Decide whether to stream to cloud: an explicit `--no-link` always wins, then
/// `--link`, else the persisted default.
pub(crate) fn effective_link(link: bool, no_link: bool, persisted: bool) -> bool {
    if no_link {
        false
    } else if link {
        true
    } else {
        persisted
    }
}

pub async fn run(cfg: &Config, opts: Options) -> Result<()> {
    let kind = BackendKind::parse(&opts.backend)?;
    let backend = resolve(kind);
    let mode = if opts.task.is_some() { Mode::Headless } else { Mode::Interactive };
    let api = network::active(cfg).api;

    // Auth: the hanzo.id bearer from the OS keychain (never argv/logged).
    let bearer = token::load(&opts.brand)?.map(|t| t.access_token);

    let mut do_link = effective_link(opts.link, opts.no_link, cfg.code.link);
    if do_link && bearer.is_none() {
        warn(&format!(
            "not signed in — run `hanzo login` to link this session. Continuing locally (no cloud stream)."
        ));
        do_link = false;
    }

    // Resume: restore cwd + the backend's own resume handle from the local store.
    let (cwd, resume_handle, resume_from) = match &opts.resume {
        Some(id) => {
            let rec = ResumeRecord::load(id)?.ok_or_else(|| {
                anyhow!(
                    "no local record for session {id} on this machine — resume runs where the session was created"
                )
            })?;
            let cwd = PathBuf::from(&rec.cwd);
            if !cwd.exists() {
                warn(&format!("recorded working dir {} no longer exists", rec.cwd));
            }
            (cwd, Some(rec.backend_session_id.clone()), Some(id.clone()))
        }
        None => (std::env::current_dir().context("resolving current dir")?, None, None),
    };

    // MCP: resolve hanzo-mcp; a missing server warns but never blocks.
    let mcp = if opts.mcp {
        let m = resolve_mcp(&cwd);
        if m.is_none() {
            warn("hanzo-mcp not found (install `hanzo-mcp` or `uv`) — continuing without the Hanzo toolset.");
        }
        m
    } else {
        None
    };

    // Routing: meter model calls through the gateway when signed in.
    let routing = if opts.route {
        bearer.clone().map(|token| Routing { api: api.clone(), token })
    } else {
        None
    };

    // For a linked interactive Claude run, pre-set the session id so its
    // transcript can be tailed; otherwise the resume handle names it.
    let preset_session = if do_link && mode == Mode::Interactive && kind == BackendKind::Claude && opts.resume.is_none() {
        Some(uuid_v4())
    } else {
        None
    };

    let snapshot = Snapshot::capture(&cwd, backend.label(), backend.version());

    // Cloud session (linked only). Resolve reuses a non-terminal resumed session
    // (same id) or forks a new one off a terminal / fresh session.
    let client = if do_link {
        Some(SessionClient::new(&api, bearer.as_deref().unwrap())?)
    } else {
        None
    };
    let mut session_id: Option<String> = None;
    if let Some(c) = &client {
        let title = session_title(&opts);
        match resolve_cloud_session(c, backend.label(), &title, resume_from.as_deref()).await {
            Ok((id, forked_from)) => {
                // The "where it runs" context snapshot (no secrets).
                let _ = c.event(&id, Kind::Status, snapshot.context_payload(forked_from.as_deref())).await;
                session_id = Some(id);
            }
            Err(e) => {
                // Fail-open for availability: never block the dev's work on a
                // cloud hiccup — degrade to a local (unlinked) run.
                warn(&format!("could not register session ({e}); continuing locally."));
            }
        }
    }

    banner(
        &opts,
        backend.label(),
        &cwd,
        &api,
        routing.is_some(),
        bearer.is_some(),
        session_id.as_deref(),
    );

    let structured = client.is_some() && session_id.is_some();
    let spec = Spec {
        mode,
        task: opts.task.clone(),
        cwd: cwd.clone(),
        routing,
        mcp,
        structured,
        preset_session: preset_session.clone(),
        resume: resume_handle.clone(),
        passthrough: opts.passthrough.clone(),
    };
    let launch = backend.build(&spec)?;

    // The session id we watch for the interactive transcript tail.
    let watch_sid = resume_handle.clone().or(preset_session);

    match mode {
        Mode::Headless => {
            let (outcome, ok) = run_headless(&*backend, structured, launch, client.clone(), session_id.clone()).await?;
            if let (Some(c), Some(id)) = (&client, &session_id) {
                let transcript = outcome
                    .backend_session
                    .as_ref()
                    .and_then(|bs| backend.transcript_path(&cwd, bs))
                    .map(|p| p.display().to_string());
                finalize(c, id, &outcome, ok, &snapshot, &api, false, transcript).await;
                report_link(id);
            }
        }
        Mode::Interactive => {
            let ok = run_interactive(&*backend, launch, client.clone(), session_id.clone(), &cwd, watch_sid).await?;
            if let (Some(c), Some(id)) = (&client, &session_id) {
                // Interactive per-event stream arrives via the transcript tail;
                // the resume handle here is what we pre-set / resumed.
                let bs = resume_handle.or_else(|| preset_session_of(&spec));
                let outcome = Outcome { backend_session: bs, ..Default::default() };
                let transcript = outcome
                    .backend_session
                    .as_ref()
                    .and_then(|s| backend.transcript_path(&cwd, s))
                    .map(|p| p.display().to_string());
                finalize(c, id, &outcome, ok, &snapshot, &api, true, transcript).await;
                report_link(id);
            }
        }
    }
    Ok(())
}

fn preset_session_of(spec: &Spec) -> Option<String> {
    spec.preset_session.clone()
}

// ---- cloud session resolution ----

/// Resolve the cloud session id for this run. Fresh runs register a new session.
/// A resume reuses the SAME id when the prior session is still live (running/
/// paused) — cloud forbids reopening a terminal one — otherwise it forks a new
/// session that records the id it was `resumedFrom` (lineage).
pub(crate) async fn resolve_cloud_session(
    client: &SessionClient,
    agent: &str,
    title: &str,
    resume_from: Option<&str>,
) -> Result<(String, Option<String>)> {
    if let Some(old) = resume_from {
        if let Ok(info) = client.get(old).await {
            if !info.is_terminal() {
                // Same-id re-attach: move the live session back to running.
                let _ = client.set_status(old, Status::Running).await;
                return Ok((old.to_string(), None));
            }
        }
        let reg = client.register(agent, title).await?;
        return Ok((reg.id, Some(old.to_string())));
    }
    let reg = client.register(agent, title).await?;
    Ok((reg.id, None))
}

// ---- streaming ----

#[derive(Debug, Default, Clone)]
pub(crate) struct Outcome {
    pub backend_session: Option<String>,
    pub usage: Usage,
    pub saw_error: bool,
    pub final_summary: Option<String>,
}

/// The forward+render sink for a structured event stream. Forwarding fires ONLY
/// when a cloud client AND session id are present — the privacy gate is
/// structural: an unlinked run cannot reach the network from here.
struct Sink {
    client: Option<SessionClient>,
    session_id: Option<String>,
    render: bool,
    out: Outcome,
}

impl Sink {
    async fn handle(&mut self, m: Mapped) {
        match m {
            Mapped::Event { kind, payload } => {
                if self.render {
                    render_event(kind, &payload);
                }
                if let (Some(c), Some(id)) = (&self.client, &self.session_id) {
                    if let Err(e) = c.event(id, kind, payload).await {
                        warn(&format!("stream event dropped: {e}"));
                    }
                }
            }
            Mapped::BackendSession(id) => {
                self.out.backend_session.get_or_insert(id);
            }
            Mapped::Usage(u) => self.out.usage.merge(u),
            Mapped::Terminal { ok, summary } => {
                if !ok {
                    self.out.saw_error = true;
                }
                if summary.is_some() {
                    self.out.final_summary = summary;
                }
            }
        }
    }
}

/// Drive a backend's structured line stream through parse → forward/render.
pub(crate) async fn run_stream<R: AsyncBufRead + Unpin>(
    backend: &dyn Backend,
    reader: R,
    client: Option<SessionClient>,
    session_id: Option<String>,
    render: bool,
) -> Result<Outcome> {
    let mut sink = Sink { client, session_id, render, out: Outcome::default() };
    let mut lines = reader.lines();
    while let Some(line) = lines.next_line().await.context("reading backend stream")? {
        for m in backend.parse(&line) {
            sink.handle(m).await;
        }
    }
    Ok(sink.out)
}

// ---- finalize ----

/// Close out a linked session: record usage, persist + mirror the resume handle,
/// and set the terminal/suspended status. Interactive runs suspend to `paused`
/// (resumable, same id); headless task runs complete to `done`; a failure is
/// `error`. All cloud writes are best-effort (a hiccup never crashes the CLI).
#[allow(clippy::too_many_arguments)]
pub(crate) async fn finalize(
    client: &SessionClient,
    session_id: &str,
    outcome: &Outcome,
    ok: bool,
    snapshot: &Snapshot,
    api: &str,
    interactive: bool,
    transcript_path: Option<String>,
) {
    if !outcome.usage.is_empty() {
        let mut p = serde_json::to_value(&outcome.usage).unwrap_or_else(|_| json!({}));
        p["type"] = json!("usage");
        let _ = client.event(session_id, Kind::Log, p).await;
    }

    if let Some(bs) = &outcome.backend_session {
        let rec = ResumeRecord {
            cloud_session_id: session_id.to_string(),
            backend: snapshot.backend.clone(),
            backend_session_id: bs.clone(),
            cwd: snapshot.cwd.clone(),
            api: api.to_string(),
            machine_id: snapshot.machine_id.clone(),
            repo: snapshot.repo.clone(),
            transcript_path,
            created_at: now(),
        };
        let _ = rec.save();
        let _ = client.event(session_id, Kind::Status, rec.resume_payload()).await;
    }

    let status = if !ok {
        Status::Error
    } else if interactive {
        Status::Paused
    } else {
        Status::Done
    };
    let _ = client.set_status(session_id, status).await;
}

// ---- process execution ----

async fn run_headless(
    backend: &dyn Backend,
    structured: bool,
    launch: Launch,
    client: Option<SessionClient>,
    session_id: Option<String>,
) -> Result<(Outcome, bool)> {
    let Launch { mut command, cleanup } = launch;
    command.stdin(Stdio::inherit()).stderr(Stdio::inherit());
    if structured {
        command.stdout(Stdio::piped());
    } else {
        command.stdout(Stdio::inherit());
    }
    let mut child = command.spawn().map_err(spawn_err)?;

    let outcome = if structured {
        let stdout = child.stdout.take().expect("piped stdout");
        let reader = tokio::io::BufReader::new(stdout);
        run_stream(backend, reader, client, session_id, true).await?
    } else {
        Outcome::default()
    };
    let status = child.wait().await.context("waiting for backend")?;
    drop(cleanup);
    Ok((outcome, status.success()))
}

async fn run_interactive(
    backend: &dyn Backend,
    launch: Launch,
    client: Option<SessionClient>,
    session_id: Option<String>,
    cwd: &Path,
    watch_sid: Option<String>,
) -> Result<bool> {
    let Launch { mut command, cleanup } = launch;
    command.stdin(Stdio::inherit()).stdout(Stdio::inherit()).stderr(Stdio::inherit());
    let mut child = command.spawn().map_err(spawn_err)?;

    // Linked interactive per-event streaming rides the backend transcript tail.
    let stop = Arc::new(AtomicBool::new(false));
    let tail = match (&client, &session_id, &watch_sid) {
        (Some(c), Some(id), Some(sid)) => backend.transcript_path(cwd, sid).map(|path| {
            tokio::spawn(tail_transcript(path, c.clone(), id.clone(), stop.clone()))
        }),
        _ => None,
    };

    let status = child.wait().await.context("waiting for backend")?;
    stop.store(true, Ordering::Relaxed);
    if let Some(h) = tail {
        let _ = h.await;
    }
    drop(cleanup);
    Ok(status.success())
}

/// Follow a Claude transcript JSONL, forwarding newly-appended events to the
/// linked session. Best-effort: parse/forward failures are ignored so the live
/// TUI is never disturbed.
async fn tail_transcript(path: PathBuf, client: SessionClient, session_id: String, stop: Arc<AtomicBool>) {
    let mut pos: u64 = 0;
    let mut buf = String::new();
    loop {
        if let Ok(mut f) = tokio::fs::File::open(&path).await {
            let len = f.metadata().await.map(|m| m.len()).unwrap_or(0);
            if len > pos {
                if f.seek(std::io::SeekFrom::Start(pos)).await.is_ok() {
                    let mut chunk = String::new();
                    if f.read_to_string(&mut chunk).await.is_ok() {
                        pos = len;
                        buf.push_str(&chunk);
                        while let Some(nl) = buf.find('\n') {
                            let line: String = buf.drain(..=nl).collect();
                            for m in claude::Claude.parse(line.trim_end()) {
                                if let Mapped::Event { kind, payload } = m {
                                    let _ = client.event(&session_id, kind, payload).await;
                                }
                            }
                        }
                    }
                }
            }
        }
        if stop.load(Ordering::Relaxed) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(400)).await;
    }
}

// ---- presentation ----

#[allow(clippy::too_many_arguments)]
fn banner(
    opts: &Options,
    backend: &str,
    cwd: &Path,
    api: &str,
    routing: bool,
    signed_in: bool,
    session: Option<&str>,
) {
    println!(
        "{} {} · {} · {}",
        "hanzo code".bold(),
        backend.cyan(),
        cwd.display().to_string().dimmed(),
        opts.resume.as_deref().map(|_| "resume").unwrap_or("start").dimmed(),
    );
    let route_line = if routing {
        format!("routing: {} (usage metered to your org)", api.trim_start_matches("https://"))
            .green()
            .to_string()
    } else if !opts.route {
        "routing: off (--no-route; using the backend's own model account)".dimmed().to_string()
    } else if !signed_in {
        "routing: off (sign in with `hanzo login` to meter usage universally)".dimmed().to_string()
    } else {
        "routing: off".dimmed().to_string()
    };
    println!("  {route_line}");
    match session {
        Some(id) => println!("  {}", format!("link: on → {id}").green()),
        None => println!("  {}", "link: off (local only; pass --link to stream to Hanzo cloud)".dimmed()),
    }
}

fn report_link(id: &str) {
    println!("{}", format!("session {id} recorded — resume with `hanzo code --resume {id}`").dimmed());
}

fn render_event(kind: Kind, payload: &Value) {
    match kind {
        Kind::Message => {
            if let Some(t) = payload.get("text").and_then(Value::as_str) {
                if payload.get("role").and_then(Value::as_str) == Some("assistant") {
                    println!("{t}");
                }
            }
        }
        Kind::ToolCall => {
            let name = payload.get("name").and_then(Value::as_str).unwrap_or("tool");
            let brief = payload
                .get("input")
                .map(one_line)
                .unwrap_or_default();
            println!("{}", format!("→ {name} {brief}").dimmed());
        }
        Kind::Spawn => {
            let a = payload.get("agent").and_then(Value::as_str).unwrap_or("agent");
            println!("{}", format!("⇒ spawn {a}").dimmed());
        }
        Kind::Log => {
            match payload.get("type").and_then(Value::as_str) {
                Some("tool-result") => {
                    let n = payload.get("output").and_then(Value::as_str).map(|s| s.len()).unwrap_or(0);
                    println!("{}", format!("← result ({n} bytes)").dimmed());
                }
                Some("reasoning") => {
                    if let Some(t) = payload.get("text").and_then(Value::as_str) {
                        println!("{}", format!("· {}", first_line(t)).dimmed());
                    }
                }
                _ => {
                    if let Some(t) = payload.get("text").and_then(Value::as_str) {
                        println!("{}", t.dimmed());
                    }
                }
            }
        }
        Kind::Status | Kind::Control => {}
    }
}

fn one_line(v: &Value) -> String {
    let s = match v {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    };
    first_line(&s)
}

fn first_line(s: &str) -> String {
    let line = s.lines().next().unwrap_or("");
    if line.len() > 100 {
        format!("{}…", &line[..100])
    } else {
        line.to_string()
    }
}

fn session_title(opts: &Options) -> String {
    match &opts.task {
        Some(t) => t.chars().take(120).collect(),
        None => "interactive coding session".to_string(),
    }
}

fn warn(msg: &str) {
    eprintln!("{} {}", "warning:".yellow().bold(), msg);
}

fn spawn_err(e: std::io::Error) -> anyhow::Error {
    anyhow!("failed to launch the coding backend ({e}) — is it installed and on PATH?")
}

fn now() -> i64 {
    chrono::Utc::now().timestamp()
}

fn uuid_v4() -> String {
    let mut b = [0u8; 16];
    rand::RngCore::fill_bytes(&mut rand::rngs::OsRng, &mut b);
    b[6] = (b[6] & 0x0f) | 0x40; // version 4
    b[8] = (b[8] & 0x3f) | 0x80; // variant 10xx
    let h: Vec<String> = b.iter().map(|x| format!("{x:02x}")).collect();
    format!(
        "{}{}{}{}-{}{}-{}{}-{}{}-{}{}{}{}{}{}",
        h[0], h[1], h[2], h[3], h[4], h[5], h[6], h[7], h[8], h[9], h[10], h[11], h[12], h[13], h[14], h[15]
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::code::claude::Claude;
    use crate::commands::code::dev::Dev;
    use crate::commands::code::testmock::MockCloud;
    use tokio::io::AsyncWriteExt;

    async fn reader_of(fixture: &str) -> impl AsyncBufRead + Unpin {
        let (r, mut w) = tokio::io::duplex(1 << 20);
        w.write_all(fixture.as_bytes()).await.unwrap();
        drop(w); // EOF
        tokio::io::BufReader::new(r)
    }

    #[test]
    fn link_gate_no_link_wins_then_link_then_persisted() {
        assert!(!effective_link(true, true, true)); // --no-link overrides everything
        assert!(effective_link(true, false, false)); // --link
        assert!(effective_link(false, false, true)); // persisted default
        assert!(!effective_link(false, false, false)); // default off
    }

    #[tokio::test]
    async fn linked_stream_forwards_mapped_events_with_correct_kinds() {
        let mock = MockCloud::start().await;
        let client = SessionClient::new(&mock.base_url(), "T").unwrap();
        let fixture = concat!(
            r#"{"type":"system","subtype":"init","session_id":"sid-1","model":"m"}"#, "\n",
            r#"{"type":"assistant","message":{"content":[{"type":"text","text":"hi"},{"type":"tool_use","id":"t","name":"Bash","input":{"command":"ls"}}]}}"#, "\n",
            r#"{"type":"result","subtype":"success","is_error":false,"usage":{"input_tokens":5,"output_tokens":2},"total_cost_usd":0.01,"result":"ok"}"#, "\n"
        );
        let reader = reader_of(fixture).await;
        let out = run_stream(&Claude, reader, Some(client), Some("sess_1".into()), false).await.unwrap();

        assert_eq!(out.backend_session.as_deref(), Some("sid-1"));
        assert_eq!(out.usage.input_tokens, Some(5));
        assert!(!out.saw_error);

        let kinds: Vec<String> = mock
            .requests()
            .iter()
            .filter(|r| r.path == "/v1/agents/sessions/sess_1/events")
            .map(|r| r.json()["kind"].as_str().unwrap_or("").to_string())
            .collect();
        // session-start log, assistant message, tool-call.
        assert!(kinds.contains(&"log".to_string()));
        assert!(kinds.contains(&"message".to_string()));
        assert!(kinds.contains(&"tool-call".to_string()));
    }

    #[tokio::test]
    async fn unlinked_stream_forwards_nothing_even_with_cloud_available() {
        // A cloud IS listening, but with no client the stream cannot reach it.
        let mock = MockCloud::start().await;
        let fixture = concat!(
            r#"{"type":"system","subtype":"init","session_id":"sid","model":"m"}"#, "\n",
            r#"{"type":"assistant","message":{"content":[{"type":"text","text":"secret code here"}]}}"#, "\n"
        );
        let reader = reader_of(fixture).await;
        let out = run_stream(&Claude, reader, None, None, false).await.unwrap();
        assert_eq!(out.backend_session.as_deref(), Some("sid")); // parsed locally
        assert!(mock.requests().is_empty(), "unlinked run must not send anything to cloud");
    }

    #[tokio::test]
    async fn dev_stream_maps_and_forwards() {
        let mock = MockCloud::start().await;
        let client = SessionClient::new(&mock.base_url(), "T").unwrap();
        let fixture = concat!(
            r#"{"type":"thread.started","thread_id":"th-9"}"#, "\n",
            r#"{"type":"item.completed","item":{"id":"i","type":"command_execution","command":"go build","aggregated_output":"ok","exit_code":0,"status":"completed"}}"#, "\n",
            r#"{"type":"turn.completed","usage":{"input_tokens":3,"output_tokens":1,"cached_input_tokens":0}}"#, "\n"
        );
        let reader = reader_of(fixture).await;
        let out = run_stream(&Dev, reader, Some(client), Some("sess_2".into()), false).await.unwrap();
        assert_eq!(out.backend_session.as_deref(), Some("th-9"));
        assert_eq!(out.usage.output_tokens, Some(1));
        let has_toolcall = mock
            .requests()
            .iter()
            .any(|r| r.path.ends_with("/events") && r.json()["kind"] == "tool-call");
        assert!(has_toolcall);
    }

    #[tokio::test]
    async fn resolve_fresh_registers_a_new_session() {
        let mock = MockCloud::start().await;
        let client = SessionClient::new(&mock.base_url(), "T").unwrap();
        let (id, forked) = resolve_cloud_session(&client, "claude", "t", None).await.unwrap();
        assert_eq!(id, "sess_mock");
        assert!(forked.is_none());
        assert!(mock.requests().iter().any(|r| r.method == "POST" && r.path == "/v1/agents/sessions"));
    }

    #[tokio::test]
    async fn resume_nonterminal_reuses_same_id_without_registering() {
        let mock = MockCloud::start_get_status("paused").await;
        let client = SessionClient::new(&mock.base_url(), "T").unwrap();
        let (id, forked) = resolve_cloud_session(&client, "claude", "t", Some("sess_old")).await.unwrap();
        assert_eq!(id, "sess_old", "must re-attach the SAME id");
        assert!(forked.is_none());
        let reqs = mock.requests();
        assert!(reqs.iter().any(|r| r.method == "GET" && r.path == "/v1/agents/sessions/sess_old"));
        assert!(reqs.iter().any(|r| r.method == "PATCH" && r.json()["status"] == "running"));
        assert!(!reqs.iter().any(|r| r.method == "POST" && r.path == "/v1/agents/sessions"));
    }

    #[tokio::test]
    async fn resume_terminal_forks_a_new_session_with_lineage() {
        let mock = MockCloud::start_get_status("done").await;
        let client = SessionClient::new(&mock.base_url(), "T").unwrap();
        let (id, forked) = resolve_cloud_session(&client, "claude", "t", Some("sess_old")).await.unwrap();
        assert_eq!(id, "sess_mock");
        assert_eq!(forked.as_deref(), Some("sess_old"));
        assert!(mock.requests().iter().any(|r| r.method == "POST" && r.path == "/v1/agents/sessions"));
    }

    #[tokio::test]
    async fn finalize_reports_usage_resume_handle_and_terminal_status() {
        let mock = MockCloud::start().await;
        let client = SessionClient::new(&mock.base_url(), "T").unwrap();
        let snapshot = Snapshot {
            machine_id: "m".into(),
            host: "h".into(),
            os: "linux".into(),
            arch: "x86_64".into(),
            cwd: "/w".into(),
            backend: "claude".into(),
            backend_version: None,
            repo: Default::default(),
        };
        let outcome = Outcome {
            backend_session: Some("sid-x".into()),
            usage: Usage { input_tokens: Some(9), ..Default::default() },
            saw_error: false,
            final_summary: None,
        };
        finalize(&client, "sess_9", &outcome, true, &snapshot, "https://api.hanzo.ai", false, None).await;

        let reqs = mock.requests();
        // usage log event
        assert!(reqs.iter().any(|r| r.path.ends_with("/events") && r.json()["kind"] == "log" && r.json()["payload"]["type"] == "usage"));
        // resume-handle status event
        assert!(reqs.iter().any(|r| r.path.ends_with("/events") && r.json()["kind"] == "status" && r.json()["payload"]["type"] == "resume" && r.json()["payload"]["backendSessionId"] == "sid-x"));
        // terminal status
        assert!(reqs.iter().any(|r| r.method == "PATCH" && r.json()["status"] == "done"));
        // clean up the resume record this finalize persisted
        let _ = std::fs::remove_file(
            dirs::data_local_dir().unwrap().join("hanzo/code/sessions/sess_9.json"),
        );
    }

    #[tokio::test]
    async fn finalize_interactive_suspends_to_paused_and_failure_is_error() {
        let mock = MockCloud::start().await;
        let client = SessionClient::new(&mock.base_url(), "T").unwrap();
        let snapshot = Snapshot {
            machine_id: "m".into(), host: "h".into(), os: "linux".into(), arch: "x".into(),
            cwd: "/w".into(), backend: "dev".into(), backend_version: None, repo: Default::default(),
        };
        let outcome = Outcome::default();
        finalize(&client, "sess_i", &outcome, true, &snapshot, "https://api.hanzo.ai", true, None).await;
        assert!(mock.requests().iter().any(|r| r.method == "PATCH" && r.json()["status"] == "paused"));

        let mock2 = MockCloud::start().await;
        let client2 = SessionClient::new(&mock2.base_url(), "T").unwrap();
        finalize(&client2, "sess_e", &outcome, false, &snapshot, "https://api.hanzo.ai", true, None).await;
        assert!(mock2.requests().iter().any(|r| r.method == "PATCH" && r.json()["status"] == "error"));
    }

    #[test]
    fn uuid_is_v4_shaped() {
        let u = uuid_v4();
        assert_eq!(u.len(), 36);
        assert_eq!(u.as_bytes()[14], b'4'); // version nibble
        assert!(matches!(u.as_bytes()[19], b'8' | b'9' | b'a' | b'b'));
    }

    /// A backend whose `build` spawns a REAL child (`cat` of a fixture file) so
    /// the full spawn → pipe stdout → parse → forward path is exercised without a
    /// live `claude`/`dev` binary.
    struct FakeBackend {
        fixture: String,
    }

    impl Backend for FakeBackend {
        fn label(&self) -> &'static str {
            "claude"
        }
        fn version(&self) -> Option<String> {
            None
        }
        fn build(&self, _spec: &Spec) -> Result<Launch> {
            use std::io::Write;
            let mut f = tempfile::Builder::new().suffix(".jsonl").tempfile().unwrap();
            f.write_all(self.fixture.as_bytes()).unwrap();
            let path = f.into_temp_path();
            let mut command = tokio::process::Command::new("cat");
            command.arg(&*path);
            Ok(Launch { command, cleanup: vec![path] })
        }
        fn parse(&self, line: &str) -> Vec<Mapped> {
            Claude.parse(line)
        }
        fn transcript_path(&self, _: &Path, _: &str) -> Option<PathBuf> {
            None
        }
    }

    fn dummy_spec() -> Spec {
        Spec {
            mode: Mode::Headless,
            task: Some("t".into()),
            cwd: PathBuf::from("."),
            routing: None,
            mcp: None,
            structured: true,
            preset_session: None,
            resume: None,
            passthrough: vec![],
        }
    }

    #[tokio::test]
    async fn run_headless_spawns_a_real_child_and_forwards_end_to_end() {
        let mock = MockCloud::start().await;
        let client = SessionClient::new(&mock.base_url(), "T").unwrap();
        let fixture = concat!(
            r#"{"type":"system","subtype":"init","session_id":"sid-1","model":"m"}"#, "\n",
            r#"{"type":"assistant","message":{"content":[{"type":"text","text":"hi"},{"type":"tool_use","id":"t","name":"Bash","input":{"command":"ls"}}]}}"#, "\n",
            r#"{"type":"result","subtype":"success","is_error":false,"usage":{"input_tokens":5,"output_tokens":2},"result":"ok"}"#, "\n"
        );
        let fake = FakeBackend { fixture: fixture.into() };
        let launch = fake.build(&dummy_spec()).unwrap();
        let (out, ok) =
            run_headless(&fake, true, launch, Some(client), Some("sess_e2e".into())).await.unwrap();

        assert!(ok, "child exited zero");
        assert_eq!(out.backend_session.as_deref(), Some("sid-1"));
        assert_eq!(out.usage.input_tokens, Some(5));
        let kinds: Vec<String> = mock
            .requests()
            .iter()
            .filter(|r| r.path == "/v1/agents/sessions/sess_e2e/events")
            .map(|r| r.json()["kind"].as_str().unwrap_or("").to_string())
            .collect();
        assert!(kinds.contains(&"message".to_string()));
        assert!(kinds.contains(&"tool-call".to_string()));
    }

    #[tokio::test]
    async fn tail_forwards_appended_transcript_lines() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.jsonl");
        std::fs::write(&path, "").unwrap();
        let mock = MockCloud::start().await;
        let client = SessionClient::new(&mock.base_url(), "T").unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let handle = tokio::spawn(tail_transcript(path.clone(), client, "sess_t".into(), stop.clone()));

        tokio::time::sleep(Duration::from_millis(120)).await;
        {
            use std::io::Write;
            let mut f = std::fs::OpenOptions::new().append(true).open(&path).unwrap();
            writeln!(
                f,
                r#"{{"type":"assistant","message":{{"content":[{{"type":"text","text":"live"}}]}}}}"#
            )
            .unwrap();
        }
        tokio::time::sleep(Duration::from_millis(950)).await;
        stop.store(true, Ordering::Relaxed);
        let _ = handle.await;

        assert!(mock
            .requests()
            .iter()
            .any(|r| r.path == "/v1/agents/sessions/sess_t/events" && r.json()["kind"] == "message"));
    }
}
