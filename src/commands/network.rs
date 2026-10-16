//! `hanzo network` — the ONE network model, shared with the console + cloud.
//!
//! Built-in networks mirror the console selector (mainnet/testnet/devnet/local)
//! and the fabric truth in `~/work/hanzo/node` (hanzo-mining `NetworkType`):
//! Hanzo is a sovereign L1, so `network_id == chain_id`. Chain IDs and RPC hosts
//! come from genesis (lux/genesis/configs/hanzo-*): mainnet 36963 @ rpc.hanzo.network,
//! testnet 36962 @ rpc.testnet.hanzo.network, devnet 36964 @ rpc.devnet.hanzo.network;
//! local is the 1337 localnet convention @ localhost:9630. Custom/sovereign networks are
//! added with `network add` and persisted in config.
//!
//! Each network has its own cloud, so its own API: mainnet api.hanzo.ai (live money),
//! testnet api-test.hanzo.ai (mainnet's release, Square sandbox), devnet
//! api-dev.hanzo.ai (the `dev` branch, Square sandbox). Selecting testnet or devnet
//! therefore never reaches production's books.
//!
//! And its own issuer, the one its API trusts: hanzo.id for mainnet and testnet,
//! which runs mainnet's release, and id-dev.hanzo.ai for devnet, which runs code
//! no review has passed and so never sees a production credential. The issuer
//! picks the realm every identity is filed and found under (`iam::paths::realm`),
//! so a credential one network's issuer signed is never sent to another's API.

use crate::config::{Config, StoredNetwork};
use anyhow::{bail, Result};
use colored::*;

/// Default active network when none is selected.
pub const DEFAULT: &str = "mainnet";

/// The built-in networks. Sovereign L1 ⇒ `network_id == chain_id`.
pub fn builtins() -> Vec<StoredNetwork> {
    vec![
        StoredNetwork {
            name: "mainnet".into(),
            label: "Hanzo Mainnet".into(),
            network_id: 36963,
            chain_id: 36963,
            rpc: "https://rpc.hanzo.network".into(),
            api: "https://api.hanzo.ai".into(),
            iam: Some("https://hanzo.id".into()),
            explorer: Some("https://explorer.hanzo.network".into()),
        },
        StoredNetwork {
            name: "testnet".into(),
            label: "Hanzo Testnet".into(),
            network_id: 36962,
            chain_id: 36962,
            rpc: "https://rpc.testnet.hanzo.network".into(),
            api: "https://api-test.hanzo.ai".into(),
            iam: Some("https://hanzo.id".into()),
            explorer: Some("https://explorer.testnet.hanzo.network".into()),
        },
        StoredNetwork {
            name: "devnet".into(),
            label: "Hanzo Devnet".into(),
            network_id: 36964,
            chain_id: 36964,
            rpc: "https://rpc.devnet.hanzo.network".into(),
            api: "https://api-dev.hanzo.ai".into(),
            iam: Some("https://id-dev.hanzo.ai".into()),
            explorer: Some("https://explorer.devnet.hanzo.network".into()),
        },
        StoredNetwork {
            name: "local".into(),
            label: "Hanzo Local".into(),
            network_id: 1337,
            chain_id: 1337,
            rpc: "http://localhost:9630/v1/chain/c/rpc".into(),
            api: "http://localhost:3690".into(),
            iam: Some("https://hanzo.id".into()),
            explorer: None,
        },
    ]
}

/// True when `name` is a built-in network.
pub fn is_builtin(name: &str) -> bool {
    builtins().iter().any(|n| n.name == name)
}

/// Resolve a network by name — a custom override wins over a built-in.
pub fn resolve(cfg: &Config, name: &str) -> Option<StoredNetwork> {
    cfg.network
        .custom
        .iter()
        .find(|n| n.name == name)
        .cloned()
        .or_else(|| builtins().into_iter().find(|n| n.name == name))
}

/// The active network: the selected one, or the default (mainnet).
pub fn active(cfg: &Config) -> StoredNetwork {
    let name = cfg.network.active.as_deref().unwrap_or(DEFAULT);
    resolve(cfg, name).unwrap_or_else(|| {
        resolve(cfg, DEFAULT).expect("DEFAULT network must exist among built-ins")
    })
}

/// All networks (built-ins with custom overrides applied, then extra customs).
fn all(cfg: &Config) -> Vec<StoredNetwork> {
    let mut out: Vec<StoredNetwork> = builtins()
        .into_iter()
        .map(|b| resolve(cfg, &b.name).unwrap_or(b))
        .collect();
    for c in &cfg.network.custom {
        if !out.iter().any(|n| n.name == c.name) {
            out.push(c.clone());
        }
    }
    out
}

fn print_row(n: &StoredNetwork, active_name: &str) {
    let marker = if n.name == active_name { "*".green().bold() } else { " ".normal() };
    let sovereign = if n.network_id == n.chain_id { "sovereign".dimmed() } else { "".normal() };
    println!(
        "{} {:<10} {:<16} net={:<7} chain={:<7} {}",
        marker,
        n.name.cyan().bold(),
        n.label,
        n.network_id.to_string(),
        n.chain_id.to_string(),
        sovereign,
    );
    println!("             rpc {}", n.rpc.dimmed());
    println!("             api {}", n.api.dimmed());
    println!("             iam {}", issuer(n).dimmed());
}

/// The issuer a network signs in at: its own, or the brand's when it names none.
fn issuer(n: &StoredNetwork) -> String {
    n.iam.clone().unwrap_or_else(|| {
        crate::iam::paths::server_url_for_brand(crate::iam::paths::DEFAULT_BRAND).unwrap_or_default()
    })
}

/// `hanzo network list`
pub fn list(cfg: &Config) -> Result<()> {
    let active_name = active(cfg).name;
    println!("{}", "Networks (* = active)".bold());
    for n in all(cfg) {
        let is_custom = cfg.network.custom.iter().any(|c| c.name == n.name);
        print_row(&n, &active_name);
        if is_custom {
            println!("             {}", "custom".yellow());
        }
    }
    Ok(())
}

/// `hanzo network current`
pub fn current(cfg: &Config) -> Result<()> {
    let n = active(cfg);
    println!("{} {}", n.name.cyan().bold(), n.label);
    println!("  network_id {}", n.network_id);
    println!("  chain_id   {}", n.chain_id);
    println!("  rpc        {}", n.rpc);
    println!("  api        {}", n.api);
    println!("  iam        {}", issuer(&n));
    if let Some(e) = &n.explorer {
        println!("  explorer   {}", e);
    }
    if n.network_id == n.chain_id {
        println!("  {}", "sovereign L1 (network_id == chain_id)".dimmed());
    }
    Ok(())
}

/// `hanzo network use <name>`
pub fn use_network(cfg: &mut Config, name: String) -> Result<()> {
    if resolve(cfg, &name).is_none() {
        bail!(
            "unknown network {:?}. Run `hanzo network list`, or add it with `hanzo network add`.",
            name
        );
    }
    cfg.update(|c| {
        c.network.active = Some(name.clone());
        Ok(())
    })?;
    let n = active(cfg);
    println!("{} active network {} ({})", "✓".green(), name.cyan().bold(), n.label);
    Ok(())
}

/// `hanzo network add <name> --network-id … [--chain-id …] --rpc … --api … [--iam …] [--explorer …]`
#[allow(clippy::too_many_arguments)]
pub fn add(
    cfg: &mut Config,
    name: String,
    network_id: u64,
    chain_id: Option<u64>,
    rpc: String,
    api: String,
    iam: Option<String>,
    explorer: Option<String>,
    label: Option<String>,
    set_active: bool,
) -> Result<()> {
    if is_builtin(&name) {
        bail!("{:?} is a built-in network name — pick another for a custom network.", name);
    }
    // Refused here, where it was typed, rather than at the first sign-in.
    crate::iam::paths::realm(crate::iam::paths::DEFAULT_BRAND, iam.as_deref())?;
    // Sovereign L1: chain_id defaults to network_id (one ID per env per L1).
    let chain_id = chain_id.unwrap_or(network_id);
    let net = StoredNetwork {
        name: name.clone(),
        label: label.unwrap_or_else(|| name.clone()),
        network_id,
        chain_id,
        rpc,
        api,
        iam,
        explorer,
    };
    // Upsert by name, against fresh on-disk state (`update` re-reads, so an edit
    // out here would just be discarded).
    cfg.update(|c| {
        c.network.custom.retain(|x| x.name != net.name);
        c.network.custom.push(net.clone());
        if set_active {
            c.network.active = Some(name.clone());
        }
        Ok(())
    })?;
    if network_id == chain_id {
        println!("{} added sovereign network {} (network_id == chain_id == {})", "✓".green(), name.cyan().bold(), chain_id);
    } else {
        println!("{} added network {} (network_id {}, chain_id {})", "✓".green(), name.cyan().bold(), network_id, chain_id);
    }
    if set_active {
        println!("  {} now active", name.cyan());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::iam::paths::{realm, server_url_for_brand, DEFAULT_BRAND};

    fn builtin(name: &str) -> StoredNetwork {
        builtins().into_iter().find(|n| n.name == name).unwrap()
    }

    /// Each network's API trusts one issuer, and the realm a credential is filed
    /// under follows it: mainnet and testnet share hanzo.id's, devnet has its own.
    #[test]
    fn each_network_signs_in_at_the_issuer_its_api_trusts() {
        for (name, api, iam, realm_name) in [
            ("mainnet", "https://api.hanzo.ai", "https://hanzo.id", "hanzo"),
            ("testnet", "https://api-test.hanzo.ai", "https://hanzo.id", "hanzo"),
            ("devnet", "https://api-dev.hanzo.ai", "https://id-dev.hanzo.ai", "hanzo@id-dev.hanzo.ai"),
            ("local", "http://localhost:3690", "https://hanzo.id", "hanzo"),
        ] {
            let n = builtin(name);
            assert_eq!(n.api, api, "{name}");
            assert_eq!(n.iam.as_deref(), Some(iam), "{name}");
            let r = realm(DEFAULT_BRAND, n.iam.as_deref()).unwrap();
            assert_eq!(r, realm_name, "{name}");
            assert_eq!(server_url_for_brand(&r).as_deref(), Some(iam), "{name}");
        }
    }
}
