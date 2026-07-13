//! `hanzo login` / `whoami` / `logout` — the user-facing IAM auth commands.
//!
//! ONE command, one way. `login` runs the browser Authorization-Code+PKCE flow
//! by default and the RFC 8628 device flow when there is no browser to open
//! (headless: no display / `--device` / the browser fails to launch). Tokens
//! live in the OS keychain ([`token`], upstream's deliberate choice); on a
//! successful DEFAULT-brand login they are ALSO mirrored to the ecosystem-shared
//! flat ~/.hanzo/credentials.json ([`credentials`]) so the Go `hanzo` CLI and
//! `hanzo-dev` share the session. `logout` clears both.

use anyhow::{bail, Result};
use colored::*;

use super::{credentials, device, oauth, paths, token};

/// `hanzo login [--brand] [--device]`: sign in and persist the credential.
pub async fn login(brand: &str, force_device: bool) -> Result<()> {
    let tokens = if force_device || headless() {
        device::login(brand).await?
    } else {
        // Browser flow; if no browser can be launched, fall back to device.
        match oauth::login(brand).await {
            Ok(t) => t,
            Err(e) if e.to_string() == oauth::BROWSER_OPEN_FAILED => device::login(brand).await?,
            Err(e) => return Err(e),
        }
    };

    token::store(brand, &tokens)?;
    // Mirror the default brand into the shared flat store for the wider
    // ecosystem (Go CLI, hanzo-dev). Other brands stay keychain-only.
    if brand == paths::DEFAULT_BRAND {
        credentials::mirror(&tokens)?;
    }

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

/// `hanzo whoami [--brand]`: resolve the stored token to an identity. Falls back
/// to the shared flat store when the keychain has no entry for the default brand.
pub async fn whoami(brand: &str) -> Result<()> {
    oauth::server_url(brand)?; // reject unknown brands before touching any store
    if let Some(tokens) = token::load(brand)? {
        let who = oauth::userinfo(brand, &tokens.access_token).await?;
        println!("{} {}", "sub:".dimmed(), who.sub);
        if let Some(email) = who.email {
            println!("{} {}", "email:".dimmed(), email);
        }
        if let Some(name) = who.name {
            println!("{} {}", "name:".dimmed(), name);
        }
        return Ok(());
    }

    // Keychain empty — fall back to the ecosystem-shared session (default brand).
    if brand == paths::DEFAULT_BRAND {
        let creds = credentials::Credentials::load()?;
        if creds.logged_in() {
            if !creds.subject.is_empty() {
                println!("{} {}", "subject:".dimmed(), creds.subject);
            }
            if !creds.owner.is_empty() {
                println!("{} {}", "org:".dimmed(), creds.owner);
            }
            return Ok(());
        }
    }
    bail!(
        "not signed in to {brand} — run `hanzo login{}`",
        brand_flag(brand)
    );
}

/// `hanzo logout [--brand]`: delete the stored credential from BOTH stores.
pub async fn logout(brand: &str) -> Result<()> {
    oauth::server_url(brand)?; // reject unknown brands before touching any store
    let mut removed = token::delete(brand)?;
    // The shared flat store only ever holds the default brand's session.
    if brand == paths::DEFAULT_BRAND {
        removed |= credentials::logout()?;
    }
    if removed {
        println!("{} Signed out of {}", "✓".green(), brand.cyan());
    } else {
        println!("Not signed in to {brand}; nothing to do.");
    }
    Ok(())
}

/// Whether to skip the browser flow: no usable display for a browser to open
/// on. macOS/Windows always have a GUI; on Linux/BSD a browser needs an X11 or
/// Wayland display.
fn headless() -> bool {
    if cfg!(any(target_os = "macos", target_os = "windows")) {
        return false;
    }
    std::env::var_os("DISPLAY").is_none() && std::env::var_os("WAYLAND_DISPLAY").is_none()
}

/// The `--brand` suffix to suggest, omitted for the default brand.
fn brand_flag(brand: &str) -> String {
    if brand == paths::DEFAULT_BRAND {
        String::new()
    } else {
        format!(" --brand {brand}")
    }
}
