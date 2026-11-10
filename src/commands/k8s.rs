//! `hanzo k8s [args…]` — manage the Hanzo Kubernetes cluster over the native
//! Hanzo Zero-Trust fabric (`k8s.hanzo` on `:26443` via `hanzozt/zt`) or pass
//! `kubectl` / `helm` commands through directly.
//!
//! - Bare `hanzo k8s` (or `hanzo k8s status`) reports the state of the ZT API
//!   tunnel (`127.0.0.1:26443`), the Hanzo SQL port-forward (`127.0.0.1:5433`),
//!   the active kubeconfig (`~/.kube/hanzo-aws.yaml`), cluster nodes, and
//!   workloads in the `hanzo` namespace.
//! - `hanzo k8s tunnel` (or `connect`) ensures both
//!   `hanzo-link-dial-k8s.hanzo.service` (`:26443`) and
//!   `hanzo-sql-forward.service` (`:5433`) are active and listening (installing
//!   the ZT dial unit via `hanzo link dial k8s.hanzo 26443 --install` if absent).
//! - `hanzo k8s zt [status | dial <svc> <port> | host …]` inspects or dials
//!   services on the Hanzo Zero-Trust fabric (`hanzo link …`).
//! - `hanzo k8s helm [args…]` ensures the `:26443` ZT tunnel is up and execs
//!   `helm` with `KUBECONFIG` set (defaulting to `-n hanzo` when no namespace
//!   flag is given).
//! - `hanzo k8s restart <deploy> [-n <ns>]` restarts a deployment and waits for
//!   rollout completion in `-n hanzo`.
//! - `hanzo k8s logs <deploy-or-pod> [args…]` tails logs (defaulting a bare
//!   service name like `cloud` or `zen` to `-n hanzo deploy/<name>`).
//! - Any other arguments (`hanzo k8s get pods -n hanzo`, `hanzo k8s apply -f …`,
//!   `hanzo k8s describe …`) ensure the `:26443` ZT tunnel is up and exec
//!   `kubectl` against `~/.kube/hanzo-aws.yaml` verbatim.

use crate::commands::launch;
use anyhow::{anyhow, bail, Context, Result};
use colored::*;
use std::net::{SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

const K8S_TUNNEL_PORT: u16 = 26443;
const SQL_TUNNEL_PORT: u16 = 5433;
const K8S_SERVICE: &str = "k8s.hanzo";
const K8S_UNIT: &str = "hanzo-link-dial-k8s.hanzo.service";
const SQL_UNIT: &str = "hanzo-sql-forward.service";
const DEFAULT_NS: &str = "hanzo";

/// Resolve the kubeconfig to use when the caller did not pass `--kubeconfig`
/// or set `KUBECONFIG`. Prefers `~/.kube/hanzo-aws.yaml` (the ZT-tunneled
/// production cluster), then `~/.kube/hanzo.yaml` (local `hanzo up`), then
/// `~/.kube/config`.
pub fn resolve_kubeconfig() -> Option<PathBuf> {
    if let Ok(env_val) = std::env::var("KUBECONFIG") {
        let trimmed = env_val.trim();
        if !trimmed.is_empty() {
            return Some(PathBuf::from(trimmed));
        }
    }
    let home = dirs::home_dir()?;
    for rel in [".kube/hanzo-aws.yaml", ".kube/hanzo.yaml", ".kube/config"] {
        let candidate = home.join(rel);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

fn port_open(port: u16) -> bool {
    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    TcpStream::connect_timeout(&addr, Duration::from_millis(400)).is_ok()
}

fn uses_zt_tunnel(kubeconfig: &Path) -> bool {
    kubeconfig
        .file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| n == "hanzo-aws.yaml")
        || std::fs::read_to_string(kubeconfig)
            .ok()
            .is_some_and(|s| s.contains(":26443"))
}

fn unit_installed(unit_name: &str) -> bool {
    dirs::home_dir()
        .map(|h| h.join(".config/systemd/user").join(unit_name).is_file())
        .unwrap_or(false)
}

/// Ensure the ZT tunnel on `:26443` is listening, starting (or installing via
/// `hanzo link dial k8s.hanzo 26443 --install`) the systemd user service if
/// needed and waiting briefly for the socket to come up.
pub fn ensure_tunnel(kubeconfig: &Path, quiet: bool) -> Result<bool> {
    if !uses_zt_tunnel(kubeconfig) {
        return Ok(true);
    }
    if port_open(K8S_TUNNEL_PORT) {
        return Ok(true);
    }
    if !unit_installed(K8S_UNIT) {
        if !quiet {
            eprintln!(
                "{} installing Zero-Trust tunnel {} ({K8S_SERVICE} → 127.0.0.1:{K8S_TUNNEL_PORT}) …",
                "→".yellow(),
                K8S_UNIT.cyan()
            );
        }
        if let Ok(exe) = std::env::current_exe() {
            let _ = Command::new(exe)
                .args([
                    "link",
                    "dial",
                    K8S_SERVICE,
                    &K8S_TUNNEL_PORT.to_string(),
                    "--install",
                ])
                .status();
        }
    } else {
        if !quiet {
            eprintln!(
                "{} starting {} for 127.0.0.1:{K8S_TUNNEL_PORT} …",
                "→".yellow(),
                K8S_UNIT.cyan()
            );
        }
        let _ = Command::new("systemctl")
            .args(["--user", "restart", K8S_UNIT])
            .status();
    }

    let deadline = Instant::now() + Duration::from_secs(8);
    while Instant::now() < deadline {
        if port_open(K8S_TUNNEL_PORT) {
            return Ok(true);
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    Ok(port_open(K8S_TUNNEL_PORT))
}

/// Ensure both the K8s API tunnel (`:26443`) and SQL forward (`:5433`) are up.
fn cmd_tunnel(kubeconfig: &Path) -> Result<()> {
    let k8s_up = ensure_tunnel(kubeconfig, false)?;
    if !port_open(SQL_TUNNEL_PORT) && k8s_up && unit_installed(SQL_UNIT) {
        eprintln!(
            "{} starting {} for 127.0.0.1:{SQL_TUNNEL_PORT} …",
            "→".yellow(),
            SQL_UNIT.cyan()
        );
        let _ = Command::new("systemctl")
            .args(["--user", "restart", SQL_UNIT])
            .status();
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline && !port_open(SQL_TUNNEL_PORT) {
            std::thread::sleep(Duration::from_millis(250));
        }
    }
    let k8s_state = if port_open(K8S_TUNNEL_PORT) {
        "listening".green().bold()
    } else {
        "not listening".red().bold()
    };
    let sql_state = if port_open(SQL_TUNNEL_PORT) {
        "listening".green().bold()
    } else {
        "not listening".yellow()
    };
    println!(
        "{} k8s.hanzo  127.0.0.1:{K8S_TUNNEL_PORT}  {k8s_state} ({K8S_UNIT})",
        "•".cyan()
    );
    println!(
        "{} hanzo-sql  127.0.0.1:{SQL_TUNNEL_PORT}   {sql_state} ({SQL_UNIT})",
        "•".cyan()
    );
    println!(
        "{} kubeconfig {}",
        "•".cyan(),
        kubeconfig.display().to_string().dimmed()
    );
    Ok(())
}

/// Print cluster tunnel status, nodes, and pod summary in namespace `hanzo`.
fn cmd_status(kubeconfig: &Path) -> Result<()> {
    cmd_tunnel(kubeconfig)?;
    println!();

    let kubectl = which::which("kubectl").context("kubectl is not installed on PATH")?;
    println!("{}", "Nodes".bold());
    let _ = Command::new(&kubectl)
        .arg("--kubeconfig")
        .arg(kubeconfig)
        .args(["get", "nodes", "-o", "wide"])
        .status();

    println!("\n{}", "Workloads (namespace: hanzo)".bold());
    let _ = Command::new(&kubectl)
        .arg("--kubeconfig")
        .arg(kubeconfig)
        .args(["get", "pods", "-n", DEFAULT_NS])
        .status();
    Ok(())
}

fn has_namespace_flag(args: &[String]) -> bool {
    args.iter().any(|a| {
        a == "-n"
            || a == "--namespace"
            || a == "-A"
            || a == "--all-namespaces"
            || a.starts_with("-n=")
            || a.starts_with("--namespace=")
    })
}

/// Build the `kubectl` argument list for `hanzo k8s logs …`, expanding a bare
/// workload name (`hanzo k8s logs cloud -f`) to `-n hanzo deploy/cloud -f`.
fn expand_logs_args(rest: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    if !has_namespace_flag(rest) {
        out.push("-n".to_string());
        out.push(DEFAULT_NS.to_string());
    }
    out.push("logs".to_string());
    let mut normalized_target = false;
    for arg in rest {
        if !normalized_target && !arg.starts_with('-') {
            normalized_target = true;
            if !arg.contains('/') && !arg.starts_with("hanzo-") && !arg.contains('-') {
                out.push(format!("deploy/{arg}"));
                continue;
            }
        }
        out.push(arg.clone());
    }
    out
}

/// Build the `helm` argument list for `hanzo k8s helm …`, defaulting namespace
/// operations to `-n hanzo` when no namespace flag is supplied.
fn expand_helm_args(rest: &[String]) -> Vec<String> {
    let mut out = rest.to_vec();
    let sub = rest.first().map(String::as_str).unwrap_or_default();
    let ns_scoped = matches!(
        sub,
        "list" | "ls" | "status" | "upgrade" | "install" | "uninstall" | "del" | "delete" | "history" | "rollback" | "get" | "test"
    );
    if ns_scoped && !has_namespace_flag(rest) {
        out.push("-n".to_string());
        out.push(DEFAULT_NS.to_string());
    }
    out
}

/// `hanzo k8s restart <deployment> [-n <ns>]`
fn cmd_restart(kubeconfig: &Path, rest: &[String]) -> Result<()> {
    if rest.is_empty() {
        bail!("usage: hanzo k8s restart <deployment> [-n <namespace>]");
    }
    let _ = ensure_tunnel(kubeconfig, false)?;
    let kubectl = which::which("kubectl").context("kubectl is not installed on PATH")?;

    let mut ns = DEFAULT_NS.to_string();
    let mut targets = Vec::new();
    let mut i = 0;
    while i < rest.len() {
        match rest[i].as_str() {
            "-n" | "--namespace" if i + 1 < rest.len() => {
                ns = rest[i + 1].clone();
                i += 2;
            }
            arg if arg.starts_with("--namespace=") => {
                ns = arg.trim_start_matches("--namespace=").to_string();
                i += 1;
            }
            arg if !arg.starts_with('-') => {
                let target = if arg.contains('/') {
                    arg.to_string()
                } else {
                    format!("deployment/{arg}")
                };
                targets.push(target);
                i += 1;
            }
            _ => {
                i += 1;
            }
        }
    }
    if targets.is_empty() {
        bail!("usage: hanzo k8s restart <deployment> [-n <namespace>]");
    }

    for target in &targets {
        let status = Command::new(&kubectl)
            .arg("--kubeconfig")
            .arg(kubeconfig)
            .args(["-n", &ns, "rollout", "restart", target])
            .status()
            .context("running kubectl rollout restart")?;
        if !status.success() {
            bail!("kubectl rollout restart {target} failed with {status}");
        }
    }
    for target in &targets {
        let status = Command::new(&kubectl)
            .arg("--kubeconfig")
            .arg(kubeconfig)
            .args(["-n", &ns, "rollout", "status", target, "--timeout=180s"])
            .status()
            .context("running kubectl rollout status")?;
        if !status.success() {
            bail!("kubectl rollout status {target} failed with {status}");
        }
    }
    Ok(())
}

/// Entry point for `hanzo k8s [args…]`.
pub async fn run(args: Vec<String>) -> Result<()> {
    // `hanzo k8s zt [args…]` delegates directly to `hanzo link [args…]` (defaulting to `status`).
    if matches!(args.first().map(String::as_str), Some("zt" | "net")) {
        let exe = std::env::current_exe().context("resolving hanzo binary")?;
        let mut link_args = vec!["link".to_string()];
        if args.len() == 1 {
            link_args.push("status".to_string());
        } else {
            link_args.extend_from_slice(&args[1..]);
        }
        return launch::exec(&exe, &link_args);
    }

    let kubeconfig = resolve_kubeconfig().ok_or_else(|| {
        anyhow!("no kubeconfig found at ~/.kube/hanzo-aws.yaml, ~/.kube/hanzo.yaml, or ~/.kube/config")
    })?;

    match args.first().map(String::as_str) {
        None | Some("status") if args.len() <= 1 => return cmd_status(&kubeconfig),
        Some("tunnel") | Some("connect") if args.len() == 1 => return cmd_tunnel(&kubeconfig),
        Some("restart") => return cmd_restart(&kubeconfig, &args[1..]),
        Some("helm") => {
            let _ = ensure_tunnel(&kubeconfig, false)?;
            let helm = which::which("helm").context("helm is not installed on PATH")?;
            if std::env::var_os("KUBECONFIG").is_none() {
                std::env::set_var("KUBECONFIG", &kubeconfig);
            }
            let helm_args = expand_helm_args(&args[1..]);
            return launch::exec(&helm, &helm_args);
        }
        _ => {}
    }

    let _ = ensure_tunnel(&kubeconfig, false)?;
    let kubectl = which::which("kubectl").context("kubectl is not installed on PATH")?;

    let has_kubeconfig_flag = args
        .iter()
        .any(|a| a == "--kubeconfig" || a.starts_with("--kubeconfig="));

    let effective_args = if args.first().map(String::as_str) == Some("logs") && args.len() >= 2 {
        expand_logs_args(&args[1..])
    } else {
        args
    };

    let mut full_args = Vec::with_capacity(effective_args.len() + 2);
    if !has_kubeconfig_flag && std::env::var_os("KUBECONFIG").is_none() {
        full_args.push("--kubeconfig".to_string());
        full_args.push(kubeconfig.display().to_string());
    }
    full_args.extend(effective_args);

    launch::exec(&kubectl, &full_args)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn logs_shorthand_expands_bare_service_into_hanzo_deployment() {
        let args = vec!["cloud".to_string(), "--tail=50".to_string()];
        assert_eq!(
            expand_logs_args(&args),
            vec!["-n", "hanzo", "logs", "deploy/cloud", "--tail=50"]
        );
    }

    #[test]
    fn logs_preserves_explicit_namespace_and_pod_or_resource_kind() {
        let args = vec![
            "-n".to_string(),
            "hanzo-testnet".to_string(),
            "pod/cloud-abc".to_string(),
        ];
        assert_eq!(
            expand_logs_args(&args),
            vec!["logs", "-n", "hanzo-testnet", "pod/cloud-abc"]
        );
    }

    #[test]
    fn helm_defaults_to_hanzo_namespace_when_omitted() {
        let args = vec!["list".to_string()];
        assert_eq!(expand_helm_args(&args), vec!["list", "-n", "hanzo"]);

        let explicit = vec!["list".to_string(), "-A".to_string()];
        assert_eq!(expand_helm_args(&explicit), vec!["list", "-A"]);
    }
}
