//! `hanzo login` / `whoami` / `logout` — the user-facing IAM auth commands.
//!
//! Thin orchestration over [`oauth`] (protocol) and [`token`] (keychain): run
//! the flow, persist/read/delete the credential, present the result.

use anyhow::{bail, Result};
use colored::*;

use super::{oauth, paths, token};

/// `hanzo login [--brand]`: interactive OIDC PKCE sign-in; store tokens in the
/// OS keychain (never on disk).
pub async fn login(brand: &str) -> Result<()> {
    let tokens = oauth::login(brand).await?;
    token::store(brand, &tokens)?;

    let who = oauth::userinfo(brand, &tokens.access_token).await?;
    let label = who
        .email
        .or(who.preferred_username)
        .unwrap_or_else(|| who.sub.clone());
    println!(
        "{} Signed in to {} as {}",
        "✓".green(),
        brand.cyan(),
        label.bold()
    );
    Ok(())
}

/// `hanzo whoami [--brand]`: resolve the stored token to an identity.
pub async fn whoami(brand: &str) -> Result<()> {
    oauth::server_url(brand)?; // reject unknown brands before touching the keychain
    let Some(tokens) = token::load(brand)? else {
        bail!("not signed in to {brand} — run `hanzo login{}`", brand_flag(brand));
    };
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

/// `hanzo logout [--brand]`: delete the stored credential.
pub async fn logout(brand: &str) -> Result<()> {
    oauth::server_url(brand)?; // reject unknown brands before touching the keychain
    if token::delete(brand)? {
        println!("{} Signed out of {}", "✓".green(), brand.cyan());
    } else {
        println!("Not signed in to {brand}; nothing to do.");
    }
    Ok(())
}

/// The `--brand` suffix to suggest, omitted for the default brand.
fn brand_flag(brand: &str) -> String {
    if brand == paths::DEFAULT_BRAND {
        String::new()
    } else {
        format!(" --brand {brand}")
    }
}
