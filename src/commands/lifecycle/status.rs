//! Multi-layer `hanzo ps`, `hanzo status` local 3-layer view, and `hanzo doctor`
//! local runtime/platform/application health checks.

use anyhow::Result;
use colored::*;
use hanzo_client::Method;
use serde_json::{json, Value};
use std::path::Path;

use crate::commands::{daemon, host, up, vm};
use crate::config::Config;

use super::client::{
    call_lifecycle, DoctorCheck, DoctorResponse, HanzodOutcome, PsResponse, PsRow,
};
use super::project::{load_state, RegisteredApp};
use super::resources::{color_state, local_engine_healthy, local_kube_client};
use super::units::format_size_mb;

fn app_ps_row(app: &RegisteredApp, live: bool, k3s_up: bool) -> PsRow {
    let port = if app.image.is_some() {
        app.node_port
    } else {
        app.container_port
    };
    let endpoint = format!("http://127.0.0.1:{port}");
    let running = live || (app.image.is_some() && app.desired_running && k3s_up);
    PsRow {
        component: app.name.clone(),
        layer: "application".to_string(),
        status: if running {
            "Running".into()
        } else {
            "Stopped".into()
        },
        detail: format!("ns/{} · {endpoint}", app.namespace),
        namespace: Some(app.namespace.clone()),
        endpoint: Some(endpoint),
    }
}

/// `hanzo ps` — render the §5 multi-layer local status table (`Infrastructure`,
/// `Platform`, `Applications`, `Sandboxes`).
pub async fn ps(as_json: bool) -> Result<()> {
    let hanzod = call_lifecycle(None, Method::GET, "ps", None, false).await?;
    let mut rows = match hanzod {
        HanzodOutcome::Handled(val) => serde_json::from_value::<PsResponse>(val)
            .map(|r| r.rows)
            .unwrap_or_default(),
        HanzodOutcome::Unsupported => Vec::new(),
    };

    if rows.is_empty() {
        rows = build_local_ps_rows().await?;
    } else if let Ok(state) = load_state() {
        let k3s_up = up::up_dir()
            .ok()
            .and_then(|d| up::running(&d, up::SUPERVISOR))
            .is_some()
            || up::host_native_k3s_active();
        for app in state.apps.values() {
            if !rows
                .iter()
                .any(|r| r.component.eq_ignore_ascii_case(&app.name))
            {
                let live = app.local_pid.is_some_and(|p| vm::alive(p as i32));
                rows.push(app_ps_row(app, live, k3s_up));
            }
        }
    }

    if as_json {
        println!("{}", serde_json::to_string_pretty(&PsResponse { rows })?);
        return Ok(());
    }

    println!(
        "{:<20} {:<16} {:<12} DETAIL",
        "COMPONENT", "LAYER", "STATUS"
    );
    for row in rows {
        let det = row.display_detail();
        println!(
            "{:<20} {:<16} {:<12} {}",
            row.component,
            row.layer.dimmed(),
            color_state(&row.status),
            if det.is_empty() { "-" } else { &det }
        );
    }
    Ok(())
}

pub(crate) async fn build_local_ps_rows() -> Result<Vec<PsRow>> {
    let state = load_state().unwrap_or_default();
    let dir = up::up_dir()?;
    let sup_pid = up::running(&dir, up::SUPERVISOR);
    let native = sup_pid.is_none() && up::host_native_k3s_active();
    let up_phase = up::read_state(&dir).unwrap_or_else(|| "stopped".into());

    let (rt_state, rt_detail) = if let Some(pid) = sup_pid {
        (
            "Running".to_string(),
            format!(
                "hanzo-vm (pid {pid}, {} cpus, {}, {up_phase})",
                state.runtime.cpus,
                format_size_mb(state.runtime.memory_mb)
            ),
        )
    } else if native {
        (
            "Running".to_string(),
            "native-linux (host k3s + containerd)".to_string(),
        )
    } else {
        (
            "Stopped".to_string(),
            format!(
                "{} ({} cpus, {} configured)",
                state.runtime.backend,
                state.runtime.cpus,
                format_size_mb(state.runtime.memory_mb)
            ),
        )
    };

    let kube = local_kube_client();
    let cloud_replicas = match &kube {
        Some(k) => k.cloud_replicas().await.ok().flatten(),
        None => None,
    };
    let k3s_up = sup_pid.is_some() || native;
    let k3s_state = if k3s_up {
        if kube.is_some() {
            "Ready"
        } else {
            "Starting"
        }
    } else {
        "Stopped"
    };

    let (cloud_state, cloud_detail) = match cloud_replicas {
        Some(n) if n > 0 => (
            "Running".to_string(),
            format!("http://127.0.0.1:3690 ({n} replica)"),
        ),
        Some(_) => (
            "Stopped".to_string(),
            "0 replicas on warm k3s (`hanzo up` to start)".to_string(),
        ),
        None if state.platform.cloud_enabled && k3s_up => (
            "Starting".to_string(),
            state.platform.cloud_image.clone(),
        ),
        None => ("Stopped".to_string(), "not deployed".to_string()),
    };

    let engine_live = local_engine_healthy().await;
    let (engine_state, engine_detail) = if engine_live {
        (
            "Running".to_string(),
            "http://127.0.0.1:1234 (/v1/chat/completions)".to_string(),
        )
    } else {
        ("Stopped".to_string(), "local inference standby".to_string())
    };

    let mut rows = vec![
        PsRow {
            component: "Hanzo Runtime".into(),
            layer: "infrastructure".into(),
            status: rt_state,
            detail: rt_detail,
            namespace: None,
            endpoint: None,
        },
        PsRow {
            component: "k3s".into(),
            layer: "infrastructure".into(),
            status: k3s_state.into(),
            detail: if k3s_up {
                format!("https://127.0.0.1:{}", up::K3S_PORT)
            } else {
                "cluster offline".into()
            },
            namespace: Some("kube-system".into()),
            endpoint: None,
        },
        PsRow {
            component: "Hanzo Cloud".into(),
            layer: "platform".into(),
            status: cloud_state,
            detail: cloud_detail,
            namespace: Some("hanzo".into()),
            endpoint: None,
        },
        PsRow {
            component: "Hanzo Engine".into(),
            layer: "platform".into(),
            status: engine_state,
            detail: engine_detail,
            namespace: Some("hanzo".into()),
            endpoint: None,
        },
    ];

    let cloud_running = cloud_replicas.is_some_and(|n| n > 0);
    for (svc_key, display, detail) in [
        ("analytics", "Analytics", "/v1/analytics & /v1/event"),
        ("insights", "Insights", "/v1/o11y logs, metrics & traces"),
    ] {
        let enabled = state
            .platform
            .services
            .get(svc_key)
            .map(|v| v.is_enabled())
            .unwrap_or(cloud_running);
        rows.push(PsRow {
            component: display.into(),
            layer: "platform".into(),
            status: if enabled && cloud_running {
                "Running".into()
            } else {
                "Stopped".into()
            },
            detail: detail.into(),
            namespace: Some("hanzo".into()),
            endpoint: None,
        });
    }

    for app in state.apps.values() {
        let mut live = app.local_pid.is_some_and(|p| vm::alive(p as i32));
        if !live {
            if let Some(ref k) = kube {
                live = k
                    .deployment_replicas(&app.namespace, &app.slug)
                    .await
                    .ok()
                    .flatten()
                    .is_some_and(|r| r > 0);
            }
        }
        rows.push(app_ps_row(app, live, k3s_up));
    }

    let dev_health = daemon::probe_dev_health().await;
    let (dev_state, dev_detail) = match dev_health {
        Some(ref d) => {
            let active = d
                .get("active_sessions")
                .and_then(Value::as_u64)
                .unwrap_or(0);
            let total = d.get("sessions").and_then(Value::as_u64).unwrap_or(0);
            if active > 0 {
                (
                    "Running".to_string(),
                    format!("{active} active ({total} total in hanzod)"),
                )
            } else {
                (
                    "Ready".to_string(),
                    format!("hanzod online ({total} saved sessions)"),
                )
            }
        }
        None => ("Stopped".to_string(), "hanzod idle".to_string()),
    };
    rows.push(PsRow {
        component: "Hanzo Dev".into(),
        layer: "application".into(),
        status: dev_state,
        detail: dev_detail,
        namespace: None,
        endpoint: None,
    });

    for sbx in state.sandboxes.values() {
        rows.push(PsRow {
            component: format!("sandbox/{}", sbx.name),
            layer: "sandbox".into(),
            status: if sbx.desired_running {
                "Running".into()
            } else {
                "Stopped".into()
            },
            detail: format!("{} on {}", sbx.image, sbx.node),
            namespace: None,
            endpoint: None,
        });
    }

    Ok(rows)
}

/// Collect the full 3-layer local snapshot (`runtime`, `platform`, `applications`,
/// `sandboxes`) for `hanzo status` and `hanzo vm status`.
pub async fn collect_local_snapshot(cfg: Option<&mut Config>) -> Result<Value> {
    if let HanzodOutcome::Handled(val) =
        call_lifecycle(cfg, Method::GET, "status", None, false).await?
    {
        if val.get("runtime").is_some() && val.get("platform").is_some() {
            return Ok(val);
        }
    }

    let state = load_state().unwrap_or_default();
    let dir = up::up_dir()?;
    let sup_pid = up::running(&dir, up::SUPERVISOR);
    let native = sup_pid.is_none() && up::host_native_k3s_active();
    let kc = up::kubeconfig_path().ok().filter(|p| p.is_file());
    let kube = local_kube_client();
    let cloud_replicas = match &kube {
        Some(k) => k.cloud_replicas().await.ok().flatten(),
        None => None,
    };
    let engine_live = local_engine_healthy().await;
    let hanzod_up = daemon::probe_dev_health().await.is_some();

    let apps: Vec<Value> = state
        .apps
        .values()
        .map(|a| {
            json!({
                "slug": a.slug,
                "name": a.name,
                "namespace": a.namespace,
                "state": if a.desired_running { "Running" } else { "Stopped" },
                "project_dir": a.project_dir,
            })
        })
        .collect();
    let sandboxes: Vec<Value> = state
        .sandboxes
        .values()
        .map(|s| {
            json!({
                "name": s.name,
                "image": s.image,
                "node": s.node,
                "state": if s.desired_running { "Running" } else { "Stopped" },
            })
        })
        .collect();

    Ok(json!({
        "runtime": {
            "state": if sup_pid.is_some() || native { "Running" } else { "Stopped" },
            "backend": if native { "native-linux" } else { &state.runtime.backend },
            "cpus": state.runtime.cpus,
            "memory_mb": state.runtime.memory_mb,
            "disk_mb": state.runtime.disk_mb,
            "k3s": if sup_pid.is_some() || native { "Ready" } else { "Stopped" },
            "hanzod": if hanzod_up { "Running" } else { "Stopped" },
            "kubeconfig": kc.map(|p| p.display().to_string()),
        },
        "platform": {
            "cloud": if cloud_replicas.is_some_and(|n| n > 0) { "Running" } else { "Stopped" },
            "cloud_replicas": cloud_replicas.unwrap_or(0),
            "engine": if engine_live { "Running" } else { "Stopped" },
        },
        "applications": apps,
        "sandboxes": sandboxes,
    }))
}

/// Render the 3-layer local system block prominently for `hanzo status`.
pub async fn render_local_status_section(cfg: &mut Config) -> Result<()> {
    let rows = build_local_ps_rows_with_hanzod(cfg).await?;
    println!("{}", "local system (3-layer)".bold());
    for row in &rows {
        let det = row.display_detail();
        println!(
            "  {:<18} {:<15} {:<10} {}",
            row.component,
            row.layer.dimmed(),
            color_state(&row.status),
            if det.is_empty() {
                "-".dimmed()
            } else {
                det.dimmed()
            }
        );
    }
    println!();
    Ok(())
}

async fn build_local_ps_rows_with_hanzod(cfg: &mut Config) -> Result<Vec<PsRow>> {
    if let HanzodOutcome::Handled(val) =
        call_lifecycle(Some(cfg), Method::GET, "ps", None, false).await?
    {
        if let Ok(mut parsed) = serde_json::from_value::<PsResponse>(val) {
            if !parsed.rows.is_empty() {
                if let Ok(state) = load_state() {
                    let k3s_up = up::up_dir()
                        .ok()
                        .and_then(|d| up::running(&d, up::SUPERVISOR))
                        .is_some()
                        || up::host_native_k3s_active();
                    for app in state.apps.values() {
                        if !parsed
                            .rows
                            .iter()
                            .any(|r| r.component.eq_ignore_ascii_case(&app.name))
                        {
                            let live = app.local_pid.is_some_and(|p| vm::alive(p as i32));
                            parsed.rows.push(app_ps_row(app, live, k3s_up));
                        }
                    }
                }
                return Ok(parsed.rows);
            }
        }
    }
    build_local_ps_rows().await
}

/// Collect 3-layer diagnostic checks (`Infrastructure`, `Platform`, `Applications`)
/// for `hanzo doctor`.
pub async fn doctor_checks(cfg: &mut Config) -> Result<DoctorResponse> {
    if let HanzodOutcome::Handled(val) =
        call_lifecycle(Some(cfg), Method::GET, "doctor", None, false).await?
    {
        if let Ok(parsed) = serde_json::from_value::<DoctorResponse>(val) {
            if !parsed.checks.is_empty() {
                return Ok(parsed);
            }
        }
    }

    let mut checks = Vec::new();
    let hanzod_running =
        host::running_pid().is_some() || daemon::probe_dev_health().await.is_some();
    let hanzod_installed = which::which("hanzod").is_ok()
        || dirs::home_dir().is_some_and(|h| h.join(".hanzo/bin/hanzod").is_file());
    checks.push(DoctorCheck {
        layer: "infrastructure".into(),
        name: "hanzod".into(),
        status: if hanzod_running || hanzod_installed {
            "ok".into()
        } else {
            "warn".into()
        },
        detail: if hanzod_running {
            "daemon online on local ZAP/HTTP".into()
        } else if hanzod_installed {
            "installed (auto-starts on demand)".into()
        } else {
            "not installed; run `hanzo upgrade hanzod`".into()
        },
    });

    let vm_installed = which::which("hanzo-vm").is_ok()
        || dirs::home_dir().is_some_and(|h| h.join(".local/bin/hanzo-vm").is_file());
    let native_k3s = up::host_native_k3s_active();
    let sup_running = up::up_dir()
        .ok()
        .and_then(|d| up::running(&d, up::SUPERVISOR))
        .is_some();
    checks.push(DoctorCheck {
        layer: "infrastructure".into(),
        name: "Hanzo Runtime".into(),
        status: if sup_running || native_k3s || vm_installed {
            "ok".into()
        } else {
            "info".into()
        },
        detail: if sup_running {
            "hanzo-vm supervisor active".into()
        } else if native_k3s {
            "native Linux k3s active on 127.0.0.1:6443".into()
        } else if vm_installed {
            "hanzo-vm installed (stopped — boot with `hanzo start` or `hanzo up`)".into()
        } else {
            "hanzo-vm bootstraps automatically on first `hanzo start` / `hanzo up`".into()
        },
    });

    let kc = up::kubeconfig_path().ok();
    let kc_exists = kc.as_ref().is_some_and(|p| p.is_file());
    checks.push(DoctorCheck {
        layer: "infrastructure".into(),
        name: "k3s kubeconfig".into(),
        status: if kc_exists { "ok".into() } else { "info".into() },
        detail: match kc {
            Some(p) if kc_exists => format!("present at {}", p.display()),
            Some(p) => format!("{} not created yet (`hanzo start` writes it)", p.display()),
            None => "no HOME directory".into(),
        },
    });

    let cloud_up = match local_kube_client() {
        Some(k) => k.cloud_replicas().await.ok().flatten().is_some_and(|n| n > 0),
        None => false,
    };
    checks.push(DoctorCheck {
        layer: "platform".into(),
        name: "Hanzo Cloud".into(),
        status: if cloud_up { "ok".into() } else { "info".into() },
        detail: if cloud_up {
            "running in local k3s namespace `hanzo`".into()
        } else {
            "stopped locally (`hanzo up` to start)".into()
        },
    });

    let eng_up = local_engine_healthy().await;
    checks.push(DoctorCheck {
        layer: "platform".into(),
        name: "Hanzo Engine".into(),
        status: if eng_up { "ok".into() } else { "info".into() },
        detail: if eng_up {
            "serving local /v1 inference".into()
        } else {
            "standby (`hanzo up --ai-only` or `hanzo model run <model>` to start)".into()
        },
    });

    let state = load_state().unwrap_or_default();
    for app in state.apps.values() {
        let dir_exists = Path::new(&app.project_dir).is_dir();
        checks.push(DoctorCheck {
            layer: "application".into(),
            name: app.name.clone(),
            status: if dir_exists { "ok".into() } else { "warn".into() },
            detail: if dir_exists {
                format!(
                    "{} ({})",
                    if app.desired_running {
                        "desired running"
                    } else {
                        "stopped"
                    },
                    app.project_dir
                )
            } else {
                format!("project directory missing: {}", app.project_dir)
            },
        });
    }

    let ok = checks.iter().all(|c| c.status != "fail");
    Ok(DoctorResponse {
        ok,
        checks,
        summary: None,
    })
}

#[cfg(test)]
mod status_row_tests {
    use super::*;

    fn sample_app(image: Option<&str>) -> RegisteredApp {
        RegisteredApp {
            slug: "sample".into(),
            name: "Sample".into(),
            project_dir: "/tmp/sample".into(),
            namespace: "hanzo-app-sample".into(),
            kind: "package.json".into(),
            image: image.map(str::to_string),
            command: Some("node index.js".into()),
            container_port: 3000,
            node_port: 31234,
            desired_running: true,
            local_pid: None,
        }
    }

    #[test]
    fn host_app_without_live_pid_is_stopped_even_when_k3s_up() {
        let host_app = sample_app(None);
        let dead_row = app_ps_row(&host_app, false, true);
        assert_eq!(dead_row.status, "Stopped");
        assert_eq!(dead_row.endpoint.as_deref(), Some("http://127.0.0.1:3000"));

        let live_row = app_ps_row(&host_app, true, false);
        assert_eq!(live_row.status, "Running");
    }

    #[test]
    fn containerized_app_uses_node_port_and_k3s_state() {
        let k8s_app = sample_app(Some("hanzo.local/sample:dev"));
        let row = app_ps_row(&k8s_app, false, true);
        assert_eq!(row.status, "Running");
        assert_eq!(row.endpoint.as_deref(), Some("http://127.0.0.1:31234"));
    }
}
