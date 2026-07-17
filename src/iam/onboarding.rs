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
use std::io::{IsTerminal, Read};

use crate::config::Config;

use super::provider::{self, Provider};
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

/// Where a login secret may come from, decided from the `--token` flag and
/// whether stdin is a terminal. PURE — the actual read is done by the caller —
/// so the "never argv" invariant is unit-testable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SecretSource {
    /// Read the whole of stdin (`--token -`, or a pipe with no flag).
    Stdin,
    /// Prompt interactively with a hidden input (a terminal, no `--token`).
    Prompt,
    /// A literal was passed in argv — REFUSED for a key.
    ArgvRefused,
}

fn secret_source(token: Option<&str>, stdin_tty: bool) -> SecretSource {
    match token {
        Some("-") => SecretSource::Stdin,
        Some(_) => SecretSource::ArgvRefused,
        None if !stdin_tty => SecretSource::Stdin, // piped input, e.g. `printf %s "$K" | hanzo login --provider openai`
        None => SecretSource::Prompt,
    }
}

/// Read a secret from `r`, trimmed; error if empty. The stdin path.
fn read_secret_from<R: Read>(mut r: R) -> Result<String> {
    let mut s = String::new();
    r.read_to_string(&mut s).map_err(|e| anyhow!("reading key from stdin: {e}"))?;
    let s = s.trim().to_string();
    if s.is_empty() {
        bail!("no key provided on stdin");
    }
    Ok(s)
}

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
        SecretSource::Stdin => read_secret_from(std::io::stdin().lock()),
        SecretSource::Prompt => prompt_secret(prompt),
        SecretSource::ArgvRefused => bail!(
            "a key must never be passed on the command line (it would land in `ps` and shell history) \
             — pipe it on stdin with `--token -`, or run `hanzo login` interactively"
        ),
    }
}

/// Resolve a Hanzo IDENTITY token argument: `-` reads stdin, a literal is used
/// as given (the existing `login --token` back-compat, for a JWT).
fn read_identity_token(token: String) -> Result<String> {
    let raw = if token == "-" {
        read_secret_from(std::io::stdin().lock())?
    } else {
        token.trim().to_string()
    };
    if raw.is_empty() {
        bail!("no token provided");
    }
    Ok(raw)
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
/// hidden prompt, file it in the Vault, and mark the provider active.
async fn provider_key_login(cfg: &mut Config, provider: Provider, token: Option<String>) -> Result<()> {
    let key = read_key(token, &format!("Paste your {} API key", provider.label()))?;
    // Soft prefix check: store what the user asked for, but flag an obvious mixup.
    if let Some(detected) = Provider::detect(&key) {
        if detected != provider {
            warn(&format!(
                "that looks like a {} key, not {} — storing it as {} as you asked",
                detected.label(),
                provider.label(),
                provider.label()
            ));
        }
    }
    provider::set_key(provider, &key)?;
    mark_provider(cfg, provider)?;
    confirm_provider(provider);
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

    /// THE invariant: a key never comes from argv. A literal token is refused; a
    /// `-` (or a pipe) reads stdin; a bare terminal prompts.
    #[test]
    fn secret_source_never_takes_a_key_from_argv() {
        assert_eq!(secret_source(Some("-"), true), SecretSource::Stdin);
        assert_eq!(secret_source(Some("-"), false), SecretSource::Stdin);
        // A literal on the command line is ALWAYS refused, TTY or not.
        assert_eq!(secret_source(Some("sk-ant-literal"), true), SecretSource::ArgvRefused);
        assert_eq!(secret_source(Some("sk-ant-literal"), false), SecretSource::ArgvRefused);
        // No flag: a pipe feeds stdin; an interactive terminal prompts.
        assert_eq!(secret_source(None, false), SecretSource::Stdin);
        assert_eq!(secret_source(None, true), SecretSource::Prompt);
    }

    #[test]
    fn read_secret_from_trims_and_rejects_empty() {
        assert_eq!(read_secret_from(std::io::Cursor::new("  sk-ant-xyz\n")).unwrap(), "sk-ant-xyz");
        assert_eq!(read_secret_from(std::io::Cursor::new("hk-abc")).unwrap(), "hk-abc");
        assert!(read_secret_from(std::io::Cursor::new("   \n ")).is_err(), "whitespace-only is empty");
        assert!(read_secret_from(std::io::Cursor::new("")).is_err());
    }

    #[test]
    fn read_identity_token_uses_a_literal_as_given() {
        assert_eq!(read_identity_token("  header.payload.sig  ".into()).unwrap(), "header.payload.sig");
        assert!(read_identity_token("   ".into()).is_err());
    }
}
