//! `hanzo bot` & sandboxed computer-use helpers — local HanzoBot runtime wired to
//! local `hanzo-engine` (`zen-nano` / `zenlm/zen-nano-0.6b`), kernel-sandboxed command
//! execution (`hanzo sandbox exec`), and sandboxed headless browser computer use
//! (`hanzo sandbox computer`).
//!
//! Cloud `/v1/bot/*` operations (`members`, `runs`) remain mounted on `hanzo bot` via
//! `commands::product::augment`'s collision law.

use anyhow::{anyhow, bail, Context, Result};
use colored::*;
use serde_json::{json, Value};
use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, Instant};

use crate::commands::{engine, launch, vm};

/// Locate the local `hanzo-bot` / `bot` runtime binary.
pub fn bot_bin() -> Option<PathBuf> {
    launch::resolve("HANZO_BOT_BIN", &["hanzo-bot", "bot"])
}

/// Locate the local `hanzo-dev` / `dev` coding agent binary (used for Seatbelt sandboxing).
fn dev_bin() -> Option<PathBuf> {
    launch::resolve("HANZO_DEV_BIN", &["hanzo-dev", "dev"])
}

/// Locate a local Chromium / Chrome binary for sandboxed computer-use rendering & screenshots.
fn chrome_bin() -> Option<PathBuf> {
    if let Ok(explicit) = std::env::var("HANZO_CHROME_BIN") {
        let p = PathBuf::from(explicit);
        if p.is_file() {
            return Some(p);
        }
    }
    let candidates = [
        "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
        "/Applications/Chromium.app/Contents/MacOS/Chromium",
        "/Applications/Brave Browser.app/Contents/MacOS/Brave Browser",
    ];
    for c in candidates {
        let p = PathBuf::from(c);
        if p.is_file() {
            return Some(p);
        }
    }
    which::which("chromium")
        .or_else(|_| which::which("google-chrome"))
        .ok()
}

fn model_engine_url(s: &str) -> &str {
    s.trim().trim_end_matches('/').trim_end_matches("/v1")
}

/// Probe `{engine_url}/v1/models` to discover the active model ID on a running `hanzo-engine`,
/// falling back to `engine::resolve_model_id(model)` when offline.
fn probe_live_model_id(engine_url: &str, requested: &str) -> String {
    use std::io::{Read, Write};
    use std::net::ToSocketAddrs;

    let wanted = engine::resolve_model_id(requested);
    let stripped = engine_url
        .trim()
        .trim_start_matches("http://")
        .trim_start_matches("https://");
    let host_port = stripped.split('/').next().unwrap_or("127.0.0.1:1234");
    let addr_str = if host_port.contains(':') {
        host_port.to_string()
    } else {
        format!("{host_port}:80")
    };

    let Ok(mut addrs) = addr_str.to_socket_addrs() else {
        return wanted.to_string();
    };
    let Some(addr) = addrs.next() else {
        return wanted.to_string();
    };
    let Ok(mut stream) = std::net::TcpStream::connect_timeout(&addr, Duration::from_millis(400)) else {
        return wanted.to_string();
    };
    let _ = stream.set_read_timeout(Some(Duration::from_millis(600)));
    let _ = stream.set_write_timeout(Some(Duration::from_millis(400)));
    let req = format!(
        "GET /v1/models HTTP/1.1\r\nHost: {host_port}\r\nAccept: application/json\r\nConnection: close\r\n\r\n"
    );
    if stream.write_all(req.as_bytes()).is_err() {
        return wanted.to_string();
    }
    let mut raw = Vec::new();
    let _ = stream.read_to_end(&mut raw);
    let Some(pos) = raw.windows(4).position(|w| w == b"\r\n\r\n") else {
        return wanted.to_string();
    };
    let Ok(json_val) = serde_json::from_slice::<Value>(&raw[pos + 4..]) else {
        return wanted.to_string();
    };
    let Some(items) = json_val.get("data").and_then(Value::as_array) else {
        return wanted.to_string();
    };
    let ids: Vec<&str> = items
        .iter()
        .filter_map(|m| m.get("id").and_then(Value::as_str))
        .collect();
    if ids.contains(&requested) {
        return requested.to_string();
    }
    if ids.contains(&wanted) {
        return wanted.to_string();
    }
    if let Some(non_default) = ids.iter().copied().find(|id| *id != "default") {
        return non_default.to_string();
    }
    if ids.contains(&"default") {
        return "default".to_string();
    }
    wanted.to_string()
}

/// Ensure `~/.hanzo/bot-local` holds a valid `bot.json` and compact workspace wired to
/// the local `hanzo-engine` OpenAI-compatible endpoint (`http://127.0.0.1:1234/v1`).
pub fn ensure_local_bot_state(model: &str, engine_url: &str, gateway_port: u16) -> Result<(PathBuf, PathBuf, String)> {
    let resolved_model = probe_live_model_id(model_engine_url(engine_url), model);
    let state_dir = dirs::home_dir()
        .context("no home directory")?
        .join(".hanzo")
        .join("bot-local");
    let workspace_dir = state_dir.join("workspace");
    std::fs::create_dir_all(&workspace_dir)
        .with_context(|| format!("creating {}", workspace_dir.display()))?;

    let soul_path = workspace_dir.join("SOUL.md");
    if !soul_path.is_file() {
        std::fs::write(
            &soul_path,
            "# Hanzo Local Bot\n\nYou are Hanzo Bot running 100% locally on Hanzo Engine (Zen Nano). Answer concisely, accurately, and warmly.\n",
        )?;
    }

    let base_v1 = format!("{}/v1", model_engine_url(engine_url));
    let cfg_json = json!({
        "gateway": {
            "mode": "local",
            "port": gateway_port,
            "auth": { "mode": "none" },
            "controlUi": {
                "allowedOrigins": [
                    "http://localhost:5173",
                    "http://127.0.0.1:5173",
                    format!("http://localhost:{gateway_port}"),
                    format!("http://127.0.0.1:{gateway_port}")
                ],
                "allowInsecureAuth": true,
                "dangerouslyDisableDeviceAuth": true,
                "basePath": "/__bot__"
            }
        },
        "agents": {
            "defaults": {
                "workspace": workspace_dir.display().to_string(),
                "bootstrapMaxChars": 2000,
                "bootstrapTotalMaxChars": 4000,
                "model": {
                    "primary": format!("hanzo-local/{resolved_model}")
                }
            }
        },
        "models": {
            "providers": {
                "hanzo-local": {
                    "baseUrl": base_v1,
                    "api": "openai-completions",
                    "apiKey": "local-hanzo-engine",
                    "models": [
                        {
                            "id": resolved_model,
                            "name": format!("{resolved_model} (Local Hanzo Engine)"),
                            "contextWindow": 32768,
                            "maxTokens": 1024
                        }
                    ]
                }
            }
        }
    });

    let cfg_path = state_dir.join("bot.json");
    std::fs::write(&cfg_path, serde_json::to_vec_pretty(&cfg_json)?)
        .with_context(|| format!("writing {}", cfg_path.display()))?;

    Ok((state_dir, cfg_path, resolved_model))
}

/// Synchronous local HanzoBot turn — used by both `hanzo bot ask` and `POST /v1/bot/ask`.
pub fn ask_sync(message: &str, model: &str, engine_url: &str) -> Result<Value> {
    let bin = bot_bin().ok_or_else(|| {
        anyhow!("`hanzo-bot` binary not found on PATH (expected `~/.local/bin/hanzo-bot`)")
    })?;
    let (state_dir, cfg_path, resolved_model) = ensure_local_bot_state(model, engine_url, 19001)?;
    let session_id = format!(
        "local-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(1)
    );

    let started = Instant::now();
    let out = Command::new(&bin)
        .env("BOT_STATE_DIR", &state_dir)
        .env("BOT_CONFIG_PATH", &cfg_path)
        .args([
            "agent",
            "--local",
            "--thinking",
            "off",
            "--session-id",
            &session_id,
            "-m",
            message,
            "--json",
        ])
        .output()
        .with_context(|| format!("running {}", bin.display()))?;

    let elapsed_ms = started.elapsed().as_millis() as u64;
    let stdout = String::from_utf8_lossy(&out.stdout);
    // `hanzo-bot agent --json` may emit log lines on stderr and a JSON object on stdout.
    let parsed: Value = extract_json_object(&stdout).unwrap_or_else(|| {
        json!({
            "payloads": [{ "text": stdout.trim() }]
        })
    });

    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        bail!("hanzo-bot exited with {}: {}", out.status, stderr.trim());
    }

    let reply_text = parsed
        .get("payloads")
        .and_then(Value::as_array)
        .and_then(|arr| arr.first())
        .and_then(|p| p.get("text"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_string();

    Ok(json!({
        "reply": reply_text,
        "model": resolved_model,
        "provider": "hanzo-local",
        "engineUrl": engine_url,
        "sessionId": session_id,
        "durationMs": elapsed_ms,
        "raw": parsed
    }))
}

fn extract_json_object(s: &str) -> Option<Value> {
    let end = s.rfind('}')?;
    for (idx, _) in s.match_indices('{') {
        if idx > end {
            break;
        }
        if let Ok(val) = serde_json::from_str::<Value>(&s[idx..=end]) {
            if val.is_object() {
                return Some(val);
            }
        }
    }
    None
}

/// `hanzo bot ask <MESSAGE>` — run one embedded local HanzoBot turn on local Zen-Nano.
pub async fn ask(message: String, model: String, engine_url: String, json_out: bool) -> Result<()> {
    let res = tokio::task::spawn_blocking(move || ask_sync(&message, &model, &engine_url))
        .await
        .context("joining local bot task")??;

    if json_out {
        println!("{}", serde_json::to_string_pretty(&res)?);
        return Ok(());
    }

    let reply = res.get("reply").and_then(Value::as_str).unwrap_or("");
    let model_id = res.get("model").and_then(Value::as_str).unwrap_or("zenlm/zen-nano-0.6b");
    let ms = res.get("durationMs").and_then(Value::as_u64).unwrap_or(0);
    println!(
        "{} {} {}",
        "●".green().bold(),
        "hanzo bot".bold(),
        format!("({model_id} · {ms} ms)").dimmed()
    );
    println!("{reply}");
    Ok(())
}

/// `hanzo bot gateway` — run the local HanzoBot WebSocket + Control UI gateway wired to local LLM.
pub async fn gateway(port: u16, model: String, engine_url: String, force: bool) -> Result<()> {
    let bin = bot_bin().ok_or_else(|| {
        anyhow!("`hanzo-bot` binary not found on PATH (expected `~/.local/bin/hanzo-bot`)")
    })?;
    let (state_dir, cfg_path, resolved_model) = ensure_local_bot_state(&model, &engine_url, port)?;

    println!(
        "{} Starting local HanzoBot gateway on {} (model: `{}` → `{}`)",
        "→".cyan(),
        format!("ws://127.0.0.1:{port}").cyan().bold(),
        resolved_model.bold(),
        engine_url
    );
    println!(
        "  Control UI: {}",
        format!("http://127.0.0.1:{port}/__bot__").underline()
    );

    let mut cmd = Command::new(&bin);
    cmd.env("BOT_STATE_DIR", &state_dir)
        .env("BOT_CONFIG_PATH", &cfg_path)
        .args([
            "gateway",
            "--allow-unconfigured",
            "--auth",
            "none",
            "--bind",
            "loopback",
            "--port",
            &port.to_string(),
        ]);
    if force {
        cmd.arg("--force");
    }
    let status = cmd.status().with_context(|| format!("running {}", bin.display()))?;
    if !status.success() {
        bail!("hanzo-bot gateway exited with {status}");
    }
    Ok(())
}

/// `hanzo bot browser <ARGS...>` — pass through to HanzoBot's dedicated browser automation CLI.
pub fn browser(args: Vec<String>) -> Result<()> {
    let bin = bot_bin().ok_or_else(|| {
        anyhow!("`hanzo-bot` binary not found on PATH (expected `~/.local/bin/hanzo-bot`)")
    })?;
    let (state_dir, cfg_path, _) =
        ensure_local_bot_state("zen-nano", "http://127.0.0.1:1234", 19001)?;
    let mut argv = vec!["browser".to_string()];
    if args.is_empty() {
        argv.push("--help".to_string());
    } else {
        argv.extend(args);
    }
    let status = Command::new(&bin)
        .env("BOT_STATE_DIR", &state_dir)
        .env("BOT_CONFIG_PATH", &cfg_path)
        .args(&argv)
        .status()
        .with_context(|| format!("running {}", bin.display()))?;
    if !status.success() {
        bail!("`hanzo bot browser` exited with {status}");
    }
    Ok(())
}

/// `hanzo bot status` — inspect local HanzoBot binary, gateway listener, and local LLM engine.
pub async fn status(engine_url: String, port: u16) -> Result<()> {
    let bin = bot_bin();
    let gw_up = tokio::net::TcpStream::connect(("127.0.0.1", port)).await.is_ok();
    let client = reqwest::Client::builder()
        .timeout(Duration::from_millis(1200))
        .build()?;
    let models_url = format!("{}/v1/models", engine_url.trim_end_matches('/'));
    let engine_model = match client.get(&models_url).send().await {
        Ok(r) if r.status().is_success() => r
            .json::<Value>()
            .await
            .ok()
            .and_then(|v| {
                v.get("data")?
                    .as_array()?
                    .iter()
                    .filter_map(|m| m.get("id")?.as_str())
                    .find(|id| *id != "default")
                    .map(String::from)
            }),
        _ => None,
    };

    println!("{}", "local hanzo bot".bold());
    match bin {
        Some(p) => println!("  {} runtime    {}", "●".green(), p.display()),
        None => println!("  {} runtime    not installed", "○".red()),
    }
    if gw_up {
        println!(
            "  {} gateway    online (ws://127.0.0.1:{port} · http://127.0.0.1:{port}/__bot__)",
            "●".green()
        );
    } else {
        println!(
            "  {} gateway    standby (start with `hanzo bot gateway --port {port}`)",
            "○".yellow()
        );
    }
    match engine_model {
        Some(m) => println!("  {} local llm  {} ({})", "●".green(), m.bold(), engine_url),
        None => println!(
            "  {} local llm  offline at {} (start with `hanzo engine serve zen-nano`)",
            "○".yellow(),
            engine_url
        ),
    }
    Ok(())
}

/// `hanzo sandbox exec` — execute a command inside the kernel Seatbelt sandbox (`hanzo-dev sandbox`)
/// or inside a hardware-virtualized `hanzo-vm` Linux microVM (`--vm`).
pub async fn sandbox_exec(use_vm: bool, cd: Option<PathBuf>, command: Vec<String>) -> Result<()> {
    if command.is_empty() {
        bail!("supply a command to execute inside the sandbox, e.g. `hanzo sandbox exec -- uname -a`");
    }
    if use_vm {
        let vm_bin = vm::resolve_or_install().await?;
        let mut argv = vec![
            "run".to_string(),
            "--allow-net".to_string(),
            "--cpus".to_string(),
            "2".to_string(),
            "--memory".to_string(),
            "2048".to_string(),
            "--".to_string(),
        ];
        argv.extend(command);
        return launch::exec(&vm_bin, &argv);
    }

    let dev = dev_bin().ok_or_else(|| {
        anyhow!("`hanzo-dev` binary not found on PATH for Seatbelt sandboxing")
    })?;
    let mut argv = vec!["sandbox".to_string()];
    if let Some(dir) = cd {
        argv.push("-C".to_string());
        argv.push(dir.display().to_string());
    }
    argv.push("--".to_string());
    argv.extend(command);
    launch::exec(&dev, &argv)
}

/// `hanzo sandbox computer <URL>` — sandboxed headless Chromium computer-use session:
/// renders `<URL>` in an ephemeral isolated profile, captures a Retina PNG screenshot +
/// hydrated DOM, and audits the live page with the local `zen-nano` LLM.
pub async fn sandbox_computer(
    url: String,
    out: Option<PathBuf>,
    engine_url: String,
) -> Result<()> {
    let chrome = chrome_bin().ok_or_else(|| {
        anyhow!("Chrome/Chromium not found for computer-use capture (set HANZO_CHROME_BIN)")
    })?;
    let out_path = out.unwrap_or_else(|| PathBuf::from(".hanzo/screenshots/computer-use.png"));
    if let Some(parent) = out_path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }
    let abs_out = if out_path.is_absolute() {
        out_path.clone()
    } else {
        std::env::current_dir()?.join(&out_path)
    };

    let profile = tempfile::tempdir().context("creating isolated browser sandbox profile")?;
    println!(
        "{} Launching sandboxed computer-use browser → {}",
        "→".cyan(),
        url.cyan().underline()
    );

    // 1. Capture Retina PNG screenshot in isolated profile.
    let shot_status = Command::new(&chrome)
        .args([
            "--headless=new",
            "--disable-gpu",
            "--no-first-run",
            "--no-default-browser-check",
            "--hide-scrollbars",
            "--window-size=1440,960",
            "--force-device-scale-factor=2",
            "--virtual-time-budget=3500",
            &format!("--user-data-dir={}", profile.path().display()),
            &format!("--screenshot={}", abs_out.display()),
            &url,
        ])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .context("capturing headless computer-use screenshot")?;
    if !shot_status.success() || !abs_out.is_file() {
        bail!("headless Chromium failed to capture screenshot of `{url}`");
    }
    let shot_bytes = std::fs::metadata(&abs_out)?.len();

    // 2. Capture hydrated DOM in the same isolated profile.
    let dom_out = Command::new(&chrome)
        .args([
            "--headless=new",
            "--disable-gpu",
            "--no-first-run",
            "--no-default-browser-check",
            "--virtual-time-budget=2500",
            &format!("--user-data-dir={}", profile.path().display()),
            "--dump-dom",
            &url,
        ])
        .output()
        .context("dumping hydrated DOM")?;
    let dom = String::from_utf8_lossy(&dom_out.stdout);
    let title = extract_tag(&dom, "title").unwrap_or_default();
    let h1 = extract_tag(&dom, "h1").unwrap_or_default();
    let text_preview = strip_html_tags(&dom)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    let snippet: String = text_preview.chars().take(700).collect();

    println!(
        "{} Screenshot saved: {} ({:.1} KiB, 2880×1920px)",
        "✓".green().bold(),
        abs_out.display(),
        shot_bytes as f64 / 1024.0
    );
    if !title.is_empty() {
        println!("  {} title: {}", "•".green(), title.bold());
    }
    if !h1.is_empty() {
        println!("  {} h1:    {}", "•".green(), h1);
    }

    // 3. Ask local Zen-Nano LLM (if running) to verify the rendered page.
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()?;
    let chat_url = format!("{}/v1/chat/completions", engine_url.trim_end_matches('/'));
    let prompt = format!(
        "You are inspecting a live webpage rendered in a sandboxed browser.\nURL: {url}\nTitle: {title}\nH1: {h1}\nVisible copy: {snippet}\n\nIn 2 concise sentences, confirm what this site showcases and verify that it rendered cleanly."
    );
    let active_model = probe_live_model_id(model_engine_url(&engine_url), "zen-nano");
    if let Ok(resp) = client
        .post(&chat_url)
        .json(&json!({
            "model": active_model,
            "chat_template_kwargs": { "enable_thinking": false },
            "messages": [{ "role": "user", "content": prompt }],
            "max_tokens": 110,
            "temperature": 0.2
        }))
        .send()
        .await
    {
        if resp.status().is_success() {
            if let Ok(body) = resp.json::<Value>().await {
                if let Some(summary) = body["choices"][0]["message"]["content"].as_str() {
                    println!(
                        "  {} zen-nano computer-use audit:\n    {}",
                        "●".cyan().bold(),
                        summary.trim().replace('\n', "\n    ")
                    );
                }
            }
        }
    }

    Ok(())
}

fn extract_tag(html: &str, tag: &str) -> Option<String> {
    let lower = html.to_ascii_lowercase();
    let open = format!("<{tag}");
    let close = format!("</{tag}>");
    let start_tag = lower.find(&open)?;
    let gt = lower[start_tag..].find('>')? + start_tag + 1;
    let end_tag = lower[gt..].find(&close)? + gt;
    let text = strip_html_tags(&html[gt..end_tag]);
    let trimmed = text.split_whitespace().collect::<Vec<_>>().join(" ");
    (!trimmed.is_empty()).then_some(trimmed)
}

fn strip_html_tags(s: &str) -> String {
    let mut out = String::with_capacity(s.len().min(4096));
    let mut in_tag = false;
    let mut in_script_or_style = false;
    let lower = s.to_ascii_lowercase();
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if !in_tag && (lower[i..].starts_with("<script") || lower[i..].starts_with("<style")) {
            in_script_or_style = true;
        }
        if in_script_or_style {
            if let Some(end_s) = lower[i..].find("</script>").or_else(|| lower[i..].find("</style>")) {
                let close_gt = lower[i + end_s..].find('>').unwrap_or(0);
                i += end_s + close_gt + 1;
                in_script_or_style = false;
                out.push(' ');
                continue;
            } else {
                break;
            }
        }
        let ch = s[i..].chars().next().unwrap();
        if ch == '<' {
            in_tag = true;
            out.push(' ');
        } else if ch == '>' {
            in_tag = false;
        } else if !in_tag {
            out.push(ch);
        }
        i += ch.len_utf8();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_title_and_h1_from_rendered_dom() {
        let html = r#"<!doctype html><html><head><title>Antje Worring · Co-CEO Hanzo AI</title><style>body{color:#fff}</style></head><body><h1><span>Antje</span> Worring</h1><script>var x = 1;</script><p>San Francisco</p></body></html>"#;
        assert_eq!(
            extract_tag(html, "title").as_deref(),
            Some("Antje Worring · Co-CEO Hanzo AI")
        );
        assert_eq!(extract_tag(html, "h1").as_deref(), Some("Antje Worring"));
        let stripped = strip_html_tags(html);
        assert!(stripped.contains("San Francisco"));
        assert!(!stripped.contains("var x"));
    }

    #[test]
    fn extracts_json_payload_past_banner_lines() {
        let noisy = "[wallet-deduct] usage={\"input\":0,\"output\":0}\n{\"payloads\":[{\"text\":\"Hello!\"}]}";
        let v = extract_json_object(noisy).unwrap();
        assert_eq!(v["payloads"][0]["text"], "Hello!");
    }
}
