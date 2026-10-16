//! `hanzo dev` — a coding session, run by Hanzo Gen (`hanzo_gen`) with this CLI
//! as its host: the account, the stored keys and the cloud meter come from the
//! CLI's own store, in process.

use std::sync::Arc;

use anyhow::Result;
use hanzo_gen::host::Pending;
use hanzo_gen::{Account, Args, Host, Profile, Provider, Root};
use serde_json::Value;
use tokio::sync::Mutex;

use crate::commands::{network, savings};
use crate::config::Config;
use crate::iam::{onboarding, paths, provider, store};

/// The CLI as Gen's host: this invocation's config, ONE copy for the life of the
/// host, behind one lock held for the whole of each ask. `store::active_token`
/// refreshes in place and re-files an identity IAM moved; holding the lock from
/// the read to the write is what keeps a move made by one ask (a heartbeat) from
/// being overwritten by another that read the config before it (the session).
pub struct Store {
    cfg: Mutex<Config>,
    /// Whether the stored Hanzo key may be offered: only on a network whose API
    /// trusts the issuer it was minted under (`hanzo_keys`).
    hanzo_keys: bool,
}

impl Store {
    pub fn host(cfg: Config) -> Arc<dyn Host> {
        let hanzo_keys = hanzo_keys(&cfg);
        Arc::new(Store { cfg: Mutex::new(cfg), hanzo_keys })
    }
}

/// Whether the active network's API trusts hanzo.id, the issuer whose realm the
/// stored Hanzo key (`hk-`, `sk-`) belongs to. A network with an issuer of its
/// own (devnet) is never sent it: the key is a production credential, and
/// devnet runs code no review has passed. There the session routes by the
/// network's own identity or not through the gateway at all.
fn hanzo_keys(cfg: &Config) -> bool {
    store::realm(cfg, paths::DEFAULT_BRAND).is_ok_and(|r| r == paths::DEFAULT_BRAND)
}

/// What a session reads off `cfg`.
fn profile_of(cfg: &Config) -> Profile {
    let net = network::active(cfg);
    Profile {
        local: net.name == "local",
        api: net.api,
        org: cfg.org.clone(),
        provider: cfg.auth.provider.clone(),
        identities: cfg.auth.identities.iter().map(|i| (i.owner.clone(), i.name.clone())).collect(),
        link: cfg.code.link,
        theme: cfg.code.theme.clone(),
    }
}

impl Host for Store {
    fn profile(&self) -> Pending<'_, Profile> {
        Box::pin(async move { profile_of(&*self.cfg.lock().await) })
    }

    fn account<'a>(&'a self, brand: &'a str) -> Pending<'a, Result<Option<Account>>> {
        Box::pin(async move {
            let mut cfg = self.cfg.lock().await;
            Ok(store::active_token(&mut cfg, brand).await?.map(|(id, t)| Account {
                owner: id.owner,
                name: id.name,
                token: t.access_token,
            }))
        })
    }

    fn refresh(&self, brand: &str) -> Vec<String> {
        // This build, not whichever `hanzo` PATH finds first.
        let exe = std::env::current_exe().map(|p| p.to_string_lossy().into_owned()).unwrap_or_else(|_| "hanzo".into());
        std::iter::once(exe).chain(hanzo_gen::host::token(brand)).collect()
    }

    fn key(&self, p: Provider) -> Result<Option<String>> {
        if p == Provider::Hanzo && !self.hanzo_keys {
            return Ok(None);
        }
        provider::key(match p {
            Provider::Hanzo => provider::Provider::Hanzo,
            Provider::Anthropic => provider::Provider::Anthropic,
            Provider::OpenAI => provider::Provider::OpenAI,
        })
    }

    fn meter<'a>(&'a self, brand: &'a str, query: &'a [String]) -> Pending<'a, std::result::Result<Value, String>> {
        Box::pin(async move {
            let mut cfg = self.cfg.lock().await;
            savings::summary(&mut cfg, brand, query).await
        })
    }
}

/// `hanzo config profile`: what a session reads off this CLI's config, as JSON —
/// the one reader `gen` asks rather than parsing the file a second time. No
/// secret is in it.
pub fn profile(cfg: &Config) -> Result<()> {
    println!("{}", serde_json::to_string(&profile_of(cfg))?);
    Ok(())
}

/// What a session is pointed at. The ONLY thing that ever differed between
/// `hanzo dev` and the old `agent run --mode desktop`, now carried as a value
/// instead of a second command with its own flag.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Target {
    Repo,
    Desktop,
}

/// THE coding-session launcher. Every spelling the CLI offers — a bare
/// `hanzo [flags] [task]`, `hanzo dev …` and `hanzo dev --<tool> …` — arrives
/// here, and they differ in NOTHING but how the backend was named (resolved
/// once, in `hanzo_gen::backend::select`) and whether the bare invocation
/// forced linking on.
///
/// Do not add a second launcher for a new spelling: add the spelling to
/// `select` and let it land here like the rest.
pub async fn session(config: &mut Config, args: Args, target: Target) -> Result<()> {
    onboarding::first_run(config, paths::DEFAULT_BRAND).await;
    let mut opts = args.into_options(Root::need()?)?;
    if target == Target::Desktop {
        // The browser/computer tools ARE how an agent drives a desktop, so this
        // target cannot run without them and never inherits a persisted opt-out.
        opts.mcp = true;
    }
    hanzo_gen::run(Store::host(config.clone()), opts).await
}

#[cfg(test)]
mod network_tests {
    use super::*;

    /// The stored Hanzo key is hanzo.id's: offered on mainnet and testnet, whose
    /// APIs trust that issuer, and never to devnet's.
    #[test]
    fn the_hanzo_key_stays_with_the_issuer_that_minted_it() {
        for (network, offered) in [("mainnet", true), ("testnet", true), ("devnet", false)] {
            let mut cfg = Config::default();
            cfg.network.active = Some(network.into());
            assert_eq!(hanzo_keys(&cfg), offered, "{network}");
        }
    }
}
