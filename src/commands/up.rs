//! `hanzo up` — a local Kubernetes running the cloud, in a Hanzo microVM (or
//! native Linux k3s runtime when requested / active).
//!
//! Bare `hanzo up` boots k3s inside a `hanzo-vm` microVM (or reuses/starts
//! native Linux k3s when `--native` / `HANZO_RUNTIME=native` or host k3s is
//! active), deploys the Hanzo cloud into it, and hands back `~/.kube/hanzo.yaml`.

use anyhow::{anyhow, bail, Context, Result};
use colored::*;
use std::io::IsTerminal;
use std::net::{SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::commands::host;
#[cfg(test)]
use crate::commands::host::key;
use crate::commands::link;
use crate::commands::vm::{self, alive};
use crate::config::Config;
use crate::image;

pub mod compute;
pub(crate) mod kube;
pub(crate) mod manifest;
pub mod savings;
pub(crate) mod supervisor;
pub mod tui;

pub(crate) use kube::KubeClient;
pub use manifest::{discover_app_manifest, select_cloud_image};
#[cfg(test)]
use manifest::ground;
use manifest::{has_workload_overrides, install_cmd, workload_for};
pub use supervisor::{attest, supervise};
#[cfg(test)]
pub(crate) use supervisor::{deployed, measured, node_ready, record, run_args, server_argv};
pub(crate) use supervisor::{rewrite_server, write_kubeconfig};

#[cfg(test)]
mod tests;

/// The VM's shape and what it runs — one value through every layer, so the
/// supervisor boots exactly what the caller asked for.
#[derive(Clone, Debug)]
pub struct Boot {
    pub cpus: u32,
    pub memory_mb: u64,
    pub disk_mb: u64,
    pub cloud: String,
    pub staged: Option<String>,
    pub iam_issuer: Option<String>,
    pub ai_base_url: Option<String>,
    pub strict_registry: bool,
}

pub(crate) const K3S_PORT: u16 = 6443;
pub(crate) const CLOUD_PORT: u16 = 8080;
pub(crate) const CLOUD_NODE_PORT: u16 = 30080;
pub(crate) const LOCAL_PORT: u16 = 36930;

/// Default OSS Cloud image (`github.com/hanzoai/cloud`).
pub const CLOUD: &str = "ghcr.io/hanzoai/cloud-oss:main";
/// Canonical Hanzo-owned OCI registry image (`cli#57`).
pub const CLOUD_OCI: &str = "oci.hanzo.ai/hanzoai/cloud-oss:main";

pub(crate) const DATA: &str = "/var/lib/hanzo";
pub(crate) const KUBECONFIG: &str = "/etc/hanzo/kubeconfig";
pub(crate) const GROUND: &str = "/var/lib/hanzo/server/manifests/cloud-ns.yaml";
pub(crate) const WORKLOAD: &str = "/var/lib/hanzo/server/manifests/cloud.yaml";
pub(crate) const KEYFILE: &str = "/dev/shm/.hanzo-cloud-master";
pub(crate) const KEYFILE_FALLBACK: &str = "/run/.hanzo-cloud-master";
pub(crate) const PULLFILE: &str = "/dev/shm/.hanzo-registry-pull";
pub(crate) const PULLFILE_FALLBACK: &str = "/run/.hanzo-registry-pull";
const PULL_SECRET: &str = "registry-pull";

pub(crate) fn checkpoint() -> String {
    format!("k3s-{}-airgap", vm::VM_VERSION)
}

const READY_TIMEOUT: Duration = Duration::from_secs(180);
const UP_TIMEOUT: Duration = Duration::from_secs(300);

pub(crate) const SUPERVISOR: &str = "supervisor";
pub(crate) const VM: &str = "vm";

pub(crate) fn up_dir() -> Result<PathBuf> {
    let d = dirs::home_dir()
        .ok_or_else(|| anyhow!("no home directory"))?
        .join(".hanzo")
        .join("up");
    std::fs::create_dir_all(&d).with_context(|| format!("creating {}", d.display()))?;
    Ok(d)
}

pub(crate) fn kubeconfig_path() -> Result<PathBuf> {
    Ok(dirs::home_dir()
        .ok_or_else(|| anyhow!("no home directory"))?
        .join(".kube")
        .join("hanzo.yaml"))
}

pub(crate) fn measure_path(dir: &Path) -> PathBuf {
    dir.join("measure.json")
}

pub(crate) fn write_pid(dir: &Path, who: &str, pid: u32) -> Result<()> {
    let f = dir.join(format!("{who}.pid"));
    std::fs::write(&f, pid.to_string()).with_context(|| format!("writing {}", f.display()))
}

pub(crate) fn read_pid(dir: &Path, who: &str) -> Option<i32> {
    std::fs::read_to_string(dir.join(format!("{who}.pid")))
        .ok()?
        .trim()
        .parse()
        .ok()
}

pub(crate) fn clear_pid(dir: &Path, who: &str) {
    let _ = std::fs::remove_file(dir.join(format!("{who}.pid")));
}

pub(crate) fn running(dir: &Path, who: &str) -> Option<i32> {
    read_pid(dir, who).filter(|p| alive(*p))
}

#[cfg(unix)]
pub(crate) fn stop(pid: i32, grace: Duration) -> bool {
    for (sig, patience) in [
        (libc::SIGTERM, grace),
        (libc::SIGKILL, Duration::from_secs(2)),
    ] {
        vm::signal(pid, sig);
        let deadline = Instant::now() + patience;
        while alive(pid) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(100));
        }
        if !alive(pid) {
            return true;
        }
    }
    false
}

#[cfg(windows)]
pub(crate) fn stop(pid: i32, _grace: Duration) -> bool {
    Command::new("taskkill")
        .args(["/PID", &pid.to_string(), "/T", "/F"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success() || !alive(pid))
}

#[cfg(all(not(unix), not(windows)))]
pub(crate) fn stop(_pid: i32, _grace: Duration) -> bool {
    false
}

pub(crate) fn write_state(dir: &Path, s: &str) {
    let _ = std::fs::write(dir.join("state"), s);
}

pub(crate) fn read_state(dir: &Path) -> Option<String> {
    std::fs::read_to_string(dir.join("state"))
        .ok()
        .map(|s| s.trim().to_string())
}

/// Detect whether host-native Linux `k3s` is active and listening on `127.0.0.1:6443`
/// without our `hanzo-vm` supervisor owning the port.
pub(crate) fn host_native_k3s_active() -> bool {
    if !cfg!(target_os = "linux") {
        return false;
    }
    if let Ok(dir) = up_dir() {
        if running(&dir, SUPERVISOR).is_some() || running(&dir, VM).is_some() {
            return false;
        }
    }
    let addr = SocketAddr::from(([127, 0, 0, 1], K3S_PORT));
    if TcpStream::connect_timeout(&addr, Duration::from_millis(250)).is_err() {
        return false;
    }
    Path::new("/etc/hanzo/kubeconfig").is_file()
        || Path::new("/etc/rancher/k3s/k3s.yaml").is_file()
        || kubeconfig_path().is_ok_and(|p| p.is_file())
}

/// Sync host-native Linux k3s kubeconfig into `~/.kube/hanzo.yaml` when readable.
pub(crate) fn adopt_native_linux_kubeconfig() -> Result<PathBuf> {
    let dest = kubeconfig_path()?;
    for src in ["/etc/hanzo/kubeconfig", "/etc/rancher/k3s/k3s.yaml"] {
        let p = Path::new(src);
        if let Ok(raw) = std::fs::read_to_string(p) {
            write_kubeconfig(&dest, &rewrite_server(&raw))?;
            return Ok(dest);
        }
    }
    if dest.is_file() {
        return Ok(dest);
    }
    bail!(
        "native Linux k3s requested, but no readable kubeconfig was found at \
         /etc/hanzo/kubeconfig, /etc/rancher/k3s/k3s.yaml, or {}",
        dest.display()
    )
}

#[cfg_attr(windows, allow(unreachable_code))]
pub async fn up(
    cfg: &mut Config,
    boot: Boot,
    link: Option<String>,
    ui: bool,
    no_ui: bool,
) -> Result<()> {
    let dir = up_dir()?;
    #[cfg(windows)]
    {
        let _ = (boot, ui, no_ui);
        let addr = format!("127.0.0.1:{LOCAL_PORT}");
        let existed = host::healthy(&addr).await;
        host::ensure(&addr).await?;
        write_state(&dir, "ready");
        let kc = kubeconfig_path()?;
        if !kc.exists() {
            if let Some(default_kc) = dirs::home_dir().map(|h| h.join(".kube").join("config")) {
                if default_kc.is_file() {
                    if let Some(parent) = kc.parent() {
                        let _ = std::fs::create_dir_all(parent);
                    }
                    let _ = std::fs::copy(&default_kc, &kc);
                }
            }
        }
        let verb = if existed {
            "already running"
        } else {
            "started"
        };
        println!(
            "{} local cloud {verb} at {}",
            "✓".green(),
            format!("http://{addr}").cyan()
        );
        endpoints()?;
        return finish_link(cfg, link).await;
    }

    let mut boot = boot;
    if let Ok(cwd) = std::env::current_dir() {
        if let Some(m) = discover_app_manifest(&cwd)? {
            if boot.cloud == CLOUD {
                if let Some(img) = m.cloud {
                    boot.cloud = img;
                }
            }
            if boot.staged.is_none() {
                boot.staged = m.staged;
            }
            if boot.iam_issuer.is_none() {
                boot.iam_issuer = m.iam_issuer;
            }
            if boot.ai_base_url.is_none() {
                boot.ai_base_url = m.ai_base_url;
            }
            boot.strict_registry = boot.strict_registry || m.strict_registry;
        }
    }

    let native_warm = running(&dir, SUPERVISOR).is_none() && host_native_k3s_active();
    if running(&dir, SUPERVISOR).is_some() || native_warm {
        let pid = running(&dir, SUPERVISOR).unwrap_or(0);
        let state = read_state(&dir).unwrap_or_else(|| {
            if native_warm {
                "native-linux".into()
            } else {
                "unknown".into()
            }
        });
        let kc = if native_warm {
            adopt_native_linux_kubeconfig()?
        } else {
            kubeconfig_path()?
        };
        let overrides = has_workload_overrides(&boot);
        let kube = KubeClient::from_kubeconfig(&kc)?;
        let replicas = kube.cloud_replicas().await?;
        if overrides || replicas.is_none() {
            let has_hanzo_oci =
                !image::credentials_for("oci.hanzo.ai", cfg).await.is_empty();
            let selected_image =
                select_cloud_image(&boot.cloud, boot.strict_registry, has_hanzo_oci)?;
            let pinned = Boot {
                cloud: image::pin_with_config(
                    &reqwest::Client::new(),
                    &selected_image,
                    image::architecture(),
                    cfg,
                )
                .await?,
                ..boot
            };
            let manifest = workload_for(&pinned)?;
            kube.apply_workload(&manifest).await?;
            write_state(&dir, "cloud-starting");
            kube.wait_cloud_ready(UP_TIMEOUT).await?;
            write_state(&dir, "ready");
            let verb = if replicas.is_none() {
                "deployed"
            } else {
                "updated & started"
            };
            if native_warm {
                println!("{} hanzo {verb} on native Linux k3s", "✓".green());
            } else {
                println!(
                    "{} hanzo {verb} on warm k3s (supervisor pid {pid} — skipped VM boot & kube-system pulls)",
                    "✓".green()
                );
            }
        } else if state == "stopped" || replicas == Some(0) {
            kube.scale_cloud(1).await?;
            write_state(&dir, "cloud-starting");
            kube.wait_cloud_ready(UP_TIMEOUT).await?;
            write_state(&dir, "ready");
            println!(
                "{} hanzo started on warm k3s (skipped VM boot & image pulls)",
                "✓".green()
            );
        } else {
            kube.wait_cloud_ready(UP_TIMEOUT).await?;
            write_state(&dir, "ready");
            if native_warm {
                println!("{} already running (native Linux k3s, ready)", "●".green());
            } else {
                println!(
                    "{} already running (supervisor pid {pid}, ready)",
                    "●".green()
                );
            }
        }
        endpoints()?;
        let res = finish_link(cfg, link).await;
        if ui || (!no_ui && std::io::stdout().is_terminal()) {
            return tui::run_dashboard();
        }
        return res;
    }

    if let Some(pid) = running(&dir, VM) {
        crate::warn(&format!(
            "collecting an orphaned vm (pid {pid}) from an earlier boot"
        ));
        if !stop(pid, Duration::from_secs(10)) {
            bail!("an earlier vm (pid {pid}) is still running — `hanzo down` first");
        }
        clear_pid(&dir, VM);
    }

    let has_hanzo_oci = !image::credentials_for("oci.hanzo.ai", cfg).await.is_empty();
    let selected_image = select_cloud_image(&boot.cloud, boot.strict_registry, has_hanzo_oci)?;

    let boot = Boot {
        cloud: image::pin_with_config(
            &reqwest::Client::new(),
            &selected_image,
            image::architecture(),
            cfg,
        )
        .await?,
        ..boot
    };
    let bin = vm::resolve_or_install().await?;
    let k3s_install = install_cmd(boot.strict_registry)?;
    vm::ensure_checkpoint(&bin, &checkpoint(), &k3s_install)?;

    write_state(&dir, "starting");
    spawn_supervisor(&dir, &boot, cfg.org.as_deref(), false)?;
    wait_ready_state(&dir)?;
    write_state(&dir, "cloud-starting");
    let kube = KubeClient::from_kubeconfig(&kubeconfig_path()?)?;
    kube.wait_cloud_ready(UP_TIMEOUT).await?;
    write_state(&dir, "ready");
    println!(
        "{} k3s and Cloud are ready — Kubernetes API at https://127.0.0.1:{K3S_PORT}",
        "✓".green()
    );
    let hanzod_ready = match host::register_upstream(Some(&format!("127.0.0.1:{LOCAL_PORT}"))).await
    {
        Ok(()) => true,
        Err(e) => {
            eprintln!(
                "{} k3s is up, but registering local cloud with hanzod failed:\n  {e:#}\n  \
                 Install/upgrade the unified daemon with `hanzo upgrade hanzod` (then `hanzo host start`) \
                 to expose http://127.0.0.1:3690; the local cloud is already reachable directly at http://127.0.0.1:{LOCAL_PORT}.",
                "warning:".yellow().bold()
            );
            false
        }
    };
    endpoints_with_hanzod(hanzod_ready)?;
    let res = finish_link(cfg, link).await;
    if ui || (!no_ui && std::io::stdout().is_terminal()) {
        return tui::run_dashboard();
    }
    res
}

pub fn dashboard() -> Result<()> {
    tui::run_dashboard()
}

#[allow(dead_code)]
pub fn console() -> Result<()> {
    tui::run_dashboard()
}

fn endpoints() -> Result<()> {
    endpoints_with_hanzod(true)
}

fn endpoints_with_hanzod(hanzod_ready: bool) -> Result<()> {
    print!("{}", format_endpoints(&kubeconfig_path()?, hanzod_ready));
    Ok(())
}

pub(crate) fn format_endpoints(kubeconfig: &Path, hanzod_ready: bool) -> String {
    let cloud_line = if hanzod_ready {
        format!(
            "  the cloud             http://127.0.0.1:3690  via hanzod → 127.0.0.1:{LOCAL_PORT}  (hanzo network use local)\n"
        )
    } else {
        format!(
            "  the cloud (direct)    http://127.0.0.1:{LOCAL_PORT}  (http://127.0.0.1:3690 requires hanzod: `hanzo upgrade hanzod`)\n"
        )
    };
    format!(
        "  export KUBECONFIG={}\n{cloud_line}  what this cluster is  hanzo up --attest\n",
        kubeconfig.display()
    )
}

fn spawn_supervisor(
    dir: &Path,
    boot: &Boot,
    org: Option<&str>,
    runtime_only: bool,
) -> Result<u32> {
    let exe = std::env::current_exe().context("resolving our own binary")?;
    let path = dir.join("supervisor.log");
    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .truncate(false)
        .open(&path)
        .and_then(|f| f.set_len(0).map(|()| f))
        .with_context(|| format!("creating {}", path.display()))?;
    let mut cmd = Command::new(exe);
    if let Some(org) = org {
        cmd.args(["--as", org]);
    }
    cmd.args([
        "up",
        "--cpus",
        &boot.cpus.to_string(),
        "--memory",
        &boot.memory_mb.to_string(),
        "--disk-size",
        &boot.disk_mb.to_string(),
        "--cloud",
        &boot.cloud,
    ]);
    if let Some(staged) = boot.staged.as_deref() {
        cmd.args(["--staged", staged]);
    }
    if let Some(issuer) = boot.iam_issuer.as_deref() {
        cmd.args(["--iam-issuer", issuer]);
    }
    if let Some(ai_url) = boot.ai_base_url.as_deref() {
        cmd.args(["--ai-base-url", ai_url]);
    }
    if boot.strict_registry {
        cmd.arg("--strict-registry");
    }
    cmd.arg("supervise");
    if runtime_only {
        cmd.arg("--runtime-only");
    }
    cmd.stdin(Stdio::null())
        .stdout(Stdio::from(
            log.try_clone().context("duplicating the log handle")?,
        ))
        .stderr(Stdio::from(log));
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    let child = cmd.spawn().context("starting the k3s supervisor")?;
    Ok(child.id())
}

pub(crate) fn wait_ready_state(dir: &Path) -> Result<()> {
    let log = dir.join("supervisor.log");
    let deadline = Instant::now() + UP_TIMEOUT;
    loop {
        match read_state(dir).as_deref() {
            Some("ready") | Some("runtime-ready") => return Ok(()),
            Some(s) if s.starts_with("error") || s.starts_with("down") => {
                bail!("{s} — see {}", log.display())
            }
            _ => {}
        }
        if Instant::now() >= deadline {
            bail!(
                "k3s did not come up within {}s — see {}",
                UP_TIMEOUT.as_secs(),
                log.display()
            );
        }
        std::thread::sleep(Duration::from_millis(500));
    }
}

async fn finish_link(cfg: &mut Config, cluster: Option<String>) -> Result<()> {
    let Some(cluster) = cluster else {
        return Ok(());
    };
    let caller = link::Caller::sign_in(cfg, None).await?;
    let target = format!("127.0.0.1:{K3S_PORT}");
    let dns = link::publish(&caller, &format!("k8s-{cluster}"), &target).await?;
    println!(
        "{} {} → {target} — `hanzo link host --install` carries it",
        "✓".green(),
        dns.cyan().bold()
    );
    Ok(())
}

pub async fn status() -> Result<()> {
    #[cfg(windows)]
    {
        let addr = format!("127.0.0.1:{LOCAL_PORT}");
        if host::healthy(&addr).await {
            match host::running_pid() {
                Some(pid) => println!(
                    "{} local cloud running at http://{addr} (pid {pid})",
                    "●".green()
                ),
                None => println!("{} local cloud running at http://{addr}", "●".green()),
            }
            return Ok(());
        }
    }

    let dir = up_dir()?;
    if running(&dir, SUPERVISOR).is_none() && host_native_k3s_active() {
        println!(
            "{} native Linux k3s active on https://127.0.0.1:{K3S_PORT}",
            "●".green()
        );
        return Ok(());
    }
    let Some(pid) = running(&dir, SUPERVISOR) else {
        if let Some(vm) = running(&dir, VM) {
            println!(
                "{} a vm (pid {vm}) is running with no supervisor — `hanzo down`",
                "●".yellow()
            );
        } else {
            println!("{} not running", "○".dimmed());
        }
        return Ok(());
    };
    let state = read_state(&dir).unwrap_or_else(|| "unknown".into());
    println!("{} supervisor running (pid {pid}, {state})", "●".green());
    println!(
        "  logs {}",
        dir.join("supervisor.log").display().to_string().dimmed()
    );
    let kc = kubeconfig_path()?;
    if !kc.exists() {
        println!("  no kubeconfig yet ({})", kc.display());
        return Ok(());
    }
    match which::which("kubectl") {
        Ok(kubectl) => {
            let _ = Command::new(kubectl)
                .arg("--kubeconfig")
                .arg(&kc)
                .args(["get", "nodes"])
                .status();
        }
        Err(_) => println!("  kubectl not on PATH — KUBECONFIG={}", kc.display()),
    }
    Ok(())
}

/// `hanzo start` — boot the local Hanzo runtime (microVM or native Linux k3s)
/// without deploying Cloud or user workloads.
pub async fn start(cfg: &mut Config, boot: Boot, prefer_native: bool) -> Result<()> {
    let dir = up_dir()?;
    if let Some(pid) = running(&dir, SUPERVISOR) {
        let state = read_state(&dir).unwrap_or_else(|| "unknown".into());
        println!(
            "{} Hanzo runtime already running (supervisor pid {pid}, {state})",
            "●".green()
        );
        return Ok(());
    }
    let env_native = std::env::var("HANZO_RUNTIME")
        .ok()
        .is_some_and(|v| matches!(v.trim().to_ascii_lowercase().as_str(), "native" | "native-linux"));
    if (prefer_native || env_native || host_native_k3s_active()) && cfg!(target_os = "linux") {
        if host_native_k3s_active() {
            let kc = adopt_native_linux_kubeconfig()?;
            write_state(&dir, "runtime-ready");
            println!(
                "{} Hanzo native Linux runtime ready — k3s at https://127.0.0.1:{K3S_PORT} (run `hanzo up` to start Cloud)",
                "✓".green()
            );
            println!("  export KUBECONFIG={}", kc.display());
            return Ok(());
        } else if prefer_native || env_native {
            // Attempt to start system k3s unit if installed without password prompt
            let _ = Command::new("systemctl")
                .args(["start", "--no-ask-password", "k3s.service"])
                .status();
            if host_native_k3s_active() {
                let kc = adopt_native_linux_kubeconfig()?;
                write_state(&dir, "runtime-ready");
                println!(
                    "{} Hanzo native Linux runtime started — k3s at https://127.0.0.1:{K3S_PORT}",
                    "✓".green()
                );
                println!("  export KUBECONFIG={}", kc.display());
                return Ok(());
            }
            bail!(
                "--native Linux runtime requested, but host `k3s` is not active on 127.0.0.1:{K3S_PORT}.\n\
                 Start host k3s (`sudo systemctl start k3s`) or omit `--native` to boot a `hanzo-vm` microVM."
            );
        }
    }

    if let Some(pid) = running(&dir, VM) {
        crate::warn(&format!(
            "collecting an orphaned vm (pid {pid}) from an earlier boot"
        ));
        if !stop(pid, Duration::from_secs(10)) {
            bail!("an earlier vm (pid {pid}) is still running — `hanzo stop` first");
        }
        clear_pid(&dir, VM);
    }
    let bin = vm::resolve_or_install().await?;
    let k3s_install = install_cmd(boot.strict_registry)?;
    vm::ensure_checkpoint(&bin, &checkpoint(), &k3s_install)?;

    write_state(&dir, "starting");
    spawn_supervisor(&dir, &boot, cfg.org.as_deref(), true)?;
    wait_ready_state(&dir)?;
    println!(
        "{} Hanzo runtime started — k3s Ready at https://127.0.0.1:{K3S_PORT} (run `hanzo up` to start Cloud)",
        "✓".green()
    );
    println!("  export KUBECONFIG={}", kubeconfig_path()?.display());
    Ok(())
}

/// `hanzo stop` — gracefully drain workloads (Cloud + user apps), stop k3s and
/// shut down the microVM; leave `hanzod` running.
pub async fn stop_runtime() -> Result<()> {
    #[cfg(windows)]
    if let Some(_pid) = host::stop_recorded_pid()? {
        let dir = up_dir()?;
        write_state(&dir, "down");
        println!("{} local cloud stopped", "✓".green());
        return Ok(());
    }

    let dir = up_dir()?;
    let (supervisor, vm_pid) = (running(&dir, SUPERVISOR), running(&dir, VM));
    if supervisor.is_none() && vm_pid.is_none() {
        clear_pid(&dir, SUPERVISOR);
        clear_pid(&dir, VM);
        if host_native_k3s_active() {
            if let Ok(kc) = kubeconfig_path() {
                if kc.exists() {
                    if let Ok(kube) = KubeClient::from_kubeconfig(&kc) {
                        let _ = kube.scale_cloud(0).await;
                    }
                }
            }
            let _ = Command::new("systemctl")
                .args(["stop", "--no-ask-password", "k3s.service"])
                .status();
            write_state(&dir, "down");
            println!("{} native Linux runtime workloads drained", "✓".green());
            return Ok(());
        }
        println!("{} not running", "○".dimmed());
        return Ok(());
    }
    if supervisor.is_some() {
        if let Ok(kc) = kubeconfig_path() {
            if kc.exists() {
                if let Ok(kube) = KubeClient::from_kubeconfig(&kc) {
                    let _ = kube.scale_cloud(0).await;
                }
            }
        }
    }
    down_vm(&dir, supervisor)
}

pub async fn ps() -> Result<()> {
    crate::commands::lifecycle::ps(false).await
}

pub async fn reset(all: bool) -> Result<()> {
    let _ = stop_runtime().await;
    if let Ok(dir) = up_dir() {
        if dir.exists() {
            std::fs::remove_dir_all(&dir)
                .with_context(|| format!("removing {}", dir.display()))?;
            println!("{} removed {}", "✓".green(), dir.display());
        }
    }
    if let Ok(kc) = kubeconfig_path() {
        if kc.exists() {
            std::fs::remove_file(&kc).with_context(|| format!("removing {}", kc.display()))?;
            println!("{} removed {}", "✓".green(), kc.display());
        }
    }
    if all {
        if let Ok(bin) = vm::resolve_or_install().await {
            let cp = checkpoint();
            let _ = Command::new(&bin)
                .args(["checkpoint", "delete", &cp])
                .status();
            let legacy = format!("k3s-{}", vm::VM_VERSION);
            let _ = Command::new(&bin)
                .args(["checkpoint", "delete", &legacy])
                .status();
            println!("{} cleared k3s microVM checkpoints", "✓".green());
        }
    }
    println!(
        "{} local environment reset (run `hanzo up` to boot fresh)",
        "✓".green()
    );
    Ok(())
}

pub async fn down(stop_vm: bool) -> Result<()> {
    if stop_vm {
        return stop_runtime().await;
    }
    #[cfg(windows)]
    if let Some(_pid) = host::stop_recorded_pid()? {
        let dir = up_dir()?;
        write_state(&dir, "down");
        println!("{} local cloud down", "✓".green());
        return Ok(());
    }

    let dir = up_dir()?;
    let (supervisor, vm_pid) = (running(&dir, SUPERVISOR), running(&dir, VM));
    let native = supervisor.is_none() && host_native_k3s_active();
    if supervisor.is_none() && vm_pid.is_none() && !native {
        clear_pid(&dir, SUPERVISOR);
        clear_pid(&dir, VM);
        println!("{} not running", "○".dimmed());
        return Ok(());
    }

    if supervisor.is_some() || native {
        let kc = if native {
            adopt_native_linux_kubeconfig().unwrap_or_else(|_| kubeconfig_path().unwrap_or_default())
        } else {
            kubeconfig_path()?
        };
        if kc.exists() {
            match KubeClient::from_kubeconfig(&kc) {
                Ok(kube) => match kube.scale_cloud(0).await {
                    Ok(()) => {
                        write_state(&dir, "stopped");
                        println!(
                            "{} hanzo stopped (k3s runtime kept warm — next `hanzo up` skips image pulls; run `hanzo stop` to shut down the runtime too)",
                            "✓".green()
                        );
                        return Ok(());
                    }
                    Err(e) => {
                        crate::warn(&format!(
                            "could not scale cloud deployment on local k3s ({e:#}); stopping the runtime instead"
                        ));
                    }
                },
                Err(e) => {
                    crate::warn(&format!(
                        "could not load {} ({e:#}); stopping the runtime instead",
                        kc.display()
                    ));
                }
            }
        }
    }

    down_vm(&dir, supervisor)
}

fn down_vm(dir: &Path, supervisor: Option<i32>) -> Result<()> {
    if let Some(pid) = supervisor {
        if !stop(pid, Duration::from_secs(30)) {
            bail!("supervisor (pid {pid}) did not exit");
        }
        clear_pid(dir, SUPERVISOR);
    }
    if let Some(pid) = running(dir, VM) {
        if !stop(pid, Duration::from_secs(10)) {
            bail!("the vm (pid {pid}) did not exit");
        }
    }
    clear_pid(dir, VM);

    write_state(dir, "down");
    println!("{} down (k3s microVM stopped)", "✓".green());
    Ok(())
}

pub(crate) fn service_argv(argv: Vec<String>) -> (String, Vec<String>) {
    let mut it = argv.into_iter();
    let service = it.next().unwrap_or_else(|| "cloud".into());
    let mut rest: Vec<String> = it.collect();
    if rest.first().map(String::as_str) == Some("--") {
        rest.remove(0);
    }
    (service, rest)
}

pub async fn deprecated_service(argv: Vec<String>) -> Result<()> {
    let (service, rest) = service_argv(argv);
    crate::warn(&format!(
        "`hanzo up {service}` is now `hanzo host serve {service}`; \
         this forwarding goes away next release"
    ));
    host::serve(service, rest).await
}
