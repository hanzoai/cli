//! `hanzo dev --serve` — local hot-reloading Vite dev server with integrated `/v1/build`,
//! `/v1/chat/completions` local LLM proxy, `/v1/bot/ask` local HanzoBot bridge, and
//! `/v1/dev/status` live local-stack telemetry.
//!
//! Vite owns module graph transformation and `<50ms` HMR over WebSockets (`Upgrade: websocket`);
//! this Tokio edge sits in front on `--port` (default `5173`), passes browser HTTP & HMR
//! WebSocket streams straight through to Vite, and serves `/v1/build`, `/v1/chat/completions`,
//! `/v1/models`, `/v1/messages`, `/v1/bot/ask`, and `/v1/dev/status` natively on the same origin.

use anyhow::{anyhow, bail, Context, Result};
use colored::*;
use hanzo_client::Method;
use rand::RngCore;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::RwLock;

use crate::commands::product::Seam;
use crate::config::Config;
use crate::iam::paths::DEFAULT_BRAND;

/// Parsed build configuration from `hanzo.yml` or `package.json`.
#[derive(Clone, Debug)]
pub struct WebManifest {
    pub install_cmd: String,
    pub build_cmd: String,
    pub output_dir: String,
}

/// Receipt recorded for every local or cloud `/v1/build` job.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BuildReceipt {
    pub build_job_id: String,
    pub mode: String,
    pub status: String,
    pub command: String,
    pub output_dir: String,
    pub started_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub digest: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub files: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default)]
    pub logs: String,
}

struct ServerState {
    project_dir: PathBuf,
    manifest: WebManifest,
    vite_port: u16,
    engine_url: String,
    cfg: Config,
    builds: RwLock<HashMap<String, BuildReceipt>>,
    build_order: RwLock<Vec<String>>,
    http: reqwest::Client,
}

struct ChildGuard(Option<Child>);

impl Drop for ChildGuard {
    fn drop(&mut self) {
        if let Some(mut child) = self.0.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

/// Parse `hanzo.yml` (`build.install`, `build.command`, `build.output`) when present,
/// falling back to lockfile-aware defaults (`pnpm` -> `npm`).
pub fn load_manifest(project_dir: &Path) -> WebManifest {
    let default_pm = if project_dir.join("pnpm-lock.yaml").is_file() {
        "pnpm"
    } else if project_dir.join("bun.lockb").is_file() || project_dir.join("bun.lock").is_file() {
        "bun"
    } else {
        "npm"
    };
    let mut install_cmd = match default_pm {
        "pnpm" => "pnpm install".to_string(),
        "bun" => "bun install".to_string(),
        _ => {
            if project_dir.join("package-lock.json").is_file() {
                "npm ci".to_string()
            } else {
                "npm install".to_string()
            }
        }
    };
    let mut build_cmd = format!("{default_pm} run build");
    let mut output_dir = "dist".to_string();

    let yml_path = project_dir.join("hanzo.yml");
    if let Ok(raw) = std::fs::read_to_string(&yml_path) {
        let mut in_build = false;
        for line in raw.lines() {
            let trimmed = line.trim();
            if trimmed.is_empty() || trimmed.starts_with('#') {
                continue;
            }
            if !line.starts_with(' ') && !line.starts_with('\t') {
                in_build = trimmed == "build:";
                continue;
            }
            if in_build {
                if let Some((k, v)) = trimmed.split_once(':') {
                    let val = v
                        .split('#')
                        .next()
                        .unwrap_or("")
                        .trim()
                        .trim_matches('"')
                        .trim_matches('\'');
                    if val.is_empty() {
                        continue;
                    }
                    match k.trim() {
                        "install" => install_cmd = val.to_string(),
                        "command" => build_cmd = val.to_string(),
                        "output" => output_dir = val.to_string(),
                        _ => {}
                    }
                }
            }
        }
    }

    WebManifest {
        install_cmd,
        build_cmd,
        output_dir,
    }
}

fn ensure_dependencies(project_dir: &Path, manifest: &WebManifest) -> Result<()> {
    if !project_dir.join("package.json").is_file() || project_dir.join("node_modules").is_dir() {
        return Ok(());
    }
    println!(
        "{} Installing project dependencies (`{}`)...",
        "→".cyan(),
        manifest.install_cmd
    );
    let status = Command::new("sh")
        .args(["-c", &manifest.install_cmd])
        .current_dir(project_dir)
        .status()
        .with_context(|| format!("running `{}`", manifest.install_cmd))?;
    if !status.success() {
        bail!("dependency install `{}` exited with {status}", manifest.install_cmd);
    }
    Ok(())
}

/// Compute a deterministic SHA-256 over all files inside `dir` (sorted by relative path),
/// returning `(sha256_prefixed, file_count, total_bytes)`.
pub fn hash_output_dir(dir: &Path) -> Result<(String, usize, u64)> {
    if !dir.is_dir() {
        bail!("build output directory `{}` does not exist", dir.display());
    }
    let mut entries: Vec<PathBuf> = Vec::new();
    collect_files(dir, &mut entries)?;
    entries.sort();

    let mut hasher = Sha256::new();
    let mut total_bytes = 0u64;
    for path in &entries {
        let rel = path
            .strip_prefix(dir)
            .unwrap_or(path)
            .to_string_lossy()
            .replace('\\', "/");
        hasher.update(rel.as_bytes());
        hasher.update([0u8]);
        let bytes = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
        total_bytes += bytes.len() as u64;
        hasher.update((bytes.len() as u64).to_le_bytes());
        hasher.update(&bytes);
    }
    let digest = format!("sha256:{:x}", hasher.finalize());
    Ok((digest, entries.len(), total_bytes))
}

fn collect_files(dir: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
    for entry in std::fs::read_dir(dir).with_context(|| format!("listing {}", dir.display()))? {
        let entry = entry?;
        let p = entry.path();
        let meta = std::fs::symlink_metadata(&p)?;
        if meta.is_dir() {
            collect_files(&p, out)?;
        } else if meta.is_file() {
            out.push(p);
        }
    }
    Ok(())
}

fn new_build_id() -> String {
    let mut raw = [0u8; 9];
    rand::rngs::OsRng.fill_bytes(&mut raw);
    use base64::Engine;
    format!(
        "bld_{}",
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(raw)
    )
}

fn persist_receipt(project_dir: &Path, receipt: &BuildReceipt) {
    let dir = project_dir.join(".hanzo").join("builds");
    if std::fs::create_dir_all(&dir).is_ok() {
        if let Ok(payload) = serde_json::to_vec_pretty(receipt) {
            let _ = std::fs::write(dir.join(format!("{}.json", receipt.build_job_id)), &payload);
            let _ = std::fs::write(dir.join("latest.json"), &payload);
        }
    }
}

/// Execute a single synchronous web/static `/v1/build` pass (`hanzo build --web` and
/// integrated `POST /v1/build` worker).
pub fn execute_local_build(
    project_dir: &Path,
    manifest: &WebManifest,
    build_job_id: String,
) -> BuildReceipt {
    let started_wall = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let started = Instant::now();
    let output = Command::new("sh")
        .args(["-c", &manifest.build_cmd])
        .current_dir(project_dir)
        .output();

    let finished_wall = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let duration_ms = started.elapsed().as_millis() as u64;

    let receipt = match output {
        Ok(out) => {
            let mut logs = String::from_utf8_lossy(&out.stdout).into_owned();
            let stderr = String::from_utf8_lossy(&out.stderr);
            if !stderr.trim().is_empty() {
                if !logs.ends_with('\n') && !logs.is_empty() {
                    logs.push('\n');
                }
                logs.push_str(&stderr);
            }
            if out.status.success() {
                let out_path = project_dir.join(&manifest.output_dir);
                match hash_output_dir(&out_path) {
                    Ok((digest, files, bytes)) => BuildReceipt {
                        build_job_id,
                        mode: "local-v1-build".to_string(),
                        status: "succeeded".to_string(),
                        command: manifest.build_cmd.clone(),
                        output_dir: manifest.output_dir.clone(),
                        started_at: started_wall,
                        finished_at: Some(finished_wall),
                        duration_ms: Some(duration_ms),
                        digest: Some(digest),
                        files: Some(files),
                        bytes: Some(bytes),
                        error: None,
                        logs,
                    },
                    Err(err) => BuildReceipt {
                        build_job_id,
                        mode: "local-v1-build".to_string(),
                        status: "failed".to_string(),
                        command: manifest.build_cmd.clone(),
                        output_dir: manifest.output_dir.clone(),
                        started_at: started_wall,
                        finished_at: Some(finished_wall),
                        duration_ms: Some(duration_ms),
                        digest: None,
                        files: None,
                        bytes: None,
                        error: Some(err.to_string()),
                        logs,
                    },
                }
            } else {
                BuildReceipt {
                    build_job_id,
                    mode: "local-v1-build".to_string(),
                    status: "failed".to_string(),
                    command: manifest.build_cmd.clone(),
                    output_dir: manifest.output_dir.clone(),
                    started_at: started_wall,
                    finished_at: Some(finished_wall),
                    duration_ms: Some(duration_ms),
                    digest: None,
                    files: None,
                    bytes: None,
                    error: Some(format!("`{}` exited with {}", manifest.build_cmd, out.status)),
                    logs,
                }
            }
        }
        Err(err) => BuildReceipt {
            build_job_id,
            mode: "local-v1-build".to_string(),
            status: "failed".to_string(),
            command: manifest.build_cmd.clone(),
            output_dir: manifest.output_dir.clone(),
            started_at: started_wall,
            finished_at: Some(finished_wall),
            duration_ms: Some(duration_ms),
            digest: None,
            files: None,
            bytes: None,
            error: Some(err.to_string()),
            logs: String::new(),
        },
    };

    persist_receipt(project_dir, &receipt);
    receipt
}

/// CLI entrypoint for `hanzo build --web` — builds the `hanzo.yml` / Vite bundle locally
/// and prints the deterministic SHA-256 artifact receipt.
pub fn run_static_build(project_dir: &Path) -> Result<BuildReceipt> {
    let manifest = load_manifest(project_dir);
    ensure_dependencies(project_dir, &manifest)?;
    let id = new_build_id();
    println!(
        "{} Running `/v1/build` static pipeline (`{}`) [{}]...",
        "→".cyan(),
        manifest.build_cmd,
        id.bold()
    );
    let receipt = execute_local_build(project_dir, &manifest, id);
    if receipt.status != "succeeded" {
        if !receipt.logs.trim().is_empty() {
            eprintln!("{}", receipt.logs.trim());
        }
        bail!(
            "build {} failed: {}",
            receipt.build_job_id,
            receipt.error.as_deref().unwrap_or("unknown error")
        );
    }
    println!(
        "{} Built `{}/` in {} ms — {} files, {:.1} KiB ({})",
        "✓".green().bold(),
        receipt.output_dir,
        receipt.duration_ms.unwrap_or(0),
        receipt.files.unwrap_or(0),
        receipt.bytes.unwrap_or(0) as f64 / 1024.0,
        receipt.digest.as_deref().unwrap_or("-")
    );
    println!(
        "  Receipt: {}",
        project_dir
            .join(".hanzo/builds/latest.json")
            .display()
            .to_string()
            .dimmed()
    );
    Ok(receipt)
}

fn spawn_vite(project_dir: &Path, vite_port: u16) -> Result<Child> {
    let local_vite = project_dir.join("node_modules").join(".bin").join("vite");
    let port_str = vite_port.to_string();
    let mut cmd = if local_vite.is_file() {
        let mut c = Command::new(local_vite);
        c.args(["--host", "127.0.0.1", "--port", &port_str, "--strictPort"]);
        c
    } else if let Ok(npx) = which::which("npx") {
        let mut c = Command::new(npx);
        c.args([
            "--yes",
            "vite",
            "--host",
            "127.0.0.1",
            "--port",
            &port_str,
            "--strictPort",
        ]);
        c
    } else {
        bail!(
            "Vite binary not found in `{}/node_modules/.bin/vite` and `npx` is unavailable",
            project_dir.display()
        );
    };
    cmd.current_dir(project_dir)
        .stdin(Stdio::null())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());
    cmd.spawn().context("spawning Vite HMR server")
}

async fn wait_port_ready(port: u16, child: &mut Child, timeout: Duration) -> Result<()> {
    let deadline = Instant::now() + timeout;
    loop {
        if TcpStream::connect(("127.0.0.1", port)).await.is_ok() {
            return Ok(());
        }
        if let Some(status) = child.try_wait().context("checking Vite child status")? {
            bail!("Vite dev server exited prematurely ({status})");
        }
        if Instant::now() >= deadline {
            bail!("Vite dev server did not listen on 127.0.0.1:{port} within {}s", timeout.as_secs());
        }
        tokio::time::sleep(Duration::from_millis(80)).await;
    }
}

fn newest_mtime(dir: &Path) -> Option<SystemTime> {
    let mut latest: Option<SystemTime> = None;
    let entries = std::fs::read_dir(dir).ok()?;
    for entry in entries.flatten() {
        let p = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if matches!(name.as_ref(), "node_modules" | "dist" | ".git" | ".hanzo") {
            continue;
        }
        if let Ok(meta) = std::fs::symlink_metadata(&p) {
            if meta.is_dir() {
                if let Some(sub) = newest_mtime(&p) {
                    latest = Some(latest.map_or(sub, |cur| cur.max(sub)));
                }
            } else if meta.is_file() {
                if let Ok(m) = meta.modified() {
                    latest = Some(latest.map_or(m, |cur| cur.max(m)));
                }
            }
        }
    }
    latest
}

/// Start `hanzo dev --serve`: Vite HMR hot-reload server + integrated `/v1/build` +
/// local Zen-Nano LLM `/v1/chat/completions` proxy + `/v1/bot/ask` + `/v1/dev/status`.
pub async fn run(
    cfg: &Config,
    project_dir: PathBuf,
    port: u16,
    engine_url: String,
    build_on_change: bool,
) -> Result<()> {
    let project_dir = project_dir
        .canonicalize()
        .unwrap_or_else(|_| project_dir.clone());
    let manifest = load_manifest(&project_dir);
    ensure_dependencies(&project_dir, &manifest)?;

    let vite_port = std::net::TcpListener::bind("127.0.0.1:0")
        .and_then(|l| l.local_addr())
        .context("allocating ephemeral port for Vite upstream")?
        .port();

    let listener = TcpListener::bind(("0.0.0.0", port))
        .await
        .with_context(|| format!("binding hanzo dev server on 0.0.0.0:{port}"))?;

    let mut child = spawn_vite(&project_dir, vite_port)?;
    wait_port_ready(vite_port, &mut child, Duration::from_secs(25)).await?;
    let _guard = ChildGuard(Some(child));

    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(60))
        .build()?;

    let state = Arc::new(ServerState {
        project_dir: project_dir.clone(),
        manifest: manifest.clone(),
        vite_port,
        engine_url: engine_url.trim_end_matches('/').to_string(),
        cfg: cfg.clone(),
        builds: RwLock::new(HashMap::new()),
        build_order: RwLock::new(Vec::new()),
        http,
    });

    // Seed initial latest build receipt if `.hanzo/builds/latest.json` already exists on disk.
    if let Ok(bytes) = std::fs::read(project_dir.join(".hanzo/builds/latest.json")) {
        if let Ok(rec) = serde_json::from_slice::<BuildReceipt>(&bytes) {
            let id = rec.build_job_id.clone();
            state.builds.write().await.insert(id.clone(), rec);
            state.build_order.write().await.push(id);
        }
    }

    println!();
    println!(
        "  {} {}  {}",
        "▲".cyan().bold(),
        "hanzo dev".bold(),
        format!("http://127.0.0.1:{port}").cyan().underline()
    );
    println!(
        "    {} Vite HMR upstream     127.0.0.1:{vite_port} (WebSocket hot-reload active)",
        "•".green()
    );
    println!(
        "    {} Integrated build API  POST/GET http://127.0.0.1:{port}/v1/build",
        "•".green()
    );
    println!(
        "    {} Local LLM proxy       http://127.0.0.1:{port}/v1/chat/completions → {}",
        "•".green(),
        state.engine_url
    );
    println!(
        "    {} Local Bot bridge      POST http://127.0.0.1:{port}/v1/bot/ask",
        "•".green()
    );
    println!(
        "    {} Stack telemetry       GET  http://127.0.0.1:{port}/v1/dev/status",
        "•".green()
    );
    println!();

    if build_on_change {
        let watch_state = Arc::clone(&state);
        tokio::spawn(async move {
            let src_dir = watch_state.project_dir.join("src");
            let mut last = newest_mtime(&src_dir);
            loop {
                tokio::time::sleep(Duration::from_millis(1200)).await;
                let now = newest_mtime(&src_dir);
                if now.is_some() && now != last {
                    last = now;
                    let _ = trigger_local_build(Arc::clone(&watch_state), false).await;
                }
            }
        });
    }

    loop {
        tokio::select! {
            accept = listener.accept() => {
                let (stream, _) = accept.context("accepting connection")?;
                let st = Arc::clone(&state);
                tokio::spawn(async move {
                    let _ = handle_conn(stream, st).await;
                });
            }
            _ = tokio::signal::ctrl_c() => {
                println!("\n{} Stopping `hanzo dev --serve`...", "→".cyan());
                return Ok(());
            }
        }
    }
}

async fn trigger_local_build(state: Arc<ServerState>, wait: bool) -> BuildReceipt {
    let id = new_build_id();
    let initial = BuildReceipt {
        build_job_id: id.clone(),
        mode: "local-v1-build".to_string(),
        status: "building".to_string(),
        command: state.manifest.build_cmd.clone(),
        output_dir: state.manifest.output_dir.clone(),
        started_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        finished_at: None,
        duration_ms: None,
        digest: None,
        files: None,
        bytes: None,
        error: None,
        logs: String::new(),
    };
    {
        state.builds.write().await.insert(id.clone(), initial.clone());
        state.build_order.write().await.push(id.clone());
    }

    let st = Arc::clone(&state);
    let build_id = id.clone();
    let handle = tokio::task::spawn_blocking(move || {
        execute_local_build(&st.project_dir, &st.manifest, build_id)
    });

    if wait {
        match handle.await {
            Ok(done) => {
                state.builds.write().await.insert(id, done.clone());
                done
            }
            Err(err) => {
                let mut failed = initial;
                failed.status = "failed".to_string();
                failed.error = Some(err.to_string());
                state.builds.write().await.insert(id, failed.clone());
                failed
            }
        }
    } else {
        let st2 = Arc::clone(&state);
        tokio::spawn(async move {
            if let Ok(done) = handle.await {
                let tag = if done.status == "succeeded" {
                    "✓".green().to_string()
                } else {
                    "✗".red().to_string()
                };
                eprintln!(
                    "  {} [/v1/build {}] {} ({} ms)",
                    tag,
                    done.build_job_id,
                    done.status,
                    done.duration_ms.unwrap_or(0)
                );
                st2.builds.write().await.insert(id, done);
            }
        });
        initial
    }
}

struct ParsedHead {
    method: String,
    target: String,
    path: String,
    query: String,
    headers: Vec<(String, String)>,
    body_offset: usize,
    content_length: usize,
    is_upgrade: bool,
}

fn parse_http_head(buf: &[u8]) -> Option<ParsedHead> {
    let header_end = buf.windows(4).position(|w| w == b"\r\n\r\n")?;
    let head_str = std::str::from_utf8(&buf[..header_end]).ok()?;
    let mut lines = head_str.split("\r\n");
    let req_line = lines.next()?;
    let mut parts = req_line.split_whitespace();
    let method = parts.next()?.to_uppercase();
    let target = parts.next()?.to_string();
    let (path, query) = match target.split_once('?') {
        Some((p, q)) => (p.to_string(), q.to_string()),
        None => (target.clone(), String::new()),
    };

    let mut headers = Vec::new();
    let mut content_length = 0usize;
    let mut is_upgrade = false;
    for line in lines {
        if let Some((k, v)) = line.split_once(':') {
            let key = k.trim().to_string();
            let val = v.trim().to_string();
            if key.eq_ignore_ascii_case("content-length") {
                content_length = val.parse::<usize>().unwrap_or(0);
            }
            if key.eq_ignore_ascii_case("upgrade") {
                is_upgrade = true;
            }
            headers.push((key, val));
        }
    }

    Some(ParsedHead {
        method,
        target,
        path,
        query,
        headers,
        body_offset: header_end + 4,
        content_length,
        is_upgrade,
    })
}

fn is_integrated_api(path: &str) -> bool {
    path == "/v1/build"
        || path.starts_with("/v1/build/")
        || path == "/v1/dev/status"
        || path == "/__hanzo/status"
        || path == "/v1/models"
        || path == "/v1/chat/completions"
        || path == "/v1/messages"
        || path == "/v1/bot/ask"
}

/// Rewrite non-WebSocket HTTP requests forwarded to Vite with `Connection: close` so browser
/// keep-alive sockets never multiplex a subsequent `/v1/*` API call past this router.
pub fn rewrite_connection_close(buf: &[u8], body_offset: usize) -> Vec<u8> {
    let Ok(head_str) = std::str::from_utf8(&buf[..body_offset - 4]) else {
        return buf.to_vec();
    };
    let mut out = String::with_capacity(head_str.len() + 32);
    for (idx, line) in head_str.split("\r\n").enumerate() {
        if idx == 0 {
            out.push_str(line);
            out.push_str("\r\n");
            continue;
        }
        if let Some((k, _)) = line.split_once(':') {
            if k.trim().eq_ignore_ascii_case("connection") {
                continue;
            }
        }
        out.push_str(line);
        out.push_str("\r\n");
    }
    out.push_str("Connection: close\r\n\r\n");
    let mut bytes = out.into_bytes();
    bytes.extend_from_slice(&buf[body_offset..]);
    bytes
}

async fn write_json_response(stream: &mut TcpStream, status: u16, body: &Value) -> Result<()> {
    let reason = match status {
        200 => "OK",
        201 => "Created",
        202 => "Accepted",
        204 => "No Content",
        400 => "Bad Request",
        404 => "Not Found",
        502 => "Bad Gateway",
        _ => "Internal Server Error",
    };
    let payload = serde_json::to_vec_pretty(body)?;
    let head = format!(
        "HTTP/1.1 {status} {reason}\r\n\
         Content-Type: application/json; charset=utf-8\r\n\
         Content-Length: {}\r\n\
         Access-Control-Allow-Origin: *\r\n\
         Access-Control-Allow-Methods: GET, POST, OPTIONS\r\n\
         Access-Control-Allow-Headers: Content-Type, Authorization, x-api-key, anthropic-version\r\n\
         Cache-Control: no-store\r\n\
         Connection: close\r\n\r\n",
        payload.len()
    );
    stream.write_all(head.as_bytes()).await?;
    stream.write_all(&payload).await?;
    stream.flush().await?;
    Ok(())
}

async fn handle_conn(mut client: TcpStream, state: Arc<ServerState>) -> Result<()> {
    let mut buf = Vec::with_capacity(8192);
    let mut chunk = [0u8; 4096];
    let head = loop {
        let n = client.read(&mut chunk).await?;
        if n == 0 {
            return Ok(());
        }
        buf.extend_from_slice(&chunk[..n]);
        if let Some(parsed) = parse_http_head(&buf) {
            break parsed;
        }
        if buf.len() > 65536 {
            return Ok(());
        }
    };

    if !is_integrated_api(&head.path) {
        let mut upstream = TcpStream::connect(("127.0.0.1", state.vite_port)).await?;
        let to_send = if head.is_upgrade {
            buf
        } else {
            rewrite_connection_close(&buf, head.body_offset)
        };
        upstream.write_all(&to_send).await?;
        let _ = tokio::io::copy_bidirectional(&mut client, &mut upstream).await;
        return Ok(());
    }

    if head.method == "OPTIONS" {
        let cors = "HTTP/1.1 204 No Content\r\n\
                    Access-Control-Allow-Origin: *\r\n\
                    Access-Control-Allow-Methods: GET, POST, OPTIONS\r\n\
                    Access-Control-Allow-Headers: Content-Type, Authorization, x-api-key, anthropic-version\r\n\
                    Content-Length: 0\r\n\
                    Connection: close\r\n\r\n";
        client.write_all(cors.as_bytes()).await?;
        return Ok(());
    }

    // Read full request body for integrated `/v1/*` handlers.
    let needed = head.body_offset + head.content_length;
    while buf.len() < needed {
        let n = client.read(&mut chunk).await?;
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..n]);
    }
    let body_bytes = if buf.len() >= head.body_offset {
        &buf[head.body_offset..buf.len().min(needed)]
    } else {
        &[]
    };

    match (head.method.as_str(), head.path.as_str()) {
        ("POST", "/v1/build") => {
            let req_json: Value = if body_bytes.is_empty() {
                json!({})
            } else {
                serde_json::from_slice(body_bytes).unwrap_or_else(|_| json!({}))
            };
            let wait = head.query.split('&').any(|kv| kv == "wait=true" || kv == "wait=1")
                || req_json.get("wait").and_then(Value::as_bool).unwrap_or(false);
            let cloud = req_json.get("cloud").and_then(Value::as_bool).unwrap_or(false);

            if cloud {
                let mut cfg = state.cfg.clone();
                match Seam::open(&mut cfg, DEFAULT_BRAND).await {
                    Ok(seam) => match seam.send(Method::POST, "/v1/build", &[], Some(req_json)).await {
                        Ok(reply) => {
                            return write_json_response(&mut client, reply.status, &reply.body).await;
                        }
                        Err(err) => {
                            return write_json_response(
                                &mut client,
                                502,
                                &json!({ "error": err.to_string() }),
                            )
                            .await;
                        }
                    },
                    Err(err) => {
                        return write_json_response(
                            &mut client,
                            502,
                            &json!({ "error": err.to_string() }),
                        )
                        .await;
                    }
                }
            }

            let receipt = trigger_local_build(Arc::clone(&state), wait).await;
            let code = if wait {
                if receipt.status == "succeeded" {
                    200
                } else {
                    500
                }
            } else {
                202
            };
            write_json_response(&mut client, code, &serde_json::to_value(&receipt)?).await
        }
        ("GET", "/v1/build") => {
            let order = state.build_order.read().await.clone();
            let map = state.builds.read().await;
            let items: Vec<BuildReceipt> = order
                .iter()
                .rev()
                .filter_map(|id| map.get(id).cloned())
                .collect();
            write_json_response(&mut client, 200, &json!({ "builds": items })).await
        }
        ("GET", p) if p.starts_with("/v1/build/") => {
            let id = p.trim_start_matches("/v1/build/").trim();
            if id == "latest" {
                let order = state.build_order.read().await;
                if let Some(last_id) = order.last() {
                    if let Some(rec) = state.builds.read().await.get(last_id).cloned() {
                        return write_json_response(&mut client, 200, &serde_json::to_value(rec)?).await;
                    }
                }
                return write_json_response(&mut client, 404, &json!({ "error": "no builds yet" })).await;
            }
            if let Some(rec) = state.builds.read().await.get(id).cloned() {
                return write_json_response(&mut client, 200, &serde_json::to_value(rec)?).await;
            }
            // Fall back to cloud `/v1/build/:id` if it's a remote build job ID.
            let mut cfg = state.cfg.clone();
            if let Ok(seam) = Seam::open(&mut cfg, DEFAULT_BRAND).await {
                if let Ok(reply) = seam.send(Method::GET, &format!("/v1/build/{id}"), &[], None).await {
                    return write_json_response(&mut client, reply.status, &reply.body).await;
                }
            }
            write_json_response(&mut client, 404, &json!({ "error": format!("build `{id}` not found") })).await
        }
        ("GET", "/v1/dev/status") | ("GET", "/__hanzo/status") => {
            let status = collect_stack_status(&state).await;
            write_json_response(&mut client, 200, &status).await
        }
        ("POST", "/v1/bot/ask") => {
            let req_json: Value = serde_json::from_slice(body_bytes).unwrap_or_else(|_| json!({}));
            let prompt = req_json
                .get("message")
                .or_else(|| req_json.get("prompt"))
                .and_then(Value::as_str)
                .unwrap_or("Hello!")
                .to_string();
            let engine_url = state.engine_url.clone();
            let res = tokio::task::spawn_blocking(move || {
                crate::commands::bot::ask_sync(&prompt, "zen-nano", &engine_url)
            })
            .await;
            match res {
                Ok(Ok(reply)) => write_json_response(&mut client, 200, &reply).await,
                Ok(Err(err)) => {
                    write_json_response(&mut client, 502, &json!({ "error": err.to_string() })).await
                }
                Err(err) => {
                    write_json_response(&mut client, 500, &json!({ "error": err.to_string() })).await
                }
            }
        }
        (_, "/v1/models") | (_, "/v1/chat/completions") | (_, "/v1/messages") => {
            proxy_to_engine(&mut client, &state, &head, body_bytes).await
        }
        _ => write_json_response(&mut client, 404, &json!({ "error": "not found" })).await,
    }
}

async fn proxy_to_engine(
    client: &mut TcpStream,
    state: &ServerState,
    head: &ParsedHead,
    body_bytes: &[u8],
) -> Result<()> {
    let url = format!("{}{}", state.engine_url, head.target);
    let method = reqwest::Method::from_bytes(head.method.as_bytes())
        .unwrap_or(reqwest::Method::GET);
    let mut req = state.http.request(method, &url);
    for (k, v) in &head.headers {
        if matches!(
            k.to_ascii_lowercase().as_str(),
            "content-type" | "authorization" | "x-api-key" | "anthropic-version"
        ) {
            req = req.header(k, v);
        }
    }
    if !body_bytes.is_empty() {
        req = req.body(body_bytes.to_vec());
    }
    match req.send().await {
        Ok(resp) => {
            let status = resp.status().as_u16();
            let ct = resp
                .headers()
                .get("content-type")
                .and_then(|v| v.to_str().ok())
                .unwrap_or("application/json")
                .to_string();
            let bytes = resp.bytes().await.unwrap_or_default();
            let hdr = format!(
                "HTTP/1.1 {status} OK\r\n\
                 Content-Type: {ct}\r\n\
                 Content-Length: {}\r\n\
                 Access-Control-Allow-Origin: *\r\n\
                 Cache-Control: no-store\r\n\
                 Connection: close\r\n\r\n",
                bytes.len()
            );
            client.write_all(hdr.as_bytes()).await?;
            client.write_all(&bytes).await?;
            client.flush().await?;
            Ok(())
        }
        Err(err) => {
            write_json_response(
                client,
                502,
                &json!({
                    "error": format!(
                        "local Hanzo Engine unreachable at {}: {} (start with `hanzo engine serve zen-nano`)",
                        state.engine_url, err
                    )
                }),
            )
            .await
        }
    }
}

async fn collect_stack_status(state: &ServerState) -> Value {
    let up_kube = dirs::home_dir()
        .map(|h| h.join(".kube").join("hanzo.yaml").is_file())
        .unwrap_or(false);
    let up_cloud = TcpStream::connect(("127.0.0.1", 3690)).await.is_ok()
        || TcpStream::connect(("127.0.0.1", 6443)).await.is_ok();

    let engine_models = match state
        .http
        .get(format!("{}/v1/models", state.engine_url))
        .timeout(Duration::from_millis(800))
        .send()
        .await
    {
        Ok(r) if r.status().is_success() => r.json::<Value>().await.ok(),
        _ => None,
    };
    let loaded_model = engine_models
        .as_ref()
        .and_then(|v| v.get("data"))
        .and_then(Value::as_array)
        .and_then(|arr| {
            arr.iter()
                .filter_map(|m| m.get("id").and_then(Value::as_str))
                .find(|id| *id != "default")
                .or_else(|| arr.first().and_then(|m| m.get("id").and_then(Value::as_str)))
        })
        .map(String::from);

    let bot_gateway = TcpStream::connect(("127.0.0.1", 19001)).await.is_ok();
    let bot_installed = crate::commands::bot::bot_bin().is_some();

    let latest_build = {
        let order = state.build_order.read().await;
        let map = state.builds.read().await;
        order.last().and_then(|id| map.get(id).cloned())
    };

    json!({
        "vite": {
            "running": true,
            "hmr": true,
            "upstreamPort": state.vite_port
        },
        "up": {
            "running": up_cloud,
            "kubeconfig": up_kube,
            "endpoint": "http://127.0.0.1:3690"
        },
        "engine": {
            "running": engine_models.is_some(),
            "endpoint": state.engine_url,
            "model": loaded_model
        },
        "bot": {
            "installed": bot_installed,
            "gatewayRunning": bot_gateway,
            "gatewayPort": 19001
        },
        "latestBuild": latest_build
    })
}

/// Dispatch a remote `/v1/build` job (`hanzo build --cloud`), optionally polling `GET /v1/build/:id`.
pub async fn run_cloud_build(
    cfg: &mut Config,
    context_dir: &Path,
    dockerfile: Option<&Path>,
    tags: &[String],
    platforms: &[String],
    wait: bool,
) -> Result<()> {
    let git_out = |args: &[&str]| -> Result<String> {
        let out = Command::new("git")
            .args(args)
            .current_dir(context_dir)
            .output()
            .context("running git")?;
        if !out.status.success() {
            bail!("git {} failed: {}", args.join(" "), String::from_utf8_lossy(&out.stderr).trim());
        }
        Ok(String::from_utf8(out.stdout)?.trim().to_string())
    };

    let origin = git_out(&["remote", "get-url", "origin"])
        .context("`hanzo build --cloud` needs a git `origin` remote (or use `hanzo build create --repo ...`)")?;
    let repo = normalize_repo_url(&origin)?;
    let branch = git_out(&["symbolic-ref", "--quiet", "--short", "HEAD"]).unwrap_or_else(|_| "main".into());
    let sha = git_out(&["rev-parse", "HEAD"])?;
    let image = tags
        .first()
        .cloned()
        .ok_or_else(|| anyhow!("`hanzo build --cloud` needs `-t <registry/org/repo:tag>`"))?;
    let df = dockerfile
        .and_then(|p| p.to_str())
        .unwrap_or("Dockerfile")
        .to_string();

    let mut body = json!({
        "repo": repo,
        "branch": branch,
        "sha": sha,
        "dockerfile": df,
        "image": image,
    });
    if tags.len() > 1 {
        body["tags"] = json!(tags);
    }
    if !platforms.is_empty() {
        body["platforms"] = json!(platforms);
    }

    let seam = Seam::open(cfg, DEFAULT_BRAND).await?;
    let reply = seam.send(Method::POST, "/v1/build", &[], Some(body)).await?;
    if !(200..300).contains(&reply.status) {
        bail!("POST /v1/build returned HTTP {}: {}", reply.status, reply.body);
    }
    let id = reply
        .body
        .get("buildJobId")
        .or_else(|| reply.body.get("id"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    println!(
        "{} Enqueued cloud `/v1/build` job `{}` → `{}`",
        "✓".green().bold(),
        id.bold(),
        image
    );
    if !wait || id.is_empty() {
        println!("  Inspect: `hanzo build get {id}`");
        return Ok(());
    }

    loop {
        tokio::time::sleep(Duration::from_secs(3)).await;
        let poll = seam.send(Method::GET, &format!("/v1/build/{id}"), &[], None).await?;
        let st = poll.body.get("status").and_then(Value::as_str).unwrap_or("unknown");
        println!("  • [{id}] status = {st}");
        match st {
            "succeeded" | "done" | "complete" => {
                println!("{} Cloud build `{id}` succeeded!", "✓".green().bold());
                return Ok(());
            }
            "failed" | "error" => {
                bail!("cloud build `{id}` failed: {}", poll.body);
            }
            _ => {}
        }
    }
}

fn normalize_repo_url(remote: &str) -> Result<String> {
    if let Some(rest) = remote.strip_prefix("git@github.com:") {
        return Ok(format!("https://github.com/{}", rest.trim_end_matches(".git")));
    }
    if let Some(rest) = remote.strip_prefix("git@git.hanzo.ai:") {
        return Ok(format!("https://git.hanzo.ai/{}", rest.trim_end_matches(".git")));
    }
    if remote.starts_with("https://") {
        return Ok(remote.trim_end_matches(".git").to_string());
    }
    bail!("unsupported git remote `{remote}` for `/v1/build`")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_hanzo_yml_build_section_and_hashes_output_deterministically() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(
            tmp.path().join("hanzo.yml"),
            "build:\n  runtime: static\n  install: pnpm install --frozen-lockfile\n  command: pnpm run build # compile\n  output: build_out\n",
        )
        .unwrap();
        let m = load_manifest(tmp.path());
        assert_eq!(m.install_cmd, "pnpm install --frozen-lockfile");
        assert_eq!(m.build_cmd, "pnpm run build");
        assert_eq!(m.output_dir, "build_out");

        let out_dir = tmp.path().join("build_out");
        std::fs::create_dir_all(out_dir.join("assets")).unwrap();
        std::fs::write(out_dir.join("index.html"), b"<!doctype html><title>Antje</title>").unwrap();
        std::fs::write(out_dir.join("assets/app.js"), b"console.log('ok')").unwrap();

        let (d1, f1, b1) = hash_output_dir(&out_dir).unwrap();
        let (d2, f2, b2) = hash_output_dir(&out_dir).unwrap();
        assert_eq!((d1.as_str(), f1, b1), (d2.as_str(), f2, b2));
        assert!(d1.starts_with("sha256:"));
        assert_eq!(f1, 2);
    }

    #[test]
    fn rewrites_non_upgrade_connection_to_close_and_identifies_api_paths() {
        let req = b"GET /src/App.tsx HTTP/1.1\r\nHost: localhost:5173\r\nConnection: keep-alive\r\n\r\n";
        let parsed = parse_http_head(req).unwrap();
        assert!(!parsed.is_upgrade);
        assert!(!is_integrated_api(&parsed.path));
        let rewritten = rewrite_connection_close(req, parsed.body_offset);
        let text = std::str::from_utf8(&rewritten).unwrap();
        assert!(text.contains("Connection: close\r\n"));
        assert!(!text.contains("keep-alive"));

        assert!(is_integrated_api("/v1/build"));
        assert!(is_integrated_api("/v1/build/bld_123"));
        assert!(is_integrated_api("/v1/chat/completions"));
        assert!(is_integrated_api("/v1/dev/status"));
    }
}
