use crate::config::{StoredNetwork, StoredWallet};
use anyhow::{bail, Result};
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

    // This command has never deployed anything: it makes no network call at all.
    // Exiting 0 told you it worked, which is the worst thing a deploy can do —
    // you cannot tell a successful deploy from a no-op. Until it drives the real
    // /v1/deploy control plane (through iam::store::active_token, like every
    // other product command), it must refuse rather than pretend.
    if dry_run {
        return Ok(());
    }
    bail!(
        "`hanzo deploy` is not implemented — it makes no call to the control plane, \
         and exiting 0 would tell you a deploy happened when none did.\n\
         Use the platform control plane directly until this drives /v1/deploy."
    )
}
