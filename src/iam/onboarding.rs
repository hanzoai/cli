//! First-run onboarding + the multi-provider login picker.
//!
//! Two surfaces meet here:
//!   1. A delightful FRESH-MACHINE greeting — an animated ASCII wordmark, shown
//!      only on an interactive terminal (never piped/CI), theme-aware
//!      (`NO_COLOR`, dumb terminals) — then the login picker.
//!   2. The login DISPATCH — interactive (arrow-key picker) or non-interactive
//!      (`hanzo login --provider hanzo|openai|anthropic [--token -]`). A secret
//!      only ever arrives on stdin or an interactive hidden prompt, NEVER argv.
//!
//! WHERE credentials land is not reinvented: a Hanzo sign-in is the existing
//! OIDC/identity flow (`login`/`store`); a provider key is filed through the
//! `provider` seam over the SAME portable Vault. Only the NON-SECRET "which
//! provider is active" is written to the config index.

use anyhow::{anyhow, bail, Result};
use colored::*;
use std::io::IsTerminal;

use crate::config::Config;

use super::provider::{self, Provider};
use super::secret::{read_trimmed, secret_source, SecretSource};
use super::{login, oauth};

// ---- terminal capability (the piped/CI + NO_COLOR gate) --------------------

fn stdout_is_tty() -> bool {
    std::io::stdout().is_terminal()
}

fn stdin_is_tty() -> bool {
    std::io::stdin().is_terminal()
}

/// A picker needs BOTH ends to be a terminal: stdout to draw, stdin to read keys.
pub fn interactive() -> bool {
    stdout_is_tty() && stdin_is_tty()
}

fn term_dumb() -> bool {
    matches!(std::env::var("TERM").as_deref(), Ok("dumb"))
}

/// Emit ANSI color only for a capable, opted-in terminal.
fn color_on() -> bool {
    stdout_is_tty() && !term_dumb() && std::env::var_os("NO_COLOR").is_none()
}

/// Animate the reveal only on a real terminal, off under CI or an explicit
/// opt-out. Independent of color — a monochrome reveal is still a reveal.
fn animate_on() -> bool {
    stdout_is_tty()
        && !term_dumb()
        && std::env::var_os("CI").is_none()
        && std::env::var_os("HANZO_NO_ANIMATION").is_none()
}

// ---- first-run detection ---------------------------------------------------

/// A machine with NO credentials of any kind: no signed-in identity and no
/// active model provider. Read purely off the non-secret config index, so it
/// needs no Vault round-trip. This is the ONLY thing that gates the animated
/// greeting — a returning user never sees it again.
pub fn is_fresh(cfg: &Config) -> bool {
    cfg.auth.identities.is_empty() && cfg.auth.provider.is_none()
}

// ---- the wordmark ----------------------------------------------------------

/// The "hanzo" wordmark (FIGlet Standard). Monochrome-friendly: it reads with or
/// without color.
const WORDMARK: &str = r#" _
| |__   __ _ _ __  ____ ___
| '_ \ / _` | '_ \|_  // _ \
| | | | (_| | | | |/ / | (_) |
|_| |_|\__,_|_| |_/___| \___/"#;

const TAGLINE: &str = "code · models · agents — one login";

/// Show the wordmark. A no-op on a non-terminal, so piped/CI output stays clean.
/// Animated (a short line-by-line reveal, a few hundred ms total) on a capable
/// terminal; a single static print otherwise.
pub fn show_banner() {
    if !stdout_is_tty() {
        return; // piped / CI: never pollute the stream
    }
    let animate = animate_on();
    let color = color_on();

    println!();
    for line in WORDMARK.lines() {
        if color {
            println!("{}", line.cyan());
        } else {
            println!("{line}");
        }
        if animate {
            use std::io::Write;
            let _ = std::io::stdout().flush();
            std::thread::sleep(std::time::Duration::from_millis(45));
        }
    }
    if color {
        println!("  {}", TAGLINE.dimmed());
    } else {
        println!("  {TAGLINE}");
    }
    println!();
}

// ---- the picker ------------------------------------------------------------

/// What the user picked in the interactive menu.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Choice {
    Hanzo,
    OpenAI,
    Anthropic,
    Paste,
}

/// Draw the arrow-key menu and return the choice, or `None` if the user
/// cancelled (Esc / Ctrl-C). Uses `dialoguer` (already a dependency) — no TUI
/// crate is pulled in.
fn pick_provider() -> Result<Option<Choice>> {
    use dialoguer::{
        theme::{ColorfulTheme, SimpleTheme, Theme},
        Select,
    };

    let items = [
        "Hanzo       unified billing, every model through the gateway  (recommended)",
        "OpenAI      use your ChatGPT / OpenAI API key",
        "Anthropic   use your Claude / Anthropic API key",
        "Paste key   auto-detect the provider from the key prefix",
    ];

    // Colorful when the terminal supports it, plain (still arrow-key navigable)
    // under NO_COLOR / a dumb terminal.
    let colorful = ColorfulTheme::default();
    let simple = SimpleTheme;
    let theme: &dyn Theme = if color_on() { &colorful } else { &simple };

    let selection = Select::with_theme(theme)
        .with_prompt("How would you like to sign in?")
        .items(&items)
        .default(0)
        .interact_opt()?;

    Ok(selection.map(|i| match i {
        0 => Choice::Hanzo,
        1 => Choice::OpenAI,
        2 => Choice::Anthropic,
        _ => Choice::Paste,
    }))
}

// ---- reading a secret (stdin or hidden prompt — never argv) ----------------
// The argv-refusal decision (`secret_source`) and the key reader (`read_trimmed`)
// are the ONE stdin-secret law, in `iam::secret`; this module only resolves a
// provider key / identity token through them.

/// Prompt for a secret with a hidden (non-echoing) input.
fn prompt_secret(prompt: &str) -> Result<String> {
    use dialoguer::{
        theme::{ColorfulTheme, SimpleTheme, Theme},
        Password,
    };
    let colorful = ColorfulTheme::default();
    let simple = SimpleTheme;
    let theme: &dyn Theme = if color_on() { &colorful } else { &simple };
    let key = Password::with_theme(theme).with_prompt(prompt).interact()?;
    let key = key.trim().to_string();
    if key.is_empty() {
        bail!("no key entered");
    }
    Ok(key)
}

/// Resolve a provider-key secret from the `--token` flag / terminal, refusing an
/// argv literal outright.
fn read_key(token: Option<String>, prompt: &str) -> Result<String> {
    match secret_source(token.as_deref(), stdin_is_tty()) {
        SecretSource::Stdin => read_trimmed(std::io::stdin().lock()),
        SecretSource::Prompt => prompt_secret(prompt),
        SecretSource::ArgvRefused => bail!(
            "a key must never be passed on the command line (it would land in `ps` and shell history) \
             — pipe it on stdin with `--token -`, or run `hanzo login` interactively"
        ),
    }
}

/// Resolve a Hanzo IDENTITY token through the SAME stdin-only discipline as a
/// provider key ([`secret_source`]): `--token -` (or a pipe) reads stdin; an argv
/// literal is REFUSED so a JWT never lands in `ps`/shell history. One
/// credential-input law. The browser flow is the no-`--token` path and never
/// reaches here, so an explicit `--token` value may ONLY ever be `-`.
fn read_identity_token(token: String) -> Result<String> {
    match secret_source(Some(&token), stdin_is_tty()) {
        SecretSource::Stdin => read_trimmed(std::io::stdin().lock()),
        // `Some(_)` never resolves to `Prompt`; a literal is refused exactly as a key is.
        SecretSource::Prompt | SecretSource::ArgvRefused => bail!(
            "a token must never be passed on the command line (it would land in `ps` and shell \
             history) — pipe it on stdin with `--token -`, or run `hanzo login` for the browser flow"
        ),
    }
}

// ---- login dispatch --------------------------------------------------------

/// Persist which provider is now active (non-secret config index).
fn mark_provider(cfg: &mut Config, provider: Provider) -> Result<()> {
    cfg.update(|c| {
        c.auth.provider = Some(provider.slug().to_string());
        Ok(())
    })
}

fn confirm_provider(provider: Provider) {
    println!(
        "{} Stored your {} key — a coding session will call {} directly with it.",
        "✓".green(),
        provider.label().bold(),
        provider.label(),
    );
    println!(
        "{}",
        "  (`hanzo login` → Hanzo to route every model through the gateway with unified billing)".dimmed()
    );
}

fn warn(msg: &str) {
    eprintln!("{} {}", "warning:".yellow().bold(), msg);
}

/// Sign in as a Hanzo IDENTITY (the OIDC browser flow, or a supplied token), and
/// record `hanzo` as the active provider so model routing uses the gateway.
async fn hanzo_login(cfg: &mut Config, brand: &str, token: Option<String>) -> Result<()> {
    oauth::server_url(brand)?; // reject an unknown brand before any I/O
    match token {
        Some(t) => {
            let raw = read_identity_token(t)?;
            login::login_with_token(cfg, brand, &raw).await?;
        }
        None => login::login(cfg, brand).await?,
    }
    mark_provider(cfg, Provider::Hanzo)
}

/// Sign in with a provider's OWN key (OpenAI / Anthropic): read it off stdin or a
/// hidden prompt, file it in the Vault, and mark the provider active. A key whose
/// own prefix names a DIFFERENT vendor is REFUSED before anything is stored — see
/// [`refuse_provider_mismatch`].
async fn provider_key_login(cfg: &mut Config, provider: Provider, token: Option<String>) -> Result<()> {
    let key = read_key(token, &format!("Paste your {} API key", provider.label()))?;
    refuse_provider_mismatch(provider, &key)?; // fail CLOSED before any store
    provider::set_key(provider, &key)?;
    mark_provider(cfg, provider)?;
    confirm_provider(provider);
    Ok(())
}

/// Refuse filing `key` under `provider` when the key's OWN prefix names a
/// DIFFERENT vendor — fail CLOSED, never warn-and-store.
///
/// A key is filed under the provider it authenticates, and `code::route_plan`
/// routes by that provider label: a coding session sends the stored key, in an
/// auth header, to THAT vendor's API. So filing an OpenAI `sk-` key under
/// `--provider anthropic` would later transmit it to api.anthropic.com — leaking a
/// key to a vendor it was never issued for, and mislabeling it "your Anthropic
/// key" in the banner. Only a POSITIVE mismatch is refused (`detect` = `Some(other)`
/// where `other != provider`); an unrecognized prefix (`None`) is allowed through,
/// since the prefix table is deliberately not exhaustive.
fn refuse_provider_mismatch(provider: Provider, key: &str) -> Result<()> {
    if let Some(detected) = Provider::detect(key) {
        if detected != provider {
            bail!(
                "that looks like a {} key, but `--provider {}` was requested. A key is filed under \
                 the provider it authenticates and a coding session sends it there, so storing a {} \
                 key as {} would transmit it to the wrong API. Re-run with `--provider {}`, or run \
                 `hanzo login` and let the key's prefix pick the provider.",
                detected.label(),
                provider.slug(),
                detected.label(),
                provider.label(),
                detected.slug(),
            );
        }
    }
    Ok(())
}

/// The "paste a key" path: detect the provider from the key's prefix.
async fn paste_key_login(cfg: &mut Config, token: Option<String>) -> Result<()> {
    let key = read_key(token, "Paste your API key (hk-… / sk-ant-… / sk-…)")?;
    let provider = Provider::detect(&key).ok_or_else(|| {
        anyhow!(
            "could not recognize that key — expected an `hk-` (Hanzo gateway), `sk-ant-` (Anthropic) \
             or `sk-` (OpenAI) key"
        )
    })?;
    provider::set_key(provider, &key)?;
    mark_provider(cfg, provider)?;
    confirm_provider(provider);
    Ok(())
}

async fn dispatch_choice(cfg: &mut Config, brand: &str, choice: Choice) -> Result<()> {
    match choice {
        Choice::Hanzo => hanzo_login(cfg, brand, None).await,
        Choice::OpenAI => provider_key_login(cfg, Provider::OpenAI, None).await,
        Choice::Anthropic => provider_key_login(cfg, Provider::Anthropic, None).await,
        Choice::Paste => paste_key_login(cfg, None).await,
    }
}

/// `hanzo login` — the command entrypoint. `--provider` drives the
/// non-interactive path; without it, an interactive terminal gets the picker
/// (with the fresh-machine banner the first time) and a non-terminal falls back
/// to the browser OIDC flow, exactly as before.
pub async fn run_login(
    cfg: &mut Config,
    brand: &str,
    provider: Option<String>,
    token: Option<String>,
) -> Result<()> {
    if let Some(p) = provider {
        let provider = Provider::parse(&p)?;
        return match provider {
            Provider::Hanzo => hanzo_login(cfg, brand, token).await,
            Provider::OpenAI | Provider::Anthropic => provider_key_login(cfg, provider, token).await,
        };
    }

    // No explicit provider.
    if let Some(t) = token {
        // Back-compat: `hanzo login --token -` files a Hanzo identity.
        return hanzo_login(cfg, brand, Some(t)).await;
    }

    if interactive() {
        if is_fresh(cfg) {
            show_banner();
        }
        return match pick_provider()? {
            Some(choice) => dispatch_choice(cfg, brand, choice).await,
            None => {
                println!("Sign-in cancelled.");
                Ok(())
            }
        };
    }

    // Non-interactive, nothing specified: the browser OIDC default (unchanged).
    hanzo_login(cfg, brand, None).await
}

/// The bare-`hanzo` FRESH greeting: on an interactive terminal with no
/// credentials, show the banner + picker, then return so the caller can proceed
/// into the coding session. BEST-EFFORT — a cancel or a failed sign-in never
/// aborts the run; bare `hanzo` simply continues locally.
pub async fn first_run(cfg: &mut Config, brand: &str) {
    if !is_fresh(cfg) || !interactive() {
        return;
    }
    show_banner();
    match pick_provider() {
        Ok(Some(choice)) => {
            if let Err(e) = dispatch_choice(cfg, brand, choice).await {
                warn(&format!("sign-in did not complete ({e}) — continuing locally."));
            }
        }
        Ok(None) => println!(
            "{}",
            "Skipped — continuing locally. Run `hanzo login` any time to connect.".dimmed()
        ),
        Err(e) => warn(&format!("could not show the sign-in picker ({e}) — continuing locally.")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pristine_config_is_fresh() {
        assert!(is_fresh(&Config::default()));
    }

    #[test]
    fn a_signed_in_identity_is_not_fresh() {
        let mut cfg = Config::default();
        cfg.auth.identities.push(crate::config::StoredIdentity {
            brand: "hanzo".into(),
            owner: "hanzo".into(),
            name: "z".into(),
        });
        assert!(!is_fresh(&cfg));
    }

    #[test]
    fn a_stored_provider_is_not_fresh() {
        let mut cfg = Config::default();
        cfg.auth.provider = Some("anthropic".into());
        assert!(!is_fresh(&cfg));
    }

    // The argv-refusal law (`secret_source`) and the key reader (`read_trimmed`)
    // are pinned by `iam::secret`'s own tests — the ONE home for that law.

    /// LOW-3: an identity JWT obeys the SAME stdin-only law as a provider key — a
    /// literal on the command line is REFUSED (it would land in `ps`/shell
    /// history). The `-` (stdin) path is pinned by `secret_source` above.
    #[test]
    fn read_identity_token_refuses_an_argv_literal() {
        let err = read_identity_token("header.payload.sig".into()).unwrap_err().to_string();
        assert!(err.contains("must never be passed on the command line"), "{err}");
        assert!(read_identity_token("  eyJhbGci.body.sig  ".into()).is_err());
        // A `--token` value may only ever be `-` (stdin); that decision is the
        // shared `secret_source` law, already pinned above.
        assert_eq!(secret_source(Some("-"), true), SecretSource::Stdin);
    }

    /// MED-1: an OpenAI `sk-` key handed to `--provider anthropic` is REFUSED, and
    /// because the guard runs BEFORE any store — exactly as `provider_key_login`
    /// sequences it with `?` — the vault and the provider index stay untouched.
    #[test]
    fn a_mismatched_provider_key_is_refused_and_nothing_is_filed() {
        use crate::iam::token::memvault::MemVault;

        // The guard fails closed with a vendor-named, actionable error.
        let err = refuse_provider_mismatch(Provider::Anthropic, "sk-proj-OPENAI-KEY")
            .unwrap_err()
            .to_string();
        assert!(err.contains("OpenAI"), "names the detected vendor: {err}");
        assert!(err.contains("anthropic"), "names the requested provider: {err}");
        assert!(err.contains("hanzo login") || err.contains("--provider"), "actionable remedy: {err}");

        // Sequenced as the caller does (`guard?; set_key`), the store never runs,
        // so the vault stays empty and `auth.provider` is never marked.
        let v = MemVault::new();
        let cfg = Config::default();
        let filed = refuse_provider_mismatch(Provider::Anthropic, "sk-proj-OPENAI-KEY")
            .and_then(|_| provider::set_key_in(&v, Provider::Anthropic, "sk-proj-OPENAI-KEY"));
        assert!(filed.is_err(), "the mismatch must short-circuit before the store");
        assert!(v.keys().is_empty(), "a refused key must never reach the vault: {:?}", v.keys());
        assert!(provider::key_in(&v, Provider::Anthropic).unwrap().is_none());
        assert_eq!(cfg.auth.provider, None, "auth.provider must be unchanged on refusal");
    }

    /// A matching key (or an unrecognized prefix) passes the guard; every KNOWN
    /// cross-vendor pairing is refused. We only block a POSITIVE mismatch — the
    /// prefix table is deliberately not exhaustive.
    #[test]
    fn refuse_provider_mismatch_blocks_only_a_known_cross_vendor_key() {
        // Matching prefixes pass.
        assert!(refuse_provider_mismatch(Provider::Anthropic, "sk-ant-REAL").is_ok());
        assert!(refuse_provider_mismatch(Provider::OpenAI, "sk-proj-REAL").is_ok());
        assert!(refuse_provider_mismatch(Provider::OpenAI, "sk-REAL").is_ok());
        assert!(refuse_provider_mismatch(Provider::Hanzo, "hk-REAL").is_ok());
        // An unrecognized prefix is NOT a positive mismatch → allowed through.
        assert!(refuse_provider_mismatch(Provider::Anthropic, "mystery-format-key").is_ok());
        // Every known cross pairing is refused.
        assert!(refuse_provider_mismatch(Provider::Anthropic, "hk-x").is_err()); // Hanzo key
        assert!(refuse_provider_mismatch(Provider::Anthropic, "sk-proj-x").is_err()); // OpenAI key
        assert!(refuse_provider_mismatch(Provider::OpenAI, "sk-ant-x").is_err()); // Anthropic key
        assert!(refuse_provider_mismatch(Provider::Hanzo, "sk-ant-x").is_err()); // Anthropic key
        assert!(refuse_provider_mismatch(Provider::OpenAI, "hk-x").is_err()); // Hanzo key
    }
}
