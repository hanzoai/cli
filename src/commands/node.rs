//! `hanzo node` — run/join hanzo.network (the fabric) with hanzod.
//!
//! hanzod (`~/work/hanzo/node`) is Hanzo's L1 node on the Lux Network (same
//! Quasar consensus, same ZAP transport as luxd). This command drives it:
//! `up` starts it on the active network, `join` switches network + starts,
//! `status` reports liveness, `stop` sends SIGTERM to the process WE started
//! (by recorded PID — never a blind pkill).
//!
//! Per CI/CD policy we never BUILD the node here; we resolve an existing binary
//! (HANZO_NODE_BIN, then `hanzod` on PATH) and, if absent, print how to get it.

use crate::commands::network;
use crate::config::Config;
use anyhow::{anyhow, bail, Context, Result};
use colored::*;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::Duration;

/// Where we record the PID of a hanzod we started (for `status`/`stop`).
fn pid_file() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("hanzo")
        .join("node.pid")
}

fn write_pid(pid: u32) -> Result<()> {
    let f = pid_file();
    if let Some(dir) = f.parent() {
        std::fs::create_dir_all(dir).ok();
    }
    std::fs::write(&f, pid.to_string()).with_context(|| format!("writing {}", f.display()))
}

fn read_pid() -> Option<u32> {
    std::fs::read_to_string(pid_file()).ok()?.trim().parse().ok()
}

fn clear_pid() {
    std::fs::remove_file(pid_file()).ok();
}

fn pid_alive(pid: u32) -> bool {
    Command::new("kill")
        .args(["-0", &pid.to_string()])
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Resolve a runnable hanzod: explicit override, then PATH. We do NOT build.
fn resolve_node_bin() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("HANZO_NODE_BIN") {
        let pb = PathBuf::from(p);
        if pb.exists() {
            return Some(pb);
        }
    }
    which::which("hanzod").ok()
}

fn missing_bin_err() -> anyhow::Error {
    anyhow!(
        "hanzod not found. Set HANZO_NODE_BIN=/path/to/hanzod, put `hanzod` on PATH, \
         or build it in ~/work/hanzo/node (we do not build node binaries here — CI/CD does)."
    )
}

/// Best-effort start of the cloud control plane alongside the node.
fn spawn_cloud() -> Result<()> {
    let bin = std::env::var("HANZO_CLOUD_BIN")
        .ok()
        .map(PathBuf::from)
        .filter(|p| p.exists())
        .or_else(|| which::which("hanzo-cloud").ok())
        .or_else(|| which::which("cloud").ok());
    match bin {
        Some(b) => {
            let child = Command::new(&b)
                .arg("cloud")
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .context("spawning cloud control plane")?;
            println!("{} cloud control plane started (pid {})", "✓".green(), child.id());
        }
        None => println!(
            "{}",
            "cloud binary not found — start it separately (set HANZO_CLOUD_BIN)".dimmed()
        ),
    }
    Ok(())
}

/// `hanzo node up [--foreground] [--with-cloud]`
pub async fn up(cfg: &Config, foreground: bool, with_cloud: bool) -> Result<()> {
    let net = network::active(cfg);
    println!(
        "{} starting hanzod on {} (network_id {}, chain {})",
        "→".cyan(),
        net.name.cyan().bold(),
        net.network_id,
        net.chain_id
    );
    let bin = resolve_node_bin().ok_or_else(missing_bin_err)?;

    let mut cmd = Command::new(&bin);
    cmd.env("HANZO_NETWORK", &net.name)
        .env("HANZO_NETWORK_ID", net.network_id.to_string())
        .env("HANZO_CHAIN_ID", net.chain_id.to_string())
        .env("HANZO_RPC", &net.rpc);

    if foreground {
        println!("{}", "running in foreground (Ctrl-C to stop)…".dimmed());
        let status = cmd.status().context("running hanzod")?;
        if !status.success() {
            bail!("hanzod exited with {status}");
        }
        return Ok(());
    }

    cmd.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    let child = cmd.spawn().context("spawning hanzod")?;
    let pid = child.id();
    write_pid(pid)?;
    println!("{} hanzod started (pid {pid}) on {}", "✓".green(), net.name.cyan().bold());
    println!("  {} hanzo node status   {} hanzo node stop", "→".dimmed(), "→".dimmed());

    if with_cloud {
        spawn_cloud()?;
    } else {
        println!("  {}", "add --with-cloud to also start the cloud control plane".dimmed());
    }
    Ok(())
}

/// `hanzo node join <network> [--foreground] [--with-cloud]`
pub async fn join(cfg: &mut Config, network_name: String, foreground: bool, with_cloud: bool) -> Result<()> {
    network::use_network(cfg, network_name)?;
    up(cfg, foreground, with_cloud).await
}

/// `hanzo node status`
pub async fn status(cfg: &Config) -> Result<()> {
    let net = network::active(cfg);
    println!("{} {} (network_id {}, chain {})", "network".bold(), net.name.cyan(), net.network_id, net.chain_id);
    match read_pid() {
        Some(pid) if pid_alive(pid) => println!("{} hanzod running (pid {pid})", "●".green()),
        Some(pid) => println!("{} stale pidfile (pid {pid} not running)", "○".yellow()),
        None => println!("{} no hanzod started by this CLI", "○".dimmed()),
    }
    let url = format!("{}/health", net.api.trim_end_matches('/'));
    let client = reqwest::Client::builder().timeout(Duration::from_secs(2)).build()?;
    match client.get(&url).send().await {
        Ok(r) => println!("{} {} -> {}", "api".bold(), url.dimmed(), r.status()),
        Err(_) => println!("{} {} {}", "api".bold(), url.dimmed(), "unreachable".yellow()),
    }
    Ok(())
}

/// `hanzo node stop`
pub fn stop(_cfg: &Config) -> Result<()> {
    match read_pid() {
        None => {
            println!("{}", "no hanzod pidfile — nothing to stop".dimmed());
        }
        Some(pid) => {
            let ok = Command::new("kill")
                .arg(pid.to_string())
                .status()
                .map(|s| s.success())
                .unwrap_or(false);
            clear_pid();
            if ok {
                println!("{} sent SIGTERM to hanzod (pid {pid})", "✓".green());
            } else {
                println!("{} no process {pid} (cleared stale pidfile)", "○".yellow());
            }
        }
    }
    Ok(())
}
