//! `hanzo auth login` / `whoami` / `switch` / `logout` — the IAM auth commands.
//!
//! Thin orchestration over [`oauth`] (protocol) and [`store`] (which identity,
//! and its credential): run the flow, add/read/select/remove the identity,
//! present the result. All four verbs are flat and top-level — the CLI has one
//! auth surface, not an `auth` sub-group.
//!
//! Multi-identity: a login ADDS an identity, `switch` selects among them, and
//! every identity survives. `owner` — the IAM org —
//! is what the gateway bills and what the SuperAdmin gate keys on, so switching
//! identity switches the billing org with no separate selector to desync.

use anyhow::{bail, Result};
use colored::*;

use crate::config::Config;

use super::identity::Selector;
use super::paths::{brand_flag, DEFAULT_BRAND};
use super::provider::{self, Provider};
use super::token::TokenSet;
use super::{oauth, store};

/// `hanzo auth login [--brand]`: interactive OIDC PKCE sign-in. ADDS an identity
/// (never clobbers another) and makes it active.
pub async fn login(cfg: &mut Config, brand: &str) -> Result<()> {
    oauth::server_url(brand)?; // reject unknown brands before opening a browser
    let tokens = oauth::login(brand).await?;
    add(cfg, brand, &tokens).await
}

/// `hanzo auth login --token`: store a hanzo.id bearer directly, for a shell
/// that cannot open a browser (CI, a container, a remote host). Same identity law
/// as the browser flow — the principal comes from the token's own claims, never
/// from the caller.
pub async fn login_with_token(cfg: &mut Config, brand: &str, access_token: &str) -> Result<()> {
    add(
        cfg,
        brand,
        &TokenSet {
            access_token: access_token.to_string(),
            token_type: "Bearer".to_string(),
            refresh_token: None,
            id_token: None,
            expires_in: None,
            scope: None,
        },
    )
    .await
}

/// File a token set as its own identity and report the result.
async fn add(cfg: &mut Config, brand: &str, tokens: &TokenSet) -> Result<()> {
    let id = store::add(cfg, brand, tokens)?;

    // Best-effort: the server's view of this token confirms the credential
    // actually works. The IDENTITY on display is the token's own claim —
    // userinfo carries no `owner`, and `owner` is the whole point.
    let label = match oauth::userinfo(brand, &tokens.access_token).await {
        Ok(who) => who.email.or(who.preferred_username).unwrap_or(who.sub),
        Err(_) => id.name.clone(),
    };

    println!(
        "{} Signed in to {} as {} ({})",
        "✓".green(),
        brand.cyan(),
        id.to_string().bold(),
        label.dimmed()
    );
    let held = store::list(cfg, brand).len();
    if held > 1 {
        println!(
            "{}",
            format!("  {held} identities on {brand} — `hanzo auth list` to list, `hanzo auth use` to change")
                .dimmed()
        );
    }
    Ok(())
}

/// `hanzo auth show [--brand] [--all]`: the ACTIVE identity, or every identity.
///
/// Listing lives here rather than behind a separate `identities` verb: one
/// question ("who am I?"), one command, one way.
pub async fn whoami(cfg: &mut Config, brand: &str, all: bool) -> Result<()> {
    oauth::server_url(brand)?; // reject unknown brands before touching the keychain

    if all {
        if store::list(cfg, brand).is_empty() {
            bail!("not signed in to {brand} — run `hanzo auth login{}`", brand_flag(brand));
        }
        println!("{}", store::render(cfg, brand));
        println!("{}", "  (* = active; owner is the billing org)".dimmed());
        return Ok(());
    }

    let Some((id, tokens)) = store::active_token(cfg, brand).await? else {
        bail!("not signed in to {brand} — run `hanzo auth login{}`", brand_flag(brand));
    };

    // `owner/name` first and bold: it is the billing org and the SuperAdmin
    // predicate, and it is exactly what a second identity makes ambiguous.
    println!("{} {}", "identity:".dimmed(), id.to_string().bold());
    println!("{} {}", "org:".dimmed(), format!("{} (billed here)", id.owner).cyan());

    let who = oauth::userinfo(brand, &tokens.access_token).await?;
    println!("{} {}", "sub:".dimmed(), who.sub);
    if let Some(email) = who.email {
        println!("{} {}", "email:".dimmed(), email);
    }
    if let Some(name) = who.name {
        println!("{} {}", "name:".dimmed(), name);
    }
    Ok(())
}

/// `hanzo auth use [IDENTITY] [--brand]`: select the active identity.
pub fn switch(cfg: &mut Config, brand: &str, identity: Option<String>) -> Result<()> {
    let sel = identity.map(|s| s.parse::<Selector>()).transpose()?;
    let id = store::switch(cfg, brand, sel)?;
    println!(
        "{} Active identity on {}: {} — billing to {}",
        "✓".green(),
        brand.cyan(),
        id.to_string().bold(),
        id.owner.cyan()
    );
    Ok(())
}

/// `hanzo auth logout [IDENTITY] [--brand] [--all]`: remove one identity, or every
/// identity for the brand. Signing out of the active identity signs you OUT — it
/// never silently promotes whatever identity remains.
///
/// `--all` is a COMPLETE sign-out: it also drops any stored provider
/// model-credentials (OpenAI/Anthropic/`hk-`) and resets the provider selection
/// to the gateway default. Those keys are not brand-scoped, so only the default
/// brand clears them — `hanzo code` uses the default brand.
///
/// Signing out ENDS THE SESSION, which means telling IAM: the refresh token this
/// client holds lives 30 days, so a logout that only forgot the local copy left a
/// month of spendable access on a machine you had signed out of. Every credential
/// dropped here is revoked at the server (RFC 7009), which kills its whole
/// rotation family.
pub async fn logout(cfg: &mut Config, brand: &str, identity: Option<String>, all: bool) -> Result<()> {
    if all {
        if identity.is_some() {
            bail!("`hanzo auth logout --all` removes every identity; do not also name one");
        }
        let removed = store::remove_all(cfg, brand)?;
        revoke_all(brand, &removed).await;
        let cleared = clear_providers(cfg, brand)?;
        if removed.is_empty() && cleared.is_empty() {
            println!("Not signed in to {brand}; nothing to do.");
            return Ok(());
        }
        if !removed.is_empty() {
            println!(
                "{} Signed out of {} ({})",
                "✓".green(),
                brand.cyan(),
                removed.iter().map(|r| r.id.to_string()).collect::<Vec<_>>().join(", ")
            );
        }
        if !cleared.is_empty() {
            println!(
                "{} Cleared provider keys ({})",
                "✓".green(),
                cleared.join(", ")
            );
        }
        return Ok(());
    }

    let sel = identity.map(|s| s.parse::<Selector>()).transpose()?;
    let removed = store::remove(cfg, brand, sel)?;
    revoke_all(brand, std::slice::from_ref(&removed)).await;
    println!("{} Signed out of {} as {}", "✓".green(), brand.cyan(), removed.id.to_string().bold());

    // Say what is left and how to select it — never select it for them.
    match store::active(cfg, brand) {
        Some(active) => println!("{} {}", "active:".dimmed(), active.to_string().bold()),
        None if !store::list(cfg, brand).is_empty() => {
            println!(
                "{}",
                format!("  no active identity on {brand} — `hanzo auth use <owner/name>`:").dimmed()
            );
            println!("{}", store::render(cfg, brand));
        }
        None => {}
    }
    Ok(())
}

/// Tell IAM to stop honouring the credentials `logout` just dropped.
///
/// BEST EFFORT, and loud when it fails: a signed-out laptop with no network still
/// signs out locally, but the refresh token it forgot is spendable at IAM until
/// this call lands, and the only person who can act on that is the one reading
/// the output. Silence here is how "I logged out" comes to mean nothing.
///
/// A brand with no IAM origin has no session to end.
async fn revoke_all(brand: &str, removed: &[store::Removed]) {
    let Some(origin) = super::paths::server_url_for_brand(brand) else {
        return;
    };
    for r in removed {
        let Some(rt) = r.refresh_token.as_deref() else {
            continue;
        };
        if let Err(e) = oauth::revoke(origin, rt).await {
            crate::warn(&format!(
                "signed out of {brand} locally, but {} is still live at the server ({e}) — \
                 revoke it from your account's sessions",
                r.id
            ));
        }
    }
}

/// Drop every stored provider credential (OpenAI/Anthropic/`hk-`) and reset the
/// provider selection to the gateway default. Provider keys are global — not
/// brand-scoped — so this only runs for the default brand; any other brand is a
/// no-op. Returns the labels of the providers actually cleared.
fn clear_providers(cfg: &mut Config, brand: &str) -> Result<Vec<&'static str>> {
    if brand != DEFAULT_BRAND {
        return Ok(Vec::new());
    }
    let mut cleared = Vec::new();
    for p in [Provider::Hanzo, Provider::OpenAI, Provider::Anthropic] {
        if provider::clear(p)? {
            cleared.push(p.label());
        }
    }
    if cfg.auth.provider.is_some() {
        cfg.update(|c| {
            c.auth.provider = None;
            Ok(())
        })?;
    }
    Ok(cleared)
}
