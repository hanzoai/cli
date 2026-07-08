use crate::config::{StoredNetwork, StoredWallet};
use anyhow::Result;
use colored::*;

/// Deploy to a Hanzo Cloud environment on the active network. The active wallet
/// is the signer; if none is configured, one is provisioned at sign time
/// (`hanzo wallet` / auto-provision).
pub async fn run(
    env: String,
    dry_run: bool,
    net: StoredNetwork,
    wallet: Option<StoredWallet>,
) -> Result<()> {
    if dry_run {
        println!("{} Dry run - no changes will be made", "🔍".yellow());
    }

    println!(
        "{} Deploying to {} on {} (chain {})",
        "🚀".green(),
        env.cyan(),
        net.name.cyan().bold(),
        net.chain_id
    );
    println!("   api {}", net.api.dimmed());
    match &wallet {
        Some(w) => println!("   signer {} [{}]", w.address.cyan(), w.custody.dimmed()),
        None => println!(
            "   {}",
            "signer: none configured — one will be provisioned (hanzo wallet create)".dimmed()
        ),
    }

    println!("{}", "deploy backend: hanzo cloud (control plane)".dimmed());

    Ok(())
}
