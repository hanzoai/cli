//! `hanzo auth` — identities and credentials, signed in through Hanzo IAM.
//!
//! The resource-noun front door for the identity model. Every verb delegates to
//! the ONE identity seam (`iam`): sign-in is Hanzo IAM's OIDC PKCE flow (or a
//! provider key), the session is stored in the portable credential vault, and the
//! active identity changes ONLY by explicit `login`/`use`. There is exactly one
//! implementation of each behavior — this module re-parents it, never re-writes.
//!
//! | verb     | does                                             |
//! |----------|--------------------------------------------------|
//! | `login`  | sign in through Hanzo IAM; store the session     |
//! | `logout` | sign out one identity (or `--all`)               |
//! | `show`   | the active identity + its org                    |
//! | `list`   | every identity, the active one marked            |
//! | `use`    | select the active identity                       |
//! | `token`  | print the active short-lived access token        |

use anyhow::{anyhow, Result};

use crate::config::Config;
use crate::iam::{login as iam, onboarding, store};

/// `hanzo auth login` — sign in through Hanzo IAM (OIDC), or store a provider key.
/// Bare on a terminal shows the interactive picker; `--provider`/`--token -` is
/// the non-interactive path. The credential is filed in the portable vault.
pub async fn login(cfg: &mut Config, brand: &str, provider: Option<String>, token: Option<String>) -> Result<()> {
    onboarding::run_login(cfg, brand, provider, token).await
}

/// `hanzo auth logout [IDENTITY] [--all]` — sign out one identity (default: the
/// active one) or all of them, removing the credential.
pub fn logout(cfg: &mut Config, brand: &str, identity: Option<String>, all: bool) -> Result<()> {
    iam::logout(cfg, brand, identity, all)
}

/// `hanzo auth show` — the active identity and org.
pub async fn show(cfg: &mut Config, brand: &str) -> Result<()> {
    iam::whoami(cfg, brand, false).await
}

/// `hanzo auth list` — every identity, the active one marked (the ONE listing).
pub async fn list(cfg: &mut Config, brand: &str) -> Result<()> {
    iam::whoami(cfg, brand, true).await
}

/// `hanzo auth use [IDENTITY]` — select the active identity (bare toggles when
/// exactly two are held). Verifies the credential is actually held.
pub fn use_identity(cfg: &mut Config, brand: &str, identity: Option<String>) -> Result<()> {
    iam::switch(cfg, brand, identity)
}

/// `hanzo auth token` — print the active identity's short-lived access token, so
/// it can seed another tool (`Authorization: Bearer …`). Nothing else is printed,
/// so it pipes byte-exactly; it is never written to disk or the config.
pub async fn token(cfg: &mut Config, brand: &str) -> Result<()> {
    let (_id, tok) = store::active_token(cfg, brand).await?
        .ok_or_else(|| anyhow!("not signed in — run `hanzo auth login`"))?;
    println!("{}", tok.access_token);
    Ok(())
}
