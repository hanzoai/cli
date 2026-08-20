//! `hanzo node` — machines in the compute fleet.
//!
//! A "node" here is a machine that has JOINED the fleet as a run target — the
//! computers mission-control can place agents on. This command manages them:
//! - `join`  registers THIS machine (its spec/capacity/GPUs) as a run target.
//! - `leave` removes THIS machine's registration (stops it advertising itself).
//! - `list`  shows the fleet (`/v1/machines` — capacity + GPUs).
//! - `show`  shows one machine by id.
//!
//! `join`/`leave` reuse the SAME capture + registry the coding wrapper uses
//! (`code::context::Machine` + `code::target`), so a machine describes itself ONE
//! way. The registry is org-scoped SERVER-SIDE from the JWT `owner`; the CLI sends
//! only the bearer — never an org.
//!
//! (The hanzod L1 fabric — run/join hanzo.network — is `hanzo fabric`, a distinct
//! concern from the compute fleet.)

use anyhow::{anyhow, Result};
use colored::*;
use reqwest::Method;

use crate::commands::code::context::{self, Machine, Snapshot};
use crate::commands::code::target::{Register, TargetClient};
use crate::commands::{cloud, network};
use crate::config::Config;
use crate::iam::{paths, store};

/// `hanzo node join` — register this machine in the compute fleet.
pub async fn join(cfg: &mut Config) -> Result<()> {
    let api = network::active(cfg).api;
    let (_id, tok) = store::active_token(cfg, paths::DEFAULT_BRAND)?
        .ok_or_else(|| anyhow!("not signed in — run `hanzo auth login`"))?;

    // The SAME host derivation the coding wrapper registers under, so `node join`
    // and a linked `hanzo agent run` upsert ONE target row for this machine.
    let cwd = std::env::current_dir().unwrap_or_default();
    let snap = Snapshot::capture(&cwd, "hanzo-node", None);
    let machine = Machine::capture().await;
    let body = Register::from_machine(&snap.host, &machine);

    let client = TargetClient::new(&api, &tok.access_token)?;
    let id = client.register(&body).await?;
    context::TargetRecord {
        id: id.clone(),
        host: snap.host.clone(),
        machine_id: snap.machine_id.clone(),
        api: api.trim_end_matches('/').to_string(),
        updated_at: chrono::Utc::now().timestamp(),
    }
    .save()
    .ok();

    println!("{} joined the fleet as {} ({})", "✓".green(), snap.host.cyan().bold(), id.dimmed());
    let cap = machine.spec.capacity();
    if !cap.is_empty() {
        println!("  {}", cap.dimmed());
    }
    Ok(())
}

/// `hanzo node leave` — remove this machine's fleet registration. There is no
/// persistent local daemon to kill: a machine advertises itself only while a
/// `join`/linked run is registering it, so "stop its service" is forgetting the
/// registration so nothing re-heartbeats it. The server prunes the stale target by
/// its own clock.
pub async fn leave(_cfg: &mut Config) -> Result<()> {
    let cwd = std::env::current_dir().unwrap_or_default();
    let machine_id = Snapshot::capture(&cwd, "hanzo-node", None).machine_id;
    if context::TargetRecord::forget(&machine_id)? {
        println!("{} left the fleet (registration removed)", "✓".green());
    } else {
        println!("{}", "this machine was not registered in the fleet".dimmed());
    }
    Ok(())
}

/// `hanzo node list` — the fleet's machines, capacity and GPUs.
pub async fn list(cfg: &mut Config) -> Result<()> {
    let v = cloud::call(cfg, Method::GET, "/v1/machines", None).await?;
    cloud::print(&v);
    Ok(())
}

/// `hanzo node show NODE` — one machine by id.
pub async fn show(cfg: &mut Config, id: String) -> Result<()> {
    let path = format!("/v1/machines/{}", cloud::enc(&id));
    let v = cloud::call(cfg, Method::GET, &path, None).await?;
    cloud::print(&v);
    Ok(())
}
