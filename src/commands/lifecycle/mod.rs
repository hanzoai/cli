//! Canonical 3-Layer Hanzo CLI Lifecycle Controller (`Infrastructure` →
//! `Platform` → `Applications & Sandboxes`).
//!
//! Coordinates with `hanzod` over ZAP (`/v1/lifecycle/*`) when available and
//! seamlessly reconciles local desired state (`~/.hanzo/lifecycle/state.json`)
//! when `hanzod` is absent or running an older release (`404`).

pub mod client;
pub mod project;
pub mod resources;
pub mod status;
pub mod units;
pub mod workloads;

#[cfg(test)]
mod tests;

pub use status::{
    collect_local_snapshot, doctor_checks, ps, render_local_status_section,
};

use anyhow::Result;
use colored::*;
use hanzo_client::Method;
use std::process::Command;
use std::time::Duration;

use crate::commands::{engine, up, vm};
use crate::config::Config;

use client::{call_lifecycle, EnvironmentRequest, ResetRequest, RuntimeRequest};
use project::{
    detect_project, lifecycle_dir, load_state, resolve_registered_app, save_state,
    upsert_detected_app,
};
use resources::{
    boot_from_state, deploy_or_spawn_app, local_engine_healthy, local_kube_client,
    stop_registered_app_instance,
};
use units::format_size_mb;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpMode {
    Full,
    AiOnly,
    CloudOnly,
}

impl UpMode {
    pub fn as_str(&self) -> &'static str {
        match self {
            UpMode::Full => "full",
            UpMode::AiOnly => "ai-only",
            UpMode::CloudOnly => "cloud-only",
        }
    }
}

/// `hanzo start` — boot Layer 1 infrastructure (`hanzo-vm` or native Linux `k3s`
/// + `containerd`) ONLY, without deploying Hanzo Cloud or user applications.
pub async fn runtime_start(
    cfg: &mut Config,
    cpus: u32,
    memory_mb: u64,
    disk_mb: u64,
    native: bool,
    strict_registry: bool,
) -> Result<()> {
    let backend = if native { "native-linux" } else { "auto" };
    let mut state = load_state().unwrap_or_default();
    state.runtime.cpus = cpus;
    state.runtime.memory_mb = memory_mb;
    state.runtime.disk_mb = disk_mb;
    state.runtime.backend = backend.to_string();
    state.runtime.strict_registry = strict_registry;
    state.runtime.desired_running = true;
    save_state(&state)?;

    let req = RuntimeRequest {
        action: "start".to_string(),
        cpus: Some(cpus),
        memory_mb: Some(memory_mb),
        disk_mb: Some(disk_mb),
        backend: Some(backend.to_string()),
        strict_registry: Some(strict_registry),
    };
    let _ = call_lifecycle(
        Some(cfg),
        Method::POST,
        "runtime",
        Some(serde_json::to_value(&req)?),
        true,
    )
    .await?;

    let boot = boot_from_state(&state);
    up::start(cfg, boot, native).await
}

/// `hanzo stop` — gracefully drain workloads (apps + Cloud), stop `k3s` & VM,
/// release ports/CPU/memory, and leave `hanzod` running.
pub async fn runtime_stop(cfg: &mut Config) -> Result<()> {
    let req = RuntimeRequest {
        action: "stop".to_string(),
        cpus: None,
        memory_mb: None,
        disk_mb: None,
        backend: None,
        strict_registry: None,
    };
    let _ = call_lifecycle(
        Some(cfg),
        Method::POST,
        "runtime",
        Some(serde_json::to_value(&req)?),
        false,
    )
    .await?;

    let mut state = load_state().unwrap_or_default();
    state.runtime.desired_running = false;
    let kube = local_kube_client();
    for app in state.apps.values_mut() {
        if let Some(pid) = app.local_pid.take() {
            let _ = up::stop(pid as i32, Duration::from_secs(5));
        }
        if let Some(ref k) = kube {
            let _ = k.scale_deployment(&app.namespace, &app.slug, 0).await;
        }
    }
    save_state(&state)?;

    up::stop_runtime().await
}

/// `hanzo restart` — restart Layer 1 runtime and restore ALL workloads (Cloud,
/// Engine, and every user app in `~/.hanzo/lifecycle/state.json` with
/// `desired_running == true`) that were active prior to restart.
pub async fn runtime_restart(cfg: &mut Config, boot: up::Boot, native: bool) -> Result<()> {
    let mut state = load_state().unwrap_or_default();
    let live_cloud = match local_kube_client() {
        Some(k) => k.cloud_replicas().await.ok().flatten().is_some_and(|r| r > 0),
        None => false,
    };
    let restore_cloud = state.platform.cloud_enabled || live_cloud;
    // Whether what runs now is this machine's own build, read off the cluster
    // before it is stopped: the Deployment says what is deployed, the state file
    // only what was last asked for.
    let live_local = match local_kube_client() {
        Some(k) => k
            .get("/apis/apps/v1/namespaces/hanzo/deployments/cloud")
            .await
            .ok()
            .flatten()
            .and_then(|d| {
                d.pointer("/spec/template/spec/containers/0/image")
                    .and_then(|i| i.as_str())
                    .map(|i| i.starts_with("hanzo.local/"))
            })
            .unwrap_or(false),
        None => false,
    };
    let restore_engine = state.platform.engine_enabled || local_engine_healthy().await;
    let restore_apps: Vec<project::RegisteredApp> = state
        .apps
        .values()
        .filter(|a| a.desired_running)
        .cloned()
        .collect();

    state.runtime.cpus = boot.cpus;
    state.runtime.memory_mb = boot.memory_mb;
    state.runtime.disk_mb = boot.disk_mb;
    state.runtime.strict_registry = boot.strict_registry;
    state.runtime.desired_running = true;
    save_state(&state)?;

    let req = RuntimeRequest {
        action: "restart".to_string(),
        cpus: Some(boot.cpus),
        memory_mb: Some(boot.memory_mb),
        disk_mb: Some(boot.disk_mb),
        backend: Some(if native { "native-linux" } else { "auto" }.into()),
        strict_registry: Some(boot.strict_registry),
    };
    let _ = call_lifecycle(
        Some(cfg),
        Method::POST,
        "runtime",
        Some(serde_json::to_value(&req)?),
        true,
    )
    .await?;

    for app in state.apps.values_mut() {
        if let Some(pid) = app.local_pid.take() {
            let _ = up::stop(pid as i32, Duration::from_secs(5));
        }
    }
    save_state(&state)?;
    up::stop_runtime().await?;

    if restore_cloud {
        state.platform.cloud_enabled = true;
        save_state(&state)?;
        // What ran before the restart is what runs after it: a local build is
        // loaded again from the layout it came from, not replaced by a published
        // image because the restart's flags name the default one.
        let boot = match &state.platform.cloud_local {
            Some(layout) if live_local && boot.cloud == up::CLOUD => up::Boot {
                local: Some(layout.clone()),
                ..boot
            },
            _ => boot,
        };
        up::up(cfg, boot, None, false, true).await?;
    } else {
        up::start(cfg, boot, native).await?;
    }

    if restore_engine {
        let _ = engine::up().await;
    }

    for app in &restore_apps {
        deploy_or_spawn_app(&mut state, app).await?;
    }
    save_state(&state)?;
    Ok(())
}

/// `hanzo vm resize` — update CPU/memory/disk allocation, resizing offline disk
/// checkpoints if needed and restarting runtime if currently running.
pub async fn runtime_resize(
    cfg: &mut Config,
    cpus: Option<u32>,
    memory_mb: Option<u64>,
    disk_mb: Option<u64>,
) -> Result<()> {
    if cpus.is_none() && memory_mb.is_none() && disk_mb.is_none() {
        anyhow::bail!("specify at least one of `--cpus`, `--memory`, or `--disk`");
    }
    let mut state = load_state().unwrap_or_default();
    if let Some(c) = cpus {
        state.runtime.cpus = c.max(1);
    }
    if let Some(m) = memory_mb {
        state.runtime.memory_mb = m.max(256);
    }
    if let Some(d) = disk_mb {
        state.runtime.disk_mb = d.max(1024);
    }
    save_state(&state)?;

    let req = RuntimeRequest {
        action: "resize".to_string(),
        cpus,
        memory_mb,
        disk_mb,
        backend: Some(state.runtime.backend.clone()),
        strict_registry: Some(state.runtime.strict_registry),
    };
    let _ = call_lifecycle(
        Some(cfg),
        Method::POST,
        "runtime",
        Some(serde_json::to_value(&req)?),
        true,
    )
    .await?;

    let is_running = up::up_dir()
        .ok()
        .and_then(|d| up::running(&d, up::SUPERVISOR))
        .is_some();

    if !is_running {
        if let Some(d) = disk_mb {
            if let Ok(bin) = vm::resolve_or_install().await {
                let cp = up::checkpoint();
                let _ = Command::new(&bin)
                    .args(["resize", "--checkpoint", &cp, "--disk", &d.to_string()])
                    .status();
            }
        }
        println!(
            "{} configured Hanzo Runtime shape → {} CPUs · {} RAM · {} disk (applies on next `hanzo start` / `hanzo up`)",
            "✓".green(),
            state.runtime.cpus,
            format_size_mb(state.runtime.memory_mb),
            format_size_mb(state.runtime.disk_mb),
        );
        return Ok(());
    }

    let boot = boot_from_state(&state);
    let native = state.runtime.backend == "native-linux";
    runtime_restart(cfg, boot, native).await?;
    println!(
        "{} resized & restarted Hanzo Runtime → {} CPUs · {} RAM · {} disk",
        "✓".green(),
        state.runtime.cpus,
        format_size_mb(state.runtime.memory_mb),
        format_size_mb(state.runtime.disk_mb),
    );
    Ok(())
}

/// `hanzo up` — bring up the developer environment (`full`, `ai-only`, or `cloud-only`).
#[allow(clippy::too_many_arguments)]
pub async fn env_up(
    cfg: &mut Config,
    boot: up::Boot,
    mode: UpMode,
    link: Option<String>,
    ui: bool,
    no_ui: bool,
    native: bool,
    local: Option<String>,
) -> Result<()> {
    if let Some(arg) = local {
        return env_up_local(cfg, boot, &arg, link, ui, no_ui).await;
    }
    let cwd = std::env::current_dir().ok();
    let project_dir_str = cwd
        .as_ref()
        .map(|p| std::fs::canonicalize(p).unwrap_or_else(|_| p.clone()))
        .map(|p| p.to_string_lossy().into_owned());

    let mut state = load_state().unwrap_or_default();
    state.runtime.cpus = boot.cpus;
    state.runtime.memory_mb = boot.memory_mb;
    state.runtime.disk_mb = boot.disk_mb;
    state.runtime.strict_registry = boot.strict_registry;
    state.runtime.desired_running = true;
    if native {
        state.runtime.backend = "native-linux".to_string();
    }

    match mode {
        UpMode::AiOnly => {
            state.platform.engine_enabled = true;
        }
        UpMode::CloudOnly | UpMode::Full => {
            state.platform.cloud_enabled = true;
            state.platform.cloud_image = boot.cloud.clone();
            if boot.staged.is_some() {
                state.platform.staged = boot.staged.clone();
            }
            if boot.iam_issuer.is_some() {
                state.platform.iam_issuer = boot.iam_issuer.clone();
            }
            if boot.ai_base_url.is_some() {
                state.platform.ai_base_url = boot.ai_base_url.clone();
            }
        }
    }
    save_state(&state)?;

    let req = EnvironmentRequest {
        action: "up".to_string(),
        mode: Some(mode.as_str().to_string()),
        no_ui: Some(no_ui || !ui),
        project_dir: project_dir_str,
        scope: Some("auto".to_string()),
        cloud_image: Some(boot.cloud.clone()),
        staged: boot.staged.clone(),
        iam_issuer: boot.iam_issuer.clone(),
        ai_base_url: boot.ai_base_url.clone(),
        strict_registry: Some(boot.strict_registry),
    };
    let _ = call_lifecycle(
        Some(cfg),
        Method::POST,
        "environment",
        Some(serde_json::to_value(&req)?),
        true,
    )
    .await?;

    if mode == UpMode::AiOnly {
        let runtime_running = up::up_dir()
            .ok()
            .and_then(|d| up::running(&d, up::SUPERVISOR))
            .is_some()
            || up::host_native_k3s_active();
        if !runtime_running && native {
            let _ = up::start(cfg, boot, true).await;
        }
        engine::up().await?;
        println!(
            "{} AI-only environment ready (Cloud workloads skipped — run `hanzo up` for full platform)",
            "✓".green()
        );
        return Ok(());
    }

    let detected = if mode == UpMode::Full {
        match cwd.as_deref() {
            Some(dir) => detect_project(dir)?,
            None => None,
        }
    } else {
        None
    };

    up::up(cfg, boot, link, false, true).await?;

    if let Some(proj) = detected {
        let app = upsert_detected_app(&mut state, &proj);
        deploy_or_spawn_app(&mut state, &app).await?;
        save_state(&state)?;
    }

    if ui || (!no_ui && std::io::IsTerminal::is_terminal(&std::io::stdout())) {
        return up::dashboard();
    }
    Ok(())
}

/// `hanzo up --local` — Cloud from this machine's source.
///
/// `hanzod` is not asked to bring the environment up, as it is for a published
/// image: its request names an image to deploy, and here there is none to name. The
/// local build runs first, and only a build that succeeded reaches the runtime.
async fn env_up_local(
    cfg: &mut Config,
    mut boot: up::Boot,
    arg: &str,
    link: Option<String>,
    ui: bool,
    no_ui: bool,
) -> Result<()> {
    let cwd = std::env::current_dir()?;
    let (layout, built) = up::prepare_local(arg, &cwd)?;
    boot.local = Some(layout.clone());

    let mut state = load_state().unwrap_or_default();
    state.runtime.desired_running = true;
    state.platform.cloud_enabled = true;
    state.platform.cloud_local = Some(layout);
    if boot.staged.is_some() {
        state.platform.staged = boot.staged.clone();
    }
    if boot.iam_issuer.is_some() {
        state.platform.iam_issuer = boot.iam_issuer.clone();
    }
    if boot.ai_base_url.is_some() {
        state.platform.ai_base_url = boot.ai_base_url.clone();
    }
    save_state(&state)?;

    up::up_local(cfg, boot, Some(built), link, ui, no_ui).await
}

/// `hanzo down` — stop workloads belonging to the selected environment while
/// preserving running VM & `k3s`, image caches, volumes, model weights,
/// `kube-system`, unrelated projects, and local networking.
pub async fn env_down(
    cfg: &mut Config,
    stop_vm: bool,
    cloud_scope: bool,
    all_workloads: bool,
) -> Result<()> {
    if stop_vm {
        return runtime_stop(cfg).await;
    }

    let cwd = std::env::current_dir().ok();
    let project_dir_str = cwd
        .as_ref()
        .map(|p| std::fs::canonicalize(p).unwrap_or_else(|_| p.clone()))
        .map(|p| p.to_string_lossy().into_owned());

    let scope = if all_workloads {
        "all"
    } else if cloud_scope {
        "cloud"
    } else {
        "auto"
    };

    let req = EnvironmentRequest {
        action: "down".to_string(),
        mode: None,
        no_ui: None,
        project_dir: project_dir_str,
        scope: Some(scope.to_string()),
        cloud_image: None,
        staged: None,
        iam_issuer: None,
        ai_base_url: None,
        strict_registry: None,
    };
    let _ = call_lifecycle(
        Some(cfg),
        Method::POST,
        "environment",
        Some(serde_json::to_value(&req)?),
        false,
    )
    .await?;

    let mut state = load_state().unwrap_or_default();

    if !cloud_scope && !all_workloads {
        if let Ok(app) = resolve_registered_app(&state, None, cwd.as_deref()) {
            let slug = app.slug.clone();
            let display_name = app.name.clone();
            stop_registered_app_instance(&mut state, &slug).await?;
            save_state(&state)?;
            println!(
                "{} stopped application `{}` (Hanzo Runtime, k3s, Cloud & other projects kept warm)",
                "✓".green(),
                display_name.bold()
            );
            return Ok(());
        }
    }

    if all_workloads {
        let slugs: Vec<String> = state.apps.keys().cloned().collect();
        for slug in slugs {
            let _ = stop_registered_app_instance(&mut state, &slug).await;
        }
    }
    state.platform.cloud_enabled = false;
    save_state(&state)?;

    up::down(false).await
}

/// `hanzo reset [--all]` — stop runtime, wipe `~/.hanzo/up`, `~/.hanzo/lifecycle`,
/// and `~/.kube/hanzo.yaml` (plus cached VM checkpoints when `--all`).
pub async fn reset(cfg: &mut Config, all: bool) -> Result<()> {
    let req = ResetRequest { all };
    let _ = call_lifecycle(
        Some(cfg),
        Method::POST,
        "reset",
        Some(serde_json::to_value(&req)?),
        false,
    )
    .await?;

    if let Ok(mut state) = load_state() {
        for app in state.apps.values_mut() {
            if let Some(pid) = app.local_pid.take() {
                let _ = up::stop(pid as i32, Duration::from_secs(3));
            }
        }
    }
    if let Ok(lc_dir) = lifecycle_dir() {
        if lc_dir.exists() {
            let _ = std::fs::remove_dir_all(&lc_dir);
            println!("{} removed {}", "✓".green(), lc_dir.display());
        }
    }
    up::reset(all).await
}
