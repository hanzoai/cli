//! Resource-specific lifecycle controllers: `hanzo vm`, `hanzo service`,
//! and `hanzo app`. Model, sandbox, and `hanzo run` live in `workloads.rs`.

use anyhow::{anyhow, bail, Context, Result};
use clap::{Parser, Subcommand};
use colored::*;
use hanzo_client::Method;
use serde_json::{json, Value};
use std::process::{Command, Stdio};

use super::client::{call_lifecycle, AppRequest, HanzodOutcome, ServiceRequest};
use super::project::{
    detect_project, lifecycle_dir, load_state, render_app_k8s_manifest, resolve_registered_app,
    save_state, upsert_detected_app, RegisteredApp, DEFAULT_CPUS, DEFAULT_DISK_MB,
    DEFAULT_MEMORY_MB,
};
use super::units::{format_size_mb, parse_cpus, parse_disk_mb, parse_memory_mb};
pub use super::workloads::{
    model_list, model_pull, model_run, run_target, sandbox_start, sandbox_stop,
};
use crate::commands::{engine, launch, up, vm};
use crate::config::Config;

// ── `hanzo vm` lifecycle subcommands + verbatim passthrough ──────────────────

#[derive(Parser, Debug)]
#[command(name = "hanzo vm", bin_name = "hanzo vm")]
pub struct VmLifecycleCli {
    #[command(subcommand)]
    pub command: VmLifecycleCommand,
}

#[derive(Subcommand, Debug)]
pub enum VmLifecycleCommand {
    /// Start the local Hanzo microVM (or native Linux runtime) without deploying Cloud
    Start {
        #[arg(long, value_parser = parse_cpus)]
        cpus: Option<u32>,
        #[arg(long, value_parser = parse_memory_mb)]
        memory: Option<u64>,
        #[arg(long = "disk", visible_alias = "disk-size", value_parser = parse_disk_mb)]
        disk_mb: Option<u64>,
        #[arg(long)]
        native: bool,
        #[arg(long, env = "HANZO_STRICT_REGISTRY")]
        strict_registry: bool,
    },
    /// Gracefully drain workloads and stop the local Hanzo microVM
    Stop,
    /// Resize CPU, memory, or disk allocation for the local Hanzo microVM
    Resize {
        #[arg(long, value_parser = parse_cpus)]
        cpus: Option<u32>,
        #[arg(long, value_parser = parse_memory_mb)]
        memory: Option<u64>,
        #[arg(long = "disk", visible_alias = "disk-size", value_parser = parse_disk_mb)]
        disk_mb: Option<u64>,
    },
    /// Show local Hanzo microVM / runtime status, sizing, and checkpoints
    Status {
        #[arg(long)]
        json: bool,
    },
}

pub fn is_vm_lifecycle_verb(args: &[String]) -> bool {
    matches!(
        args.first().map(String::as_str),
        Some("start" | "stop" | "resize" | "status")
    )
}

pub fn parse_vm_lifecycle(args: &[String]) -> std::result::Result<VmLifecycleCli, clap::Error> {
    let argv = std::iter::once("hanzo-vm".to_string()).chain(args.iter().cloned());
    VmLifecycleCli::try_parse_from(argv)
}

pub async fn dispatch_vm(cfg: &mut Config, args: Vec<String>) -> Result<()> {
    if !is_vm_lifecycle_verb(&args) {
        return vm::run(args).await;
    }
    let parsed = match parse_vm_lifecycle(&args) {
        Ok(p) => p,
        Err(e) => e.exit(),
    };
    match parsed.command {
        VmLifecycleCommand::Start {
            cpus,
            memory,
            disk_mb,
            native,
            strict_registry,
        } => {
            let state = load_state().unwrap_or_default();
            super::runtime_start(
                cfg,
                cpus.unwrap_or(state.runtime.cpus.max(1)),
                memory.unwrap_or(state.runtime.memory_mb.max(256)),
                disk_mb.unwrap_or(state.runtime.disk_mb.max(1024)),
                native,
                strict_registry,
            )
            .await
        }
        VmLifecycleCommand::Stop => super::runtime_stop(cfg).await,
        VmLifecycleCommand::Resize {
            cpus,
            memory,
            disk_mb,
        } => super::runtime_resize(cfg, cpus, memory, disk_mb).await,
        VmLifecycleCommand::Status { json } => vm_status(cfg, json).await,
    }
}

pub async fn vm_status(cfg: &mut Config, as_json: bool) -> Result<()> {
    let snap = super::collect_local_snapshot(Some(cfg)).await?;
    if as_json {
        println!("{}", serde_json::to_string_pretty(&snap["runtime"])?);
        return Ok(());
    }
    let rt = &snap["runtime"];
    let state = rt["state"]
        .as_str()
        .or_else(|| rt["actual"].as_str())
        .unwrap_or("Stopped");
    let backend = rt["backend"].as_str().unwrap_or("managed-vm");
    let cpus = rt["cpus"].as_u64().unwrap_or(DEFAULT_CPUS as u64);
    let mem = rt["memory_mb"].as_u64().unwrap_or(DEFAULT_MEMORY_MB);
    let disk = rt["disk_mb"].as_u64().unwrap_or(DEFAULT_DISK_MB);
    let k3s = rt["k3s"]
        .as_str()
        .or_else(|| rt.pointer("/components/k3s").and_then(Value::as_str))
        .unwrap_or("Stopped");
    let dot = if state.eq_ignore_ascii_case("running") {
        "●".green()
    } else {
        "○".dimmed()
    };
    println!("{dot} Hanzo Runtime: {} ({backend})", state.bold());
    println!(
        "  shape       {cpus} CPUs · {} RAM · {} disk",
        format_size_mb(mem),
        format_size_mb(disk)
    );
    println!("  k3s         {k3s}");
    if let Some(kc) = rt["kubeconfig"].as_str().filter(|s| !s.is_empty()) {
        println!("  kubeconfig  {kc}");
    }
    Ok(())
}

// ── `hanzo service` ──────────────────────────────────────────────────────────

const KNOWN_PLATFORM_SERVICES: &[(&str, &str)] = &[
    ("cloud", "Hanzo Cloud API & control plane"),
    ("engine", "Local AI model serving & router (:1234 / :1235)"),
    ("iam", "Identity & OIDC subsystem"),
    ("datastore", "SQL & vector data subsystem"),
    ("analytics", "Telemetry & event analytics"),
    ("insights", "Observability & logs subsystem"),
    ("storage", "S3-compatible object storage"),
    ("commerce", "Billing & commerce subsystem"),
];

pub async fn service_action(
    cfg: &mut Config,
    action: &str,
    name: Option<&str>,
    as_json: bool,
) -> Result<()> {
    let req = ServiceRequest {
        action: action.to_string(),
        name: name.map(str::to_string),
    };
    let hanzod_reply = call_lifecycle(
        Some(cfg),
        Method::POST,
        "service",
        Some(serde_json::to_value(&req)?),
        true,
    )
    .await?;

    let mut state = load_state().unwrap_or_default();

    if action == "list" {
        if let HanzodOutcome::Handled(val) = hanzod_reply {
            if as_json {
                println!("{}", serde_json::to_string_pretty(&val)?);
                return Ok(());
            }
            if let Some(rows) = val.get("services").and_then(Value::as_array) {
                println!("{:<16} {:<12} DETAIL", "SERVICE", "STATE");
                for r in rows {
                    let sname = r.get("name").and_then(Value::as_str).unwrap_or("-");
                    let st = r.get("state").and_then(Value::as_str).unwrap_or("Stopped");
                    let det = r.get("detail").and_then(Value::as_str).unwrap_or("");
                    println!("{:<16} {:<12} {}", sname, color_state(st), det);
                }
                return Ok(());
            }
        }
        return render_local_service_list(&state, as_json).await;
    }

    let svc = name
        .map(|s| s.trim().to_ascii_lowercase())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| anyhow!("service name is required (e.g. `hanzo service {action} cloud`)"))?;

    match (svc.as_str(), action) {
        ("cloud", "start") => {
            state.platform.cloud_enabled = true;
            save_state(&state)?;
            let boot = boot_from_state(&state);
            up::up(cfg, boot, None, false, true).await?;
        }
        ("cloud", "stop") => {
            state.platform.cloud_enabled = false;
            save_state(&state)?;
            up::down(false).await?;
        }
        ("cloud", "restart") => {
            state.platform.cloud_enabled = true;
            save_state(&state)?;
            let _ = up::down(false).await;
            let boot = boot_from_state(&state);
            up::up(cfg, boot, None, false, true).await?;
        }
        ("engine", "start") => {
            state.platform.engine_enabled = true;
            save_state(&state)?;
            engine::up().await?;
        }
        ("engine", "stop") => {
            state.platform.engine_enabled = false;
            save_state(&state)?;
            engine::down().await?;
        }
        ("engine", "restart") => {
            state.platform.engine_enabled = true;
            save_state(&state)?;
            let _ = engine::down().await;
            engine::up().await?;
        }
        (other, "start" | "restart") => {
            state
                .platform
                .services
                .insert(other.to_string(), super::project::ValueOrBool::Bool(true));
            save_state(&state)?;
            if let Some(kube) = local_kube_client() {
                if action == "restart" {
                    let _ = kube.scale_deployment("hanzo", other, 0).await;
                }
                let _ = kube.scale_deployment("hanzo", other, 1).await;
            }
            let verb = if action == "restart" {
                "restarted"
            } else {
                "started"
            };
            println!("{} service `{other}` {verb}", "✓".green());
        }
        (other, "stop") => {
            state
                .platform
                .services
                .insert(other.to_string(), super::project::ValueOrBool::Bool(false));
            save_state(&state)?;
            if let Some(kube) = local_kube_client() {
                let _ = kube.scale_deployment("hanzo", other, 0).await;
            }
            println!("{} service `{other}` stopped", "✓".green());
        }
        _ => bail!("unsupported service action `{action}`"),
    }
    Ok(())
}

async fn render_local_service_list(
    state: &super::project::DesiredState,
    as_json: bool,
) -> Result<()> {
    let cloud_replicas = match local_kube_client() {
        Some(kube) => kube.cloud_replicas().await.ok().flatten(),
        None => None,
    };
    let engine_up = local_engine_healthy().await;

    let mut items = Vec::new();
    for &(svc, desc) in KNOWN_PLATFORM_SERVICES {
        let st = match svc {
            "cloud" => match cloud_replicas {
                Some(n) if n > 0 => "Running",
                _ if state.platform.cloud_enabled => "Desired",
                _ => "Stopped",
            },
            "engine" => {
                if engine_up {
                    "Running"
                } else if state.platform.engine_enabled {
                    "Desired"
                } else {
                    "Stopped"
                }
            }
            other => {
                if state
                    .platform
                    .services
                    .get(other)
                    .is_some_and(|v| v.is_enabled())
                    || (matches!(other, "iam" | "kms" | "storage")
                        && cloud_replicas.is_some_and(|n| n > 0))
                {
                    "Running"
                } else {
                    "Stopped"
                }
            }
        };
        items.push(json!({
            "name": svc,
            "state": st,
            "detail": desc,
        }));
    }

    if as_json {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({ "services": items }))?
        );
        return Ok(());
    }
    println!("{:<16} {:<12} DETAIL", "SERVICE", "STATE");
    for item in items {
        let n = item["name"].as_str().unwrap_or("");
        let st = item["state"].as_str().unwrap_or("Stopped");
        let d = item["detail"].as_str().unwrap_or("");
        println!("{:<16} {:<12} {}", n, color_state(st), d);
    }
    Ok(())
}

// ── `hanzo app` ──────────────────────────────────────────────────────────────

pub async fn app_start(cfg: &mut Config, name: Option<&str>) -> Result<()> {
    let cwd = std::env::current_dir().ok();
    let mut state = load_state().unwrap_or_default();

    let app = if let Ok(existing) = resolve_registered_app(&state, name, cwd.as_deref()) {
        let mut updated = existing.clone();
        updated.desired_running = true;
        state.apps.insert(updated.slug.clone(), updated.clone());
        updated
    } else if name.is_none() {
        let dir = cwd
            .as_deref()
            .ok_or_else(|| anyhow!("cannot read current directory"))?;
        let detected = detect_project(dir)?.ok_or_else(|| {
            anyhow!(
                "no Hanzo project manifest or application detected in {} — add `hanzo.toml` or pass a registered app name",
                dir.display()
            )
        })?;
        upsert_detected_app(&mut state, &detected)
    } else {
        bail!(
            "no registered application matching `{}` — run `hanzo up` inside its project directory first",
            name.unwrap_or_default()
        );
    };

    save_state(&state)?;

    let req = AppRequest {
        action: "start".to_string(),
        name: Some(app.slug.clone()),
        project_dir: Some(app.project_dir.clone()),
    };
    let _ = call_lifecycle(
        Some(cfg),
        Method::POST,
        "app",
        Some(serde_json::to_value(&req)?),
        true,
    )
    .await?;

    deploy_or_spawn_app(&mut state, &app).await?;
    save_state(&state)?;
    Ok(())
}

pub(crate) async fn deploy_or_spawn_app(
    state: &mut super::project::DesiredState,
    app: &RegisteredApp,
) -> Result<()> {
    // 1. If the app declares a container image and local k3s is warm, deploy isolated Namespace+Deployment+Service!
    if let Some(ref image) = app.image {
        if let Some(kube) = local_kube_client() {
            let manifest = render_app_k8s_manifest(app, image);
            if kube
                .apply_manifest_bundle(&app.namespace, &manifest)
                .await
                .is_ok()
            {
                let _ = kube.scale_deployment(&app.namespace, &app.slug, 1).await;
                println!(
                    "{} deployed {} ({}) → namespace `{}` · Service `{}:{}` (NodePort {})",
                    "✓".green(),
                    app.name.bold(),
                    image.cyan(),
                    app.namespace,
                    app.slug,
                    app.container_port,
                    app.node_port
                );
                return Ok(());
            }
        }
    }

    // 2. If a local process from an earlier `app start` is already alive, keep it idempotently.
    if let Some(pid) = app.local_pid.filter(|p| vm::alive(*p as i32)) {
        println!(
            "{} {} already running (pid {pid}, port {})",
            "●".green(),
            app.name.bold(),
            app.container_port
        );
        return Ok(());
    }

    // 3. If the app has a local dev command and HANZO_SKIP_APP_SPAWN is not set, start detached with logs in ~/.hanzo/lifecycle/logs/<slug>.log
    let skip_spawn = std::env::var("HANZO_SKIP_APP_SPAWN")
        .ok()
        .is_some_and(|v| v == "1" || v.eq_ignore_ascii_case("true"));
    if !skip_spawn {
        if let Some(ref cmd_line) = app.command {
            let logs_dir = lifecycle_dir()?.join("logs");
            std::fs::create_dir_all(&logs_dir)?;
            let log_file = logs_dir.join(format!("{}.log", app.slug));
            if let Ok(out) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&log_file)
            {
                if let Ok(err) = out.try_clone() {
                    let mut child_cmd = Command::new("sh");
                    child_cmd
                        .args(["-c", cmd_line])
                        .current_dir(&app.project_dir)
                        .env("PORT", app.container_port.to_string())
                        .stdin(Stdio::null())
                        .stdout(Stdio::from(out))
                        .stderr(Stdio::from(err));
                    #[cfg(unix)]
                    {
                        use std::os::unix::process::CommandExt;
                        child_cmd.process_group(0);
                    }
                    if let Ok(child) = child_cmd.spawn() {
                        let pid = child.id();
                        if let Some(entry) = state.apps.get_mut(&app.slug) {
                            entry.local_pid = Some(pid);
                        }
                        println!(
                            "{} started {} (pid {pid}) → http://127.0.0.1:{} (logs: {})",
                            "✓".green(),
                            app.name.bold(),
                            app.container_port,
                            log_file.display()
                        );
                        return Ok(());
                    }
                }
            }
        }
    }

    println!(
        "{} registered {} ({}) → namespace `{}` · http://127.0.0.1:{}",
        "✓".green(),
        app.name.bold(),
        app.kind,
        app.namespace,
        app.container_port
    );
    Ok(())
}

pub async fn app_stop(cfg: &mut Config, name: Option<&str>) -> Result<()> {
    let cwd = std::env::current_dir().ok();
    let mut state = load_state().unwrap_or_default();
    let app = resolve_registered_app(&state, name, cwd.as_deref())?.clone();

    let req = AppRequest {
        action: "stop".to_string(),
        name: Some(app.slug.clone()),
        project_dir: Some(app.project_dir.clone()),
    };
    let _ = call_lifecycle(
        Some(cfg),
        Method::POST,
        "app",
        Some(serde_json::to_value(&req)?),
        true,
    )
    .await?;

    stop_registered_app_instance(&mut state, &app.slug).await?;
    save_state(&state)?;
    println!("{} stopped {} ({})", "✓".green(), app.name.bold(), app.slug);
    Ok(())
}

pub(crate) async fn stop_registered_app_instance(
    state: &mut super::project::DesiredState,
    slug: &str,
) -> Result<()> {
    if let Some(entry) = state.apps.get_mut(slug) {
        entry.desired_running = false;
        if let Some(pid) = entry.local_pid.take() {
            let _ = up::stop(pid as i32, std::time::Duration::from_secs(5));
        }
        if let Some(kube) = local_kube_client() {
            let _ = kube
                .scale_deployment(&entry.namespace, &entry.slug, 0)
                .await;
        }
    }
    Ok(())
}

pub async fn app_list(cfg: &mut Config, as_json: bool) -> Result<()> {
    let req = AppRequest {
        action: "list".to_string(),
        name: None,
        project_dir: None,
    };
    let hanzod = call_lifecycle(
        Some(cfg),
        Method::POST,
        "app",
        Some(serde_json::to_value(&req)?),
        false,
    )
    .await?;
    let state = load_state().unwrap_or_default();

    if state.apps.is_empty() {
        if let HanzodOutcome::Handled(val) = hanzod {
            if let Some(arr) = val.get("apps").and_then(Value::as_array) {
                if !arr.is_empty() {
                    if as_json {
                        println!("{}", serde_json::to_string_pretty(&val)?);
                    } else {
                        println!("{:<20} {:<12} {:<24} ENDPOINT", "APP", "STATE", "NAMESPACE");
                        for a in arr {
                            println!(
                                "{:<20} {:<12} {:<24} {}",
                                a.get("name").and_then(Value::as_str).unwrap_or("-"),
                                color_state(
                                    a.get("state").and_then(Value::as_str).unwrap_or("Stopped")
                                ),
                                a.get("namespace").and_then(Value::as_str).unwrap_or("-"),
                                a.get("endpoint").and_then(Value::as_str).unwrap_or("-"),
                            );
                        }
                    }
                    return Ok(());
                }
            }
        }
        if as_json {
            println!("{}", serde_json::to_string_pretty(&json!({ "apps": [] }))?);
        } else {
            println!("No local applications registered. Run `hanzo up` inside a project directory to deploy one.");
        }
        return Ok(());
    }

    let kube = local_kube_client();
    let mut rows = Vec::new();
    for app in state.apps.values() {
        let mut live = false;
        if let Some(pid) = app.local_pid {
            if vm::alive(pid as i32) {
                live = true;
            }
        }
        if !live {
            if let Some(ref k) = kube {
                if k.deployment_replicas(&app.namespace, &app.slug)
                    .await
                    .ok()
                    .flatten()
                    .is_some_and(|r| r > 0)
                {
                    live = true;
                }
            }
        }
        let st = if live || app.desired_running {
            "Running"
        } else {
            "Stopped"
        };
        let port = if app.image.is_some() {
            app.node_port
        } else {
            app.container_port
        };
        rows.push(json!({
            "slug": app.slug,
            "name": app.name,
            "state": st,
            "namespace": app.namespace,
            "endpoint": format!("http://127.0.0.1:{port}"),
            "project_dir": app.project_dir,
        }));
    }

    if as_json {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({ "apps": rows }))?
        );
        return Ok(());
    }
    println!("{:<20} {:<12} {:<24} ENDPOINT", "APP", "STATE", "NAMESPACE");
    for r in rows {
        println!(
            "{:<20} {:<12} {:<24} {}",
            r["name"].as_str().unwrap_or("-"),
            color_state(r["state"].as_str().unwrap_or("Stopped")),
            r["namespace"].as_str().unwrap_or("-"),
            r["endpoint"].as_str().unwrap_or("-"),
        );
    }
    Ok(())
}

pub async fn app_logs(cfg: &mut Config, name: Option<&str>, follow: bool) -> Result<()> {
    let cwd = std::env::current_dir().ok();
    let state = load_state().unwrap_or_default();
    let app = resolve_registered_app(&state, name, cwd.as_deref())?.clone();

    if follow {
        if let Ok(kc) = up::kubeconfig_path() {
            if kc.is_file() && app.image.is_some() {
                if let Ok(kubectl) = which::which("kubectl") {
                    return launch::exec(
                        &kubectl,
                        &[
                            "--kubeconfig".into(),
                            kc.display().to_string(),
                            "-n".into(),
                            app.namespace.clone(),
                            "logs".into(),
                            format!("deploy/{}", app.slug),
                            "-f".into(),
                        ],
                    );
                }
            }
        }
    }

    if let Some(kube) = local_kube_client() {
        if let Ok(logs) = kube
            .pod_logs(&app.namespace, &format!("app={}", app.slug), 200)
            .await
        {
            print!("{logs}");
            return Ok(());
        }
    }

    let log_file = lifecycle_dir()?
        .join("logs")
        .join(format!("{}.log", app.slug));
    if log_file.is_file() {
        if follow {
            if let Ok(tail_bin) = which::which("tail") {
                return launch::exec(
                    &tail_bin,
                    &[
                        "-n".into(),
                        "200".into(),
                        "-f".into(),
                        log_file.display().to_string(),
                    ],
                );
            }
        }
        let text = std::fs::read_to_string(&log_file)
            .with_context(|| format!("reading {}", log_file.display()))?;
        print!("{text}");
        return Ok(());
    }

    let req = AppRequest {
        action: "logs".to_string(),
        name: Some(app.slug.clone()),
        project_dir: Some(app.project_dir.clone()),
    };
    if let HanzodOutcome::Handled(val) = call_lifecycle(
        Some(cfg),
        Method::POST,
        "app",
        Some(serde_json::to_value(&req)?),
        false,
    )
    .await?
    {
        if let Some(text) = val.get("logs").and_then(Value::as_str) {
            print!("{text}");
            return Ok(());
        }
    }

    println!(
        "No logs available yet for `{}` (namespace `{}`).",
        app.name, app.namespace
    );
    Ok(())
}

// ── Shared helpers ───────────────────────────────────────────────────────────

pub(crate) fn local_kube_client() -> Option<up::KubeClient> {
    let kc = up::kubeconfig_path().ok()?;
    if !kc.is_file() {
        return None;
    }
    let dir = up::up_dir().ok()?;
    if up::running(&dir, up::SUPERVISOR).is_none() && !up::host_native_k3s_active() {
        return None;
    }
    up::KubeClient::from_kubeconfig(&kc).ok()
}

pub(crate) async fn local_engine_healthy() -> bool {
    let Ok(client) = reqwest::Client::builder()
        .timeout(std::time::Duration::from_millis(300))
        .build()
    else {
        return false;
    };
    for url in [
        "http://127.0.0.1:1234/v1/models",
        "http://127.0.0.1:1235/health",
    ] {
        if client
            .get(url)
            .send()
            .await
            .is_ok_and(|r| r.status().is_success())
        {
            return true;
        }
    }
    false
}

pub(crate) fn boot_from_state(state: &super::project::DesiredState) -> up::Boot {
    up::Boot {
        cpus: state.runtime.cpus.max(1),
        memory_mb: state.runtime.memory_mb.max(256),
        disk_mb: state.runtime.disk_mb.max(1024),
        cloud: state.platform.cloud_image.clone(),
        staged: state.platform.staged.clone(),
        iam_issuer: state.platform.iam_issuer.clone(),
        ai_base_url: state.platform.ai_base_url.clone(),
        strict_registry: state.runtime.strict_registry,
        local: state.platform.cloud_local.clone(),
    }
}

pub(crate) fn color_state(st: &str) -> ColoredString {
    match st.to_ascii_lowercase().as_str() {
        "running" | "ready" | "ok" | "healthy" => st.green(),
        "starting" | "desired" | "warn" => st.yellow(),
        "failed" | "error" | "fail" => st.red(),
        _ => st.dimmed(),
    }
}
