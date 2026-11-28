//! Model (`hanzo model`), sandbox (`hanzo sandbox start|stop`), and workload
//! execution (`hanzo run <target> [args...]`) lifecycle controllers.

use anyhow::{anyhow, bail, Context, Result};
use colored::*;
use hanzo_client::Method;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

use super::client::{call_lifecycle, SandboxRequest};
use super::project::{
    detect_project, load_state, resolve_registered_app, save_state, slugify, upsert_detected_app,
    RegisteredSandbox,
};
use super::resources::{app_start, color_state, deploy_or_spawn_app};
use crate::commands::{daemon, engine, launch, vm};
use crate::config::Config;

// ── `hanzo model` ────────────────────────────────────────────────────────────

pub async fn model_pull(model: &str, node: &str) -> Result<()> {
    engine::pull(model, node).await
}

pub async fn model_list(as_json: bool) -> Result<()> {
    let hub_dir = engine::hf_hub_dir();
    let mut models_map: std::collections::BTreeMap<String, Value> =
        std::collections::BTreeMap::new();

    // 1. Hugging Face Hub cached weights
    if let Ok(entries) = std::fs::read_dir(&hub_dir) {
        for entry in entries.flatten() {
            let fname = entry.file_name().to_string_lossy().to_string();
            if let Some(rest) = fname.strip_prefix("models--") {
                let repo_id = rest.replacen("--", "/", 1);
                let rev = std::fs::read_to_string(entry.path().join("refs").join("main"))
                    .map(|s| s.trim().chars().take(8).collect::<String>())
                    .unwrap_or_else(|_| "-".into());
                models_map.insert(
                    repo_id.clone(),
                    json!({
                        "id": repo_id,
                        "state": "Cached",
                        "source": "hf-hub",
                        "revision": rev,
                    }),
                );
            }
        }
    }

    // 2. Local engine (:1234 / :1235) active models
    let http = reqwest::Client::builder()
        .timeout(std::time::Duration::from_millis(400))
        .build()?;
    for port in [1234u16, 1235u16] {
        let url = format!("http://127.0.0.1:{port}/v1/models");
        if let Ok(resp) = http.get(&url).send().await {
            if let Ok(val) = resp.json::<Value>().await {
                if let Some(arr) = val.get("data").and_then(Value::as_array) {
                    for m in arr {
                        if let Some(id) = m.get("id").and_then(Value::as_str) {
                            if hanzo_gen::enso::is_hidden_enso_model(id) {
                                continue;
                            }
                            models_map.insert(
                                id.to_string(),
                                json!({
                                    "id": id,
                                    "state": "Running",
                                    "source": format!("http://127.0.0.1:{port}"),
                                    "revision": "live",
                                }),
                            );
                        }
                    }
                }
            }
        }
    }

    // 3. hanzod `/v1/engine/catalog` if available
    if let Some(cat) = daemon::probe_engine_catalog().await {
        if let Some(arr) = cat
            .get("models")
            .or_else(|| cat.get("data"))
            .and_then(Value::as_array)
        {
            for m in arr {
                if let Some(id) = m.get("id").or_else(|| m.get("name")).and_then(Value::as_str) {
                    if hanzo_gen::enso::is_hidden_enso_model(id) {
                        continue;
                    }
                    let loaded = m.get("loaded").and_then(Value::as_bool).unwrap_or(false);
                    models_map.entry(id.to_string()).or_insert_with(|| {
                        json!({
                            "id": id,
                            "state": if loaded { "Running" } else { "Available" },
                            "source": "hanzod",
                            "revision": "-",
                        })
                    });
                }
            }
        }
    }

    let rows: Vec<Value> = models_map.into_values().collect();
    if as_json {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({ "models": rows }))?
        );
        return Ok(());
    }
    if rows.is_empty() {
        println!(
            "No local models cached in {}. Pull one with `hanzo model pull zenlm/zen-nano-0.6b`.",
            hub_dir.display()
        );
        return Ok(());
    }
    println!("{:<42} {:<12} {:<24} REV", "MODEL", "STATE", "SOURCE");
    for r in rows {
        let model_display = hanzo_gen::enso::format_model_id(r["id"].as_str().unwrap_or("-"));
        println!(
            "{:<42} {:<12} {:<24} {}",
            model_display,
            color_state(r["state"].as_str().unwrap_or("Cached")),
            r["source"].as_str().unwrap_or("-"),
            r["revision"].as_str().unwrap_or("-"),
        );
    }
    Ok(())
}

pub async fn model_run(cfg: &mut Config, model: &str, prompt_words: Vec<String>) -> Result<()> {
    let resolved = engine::resolve_model_id(model).to_string();
    let prompt = prompt_words.join(" ").trim().to_string();
    if prompt.is_empty() {
        return engine::serve(resolved, None, Vec::new()).await;
    }

    // Try direct local engine (:1234 or :1235) first for lowest-latency local inference
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(120))
        .build()?;
    for port in [1234u16, 1235u16] {
        let probe = reqwest::Client::builder()
            .timeout(std::time::Duration::from_millis(350))
            .build()?
            .get(format!("http://127.0.0.1:{port}/v1/models"))
            .send()
            .await;
        if probe.is_ok_and(|r| r.status().is_success()) {
            let resp = client
                .post(format!("http://127.0.0.1:{port}/v1/chat/completions"))
                .json(&json!({
                    "model": resolved,
                    "messages": [{ "role": "user", "content": prompt }],
                }))
                .send()
                .await
                .context("calling local Hanzo Engine")?;
            if resp.status().is_success() {
                let body: Value = resp.json().await?;
                if let Some(text) = body
                    .pointer("/choices/0/message/content")
                    .and_then(Value::as_str)
                {
                    println!("{text}");
                    return Ok(());
                }
            }
        }
    }

    // Delegate through `hanzod` runtime
    daemon::run_turn(cfg, &prompt, None, None, Some(resolved), None, None, false).await
}

// ── `hanzo sandbox start / stop` ─────────────────────────────────────────────

pub async fn sandbox_start(
    cfg: &mut Config,
    name: Option<String>,
    image: Option<String>,
    node: String,
) -> Result<()> {
    let sbx_name = name
        .filter(|s| !s.trim().is_empty())
        .map(|s| slugify(&s))
        .unwrap_or_else(|| "default".to_string());
    let sbx_image = image
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| "hanzo".to_string());

    let req = SandboxRequest {
        action: "start".to_string(),
        name: Some(sbx_name.clone()),
        image: Some(sbx_image.clone()),
    };
    let _ = call_lifecycle(
        Some(cfg),
        Method::POST,
        "sandbox",
        Some(serde_json::to_value(&req)?),
        true,
    )
    .await?;

    let mut state = load_state().unwrap_or_default();
    state.sandboxes.insert(
        sbx_name.clone(),
        RegisteredSandbox {
            name: sbx_name.clone(),
            image: sbx_image.clone(),
            node: node.clone(),
            desired_running: true,
            pid: None,
        },
    );
    save_state(&state)?;

    println!(
        "{} started sandbox `{}` (template/image `{}` on `{}`)",
        "✓".green(),
        sbx_name.bold(),
        sbx_image.cyan(),
        node
    );
    Ok(())
}

pub async fn sandbox_stop(cfg: &mut Config, name: Option<String>) -> Result<()> {
    let sbx_name = name
        .filter(|s| !s.trim().is_empty())
        .map(|s| slugify(&s))
        .unwrap_or_else(|| "default".to_string());

    let req = SandboxRequest {
        action: "stop".to_string(),
        name: Some(sbx_name.clone()),
        image: None,
    };
    let _ = call_lifecycle(
        Some(cfg),
        Method::POST,
        "sandbox",
        Some(serde_json::to_value(&req)?),
        true,
    )
    .await?;

    let mut state = load_state().unwrap_or_default();
    if let Some(sbx) = state.sandboxes.get_mut(&sbx_name) {
        sbx.desired_running = false;
    }
    save_state(&state)?;

    println!("{} stopped sandbox `{}`", "✓".green(), sbx_name.bold());
    Ok(())
}

// ── `hanzo run <target> [args...]` ───────────────────────────────────────────

pub async fn run_target(cfg: &mut Config, target: String, args: Vec<String>) -> Result<()> {
    let t = target.trim();
    if t.is_empty() {
        bail!("usage: hanzo run <target> [args...]");
    }

    // 1. Known model alias or cached HF model or explicit `org/model` inference target
    if looks_like_model_target(t) {
        return model_run(cfg, t, args).await;
    }

    // 2. Directory path (`.` / `./app`) or registered application slug
    let path = Path::new(t);
    if path.is_dir() {
        let detected = detect_project(path)?.ok_or_else(|| {
            anyhow!(
                "directory `{}` contains no recognized project manifest",
                path.display()
            )
        })?;
        let mut state = load_state().unwrap_or_default();
        let app = upsert_detected_app(&mut state, &detected);
        deploy_or_spawn_app(&mut state, &app).await?;
        save_state(&state)?;
        return Ok(());
    }

    if let Ok(state) = load_state() {
        if resolve_registered_app(&state, Some(t), None).is_ok() {
            return app_start(cfg, Some(t)).await;
        }
    }

    // 3. Local script file
    if path.is_file() {
        return run_script_file(path, &args);
    }

    // 4. Coding agent harness name (`claude`, `codex`, `cursor`, `agy`, `dev`)
    if hanzo_gen::backend::BackendKind::names(t) {
        let mut session_args = crate::commands::cloud_sessions::SessionArgs {
            local: hanzo_gen::Args {
                backend: Some(t.to_string()),
                dev: t == "dev",
                claude: t == "claude",
                codex: t == "codex",
                agy: t == "agy" || t == "antigravity",
                cursor: t == "cursor" || t == "agent",
                link: false,
                no_link: false,
                cloud: false,
                no_mcp: false,
                project_mcp: false,
                ask: false,
                no_sandbox: false,
                runtime: None,
                gpus: None,
                resume: None,
                brand: crate::iam::paths::DEFAULT_BRAND.to_string(),
                theme: None,
                model: None,
                effort: None,
                positional: (!args.is_empty()).then(|| args.join(" ")),
                tail: None,
                passthrough: Vec::new(),
            },
            remote: crate::commands::cloud_sessions::CloudArgs::default(),
        };
        if t == "dev" {
            session_args.local.backend = None;
        }
        return crate::commands::dev::session(
            cfg,
            session_args,
            crate::commands::dev::Target::Repo,
        )
        .await;
    }

    // 5. Fallback: execute workload inside an isolated `hanzo-vm` microVM
    let mut vm_argv = vec!["run".to_string(), "--allow-net".to_string(), t.to_string()];
    vm_argv.extend(args);
    vm::run(vm_argv).await
}

fn looks_like_model_target(target: &str) -> bool {
    if engine::resolve_model_id(target) != target || target.starts_with("local:") {
        return true;
    }
    if target.contains('/') && !Path::new(target).exists() && !target.contains(':') {
        let hub = engine::hf_hub_dir();
        if engine::cached_snapshot(&hub, target).is_some() {
            return true;
        }
        let lower = target.to_ascii_lowercase();
        if [
            "zenlm/",
            "hanzoai/",
            "qwen/",
            "deepseek-ai/",
            "meta-llama/",
            "mistralai/",
            "google/",
            "nvidia/",
        ]
        .iter()
        .any(|p| lower.starts_with(p))
        {
            return true;
        }
    }
    false
}

fn run_script_file(path: &Path, args: &[String]) -> Result<()> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    let (prog, mut argv): (&str, Vec<String>) = match ext.as_str() {
        "py" => ("python3", vec![path.display().to_string()]),
        "js" | "mjs" | "cjs" => ("node", vec![path.display().to_string()]),
        "sh" => ("sh", vec![path.display().to_string()]),
        _ => (
            path.to_str().ok_or_else(|| anyhow!("non-utf8 script path"))?,
            Vec::new(),
        ),
    };
    argv.extend_from_slice(args);
    let bin = which::which(prog).unwrap_or_else(|_| PathBuf::from(prog));
    launch::exec(&bin, &argv)
}
