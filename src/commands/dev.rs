//! `hanzo dev` — a coding session, run by Hanzo Gen (`hanzo_gen`) with this CLI
//! as its host: the account, the stored keys and the cloud meter come from the
//! CLI's own store, in process.

use std::sync::Arc;

use anyhow::Result;
use hanzo_gen::host::Pending;
use hanzo_gen::{Account, Args, Host, Profile, Provider};
use serde_json::Value;

use crate::commands::{network, savings};
use crate::config::Config;
use crate::iam::{onboarding, paths, provider, store};

/// The CLI as Gen's host: this invocation's config, read on every ask.
pub struct Store {
    cfg: Config,
}

impl Store {
    pub fn host(cfg: Config) -> Arc<dyn Host> {
        Arc::new(Store { cfg })
    }
}

impl Host for Store {
    fn profile(&self) -> Profile {
        let net = network::active(&self.cfg);
        Profile {
            local: net.name == "local",
            api: net.api,
            org: self.cfg.org.clone(),
            provider: self.cfg.auth.provider.clone(),
            identities: self.cfg.auth.identities.iter().map(|i| (i.owner.clone(), i.name.clone())).collect(),
            link: self.cfg.code.link,
            theme: self.cfg.code.theme.clone(),
            // This build, not whichever `hanzo` PATH finds first.
            refresh: vec![
                std::env::current_exe()
                    .map(|p| p.to_string_lossy().into_owned())
                    .unwrap_or_else(|_| "hanzo".into()),
                "auth".into(),
                "token".into(),
            ],
        }
    }

    fn account<'a>(&'a self, brand: &'a str) -> Pending<'a, Result<Option<Account>>> {
        Box::pin(async move {
            // `active_token` refreshes, and re-files an identity IAM moved. The
            // credential store is the shared state, not this struct, so each ask
            // takes its own copy of the config.
            let mut cfg = self.cfg.clone();
            Ok(store::active_token(&mut cfg, brand).await?.map(|(id, t)| Account {
                owner: id.owner,
                name: id.name,
                token: t.access_token,
            }))
        })
    }

    fn key(&self, p: Provider) -> Result<Option<String>> {
        provider::key(match p {
            Provider::Hanzo => provider::Provider::Hanzo,
            Provider::Anthropic => provider::Provider::Anthropic,
            Provider::OpenAI => provider::Provider::OpenAI,
        })
    }

    fn meter<'a>(&'a self, query: &'a [String]) -> Pending<'a, std::result::Result<Value, String>> {
        Box::pin(async move {
            let mut cfg = self.cfg.clone();
            savings::summary(&mut cfg, query).await
        })
    }
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
    let mut opts = args.into_options()?;
    if target == Target::Desktop {
        // The browser/computer tools ARE how an agent drives a desktop, so this
        // target cannot run without them and never inherits a persisted opt-out.
        opts.mcp = true;
    }
    hanzo_gen::run(Store::host(config.clone()), opts).await
}
