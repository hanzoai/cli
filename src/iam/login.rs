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
use std::io::IsTerminal;

use crate::config::Config;

use super::identity::{Identity, Selector};
use super::paths::{brand_flag, DEFAULT_BRAND};
use super::provider::{self, Provider};
use super::token::TokenSet;
use super::{device, oauth, store};

/// `hanzo auth login [--brand] [--account A] [--choose]`: interactive OIDC
/// sign-in. ADDS an identity (never clobbers another). It becomes the active
/// identity when the person chose it — in the chooser, or by `--account` — or
/// when nothing else was active; an account hanzo.id answered with on its own
/// is added beside the active identity, never in its place.
///
/// Holding more than one identity, the sign-in asks hanzo.id for its account
/// chooser (`prompt=select_account`) rather than continuing as whichever
/// account the browser signed in last. `--account` names the account instead
/// (`login_hint`) and `--choose` asks for the chooser regardless.
///
/// THE MACHINE PICKS THE FLOW, and there is no flag, because which one works is
/// a fact about the machine rather than a preference. A browser HERE takes the
/// authorization code back over loopback, which is instant and asks nothing. No
/// browser here and `127.0.0.1` is the wrong address by construction — it names
/// this machine, and the person is at another one — so the code cannot come back
/// that way at all, and the device grant (RFC 8628) is the flow built for it: a
/// short code they read off this screen and approve on any device they already
/// hold. A `--device` switch would be asking a person to predict the failure it
/// exists to avoid. The device grant runs only for a client IAM's descriptor
/// grants it to (`admin-cli` is not: a device code approvable by a session
/// cookie alone is one click from a SuperAdmin token); otherwise the person
/// signs in anywhere and pastes the callback URL back here.
pub async fn login(cfg: &mut Config, brand: &str, account: Option<&str>, choose: bool) -> Result<()> {
    let brand: &str = &store::realm(cfg, brand)?;
    let origin = &oauth::server_url(brand)?; // reject unknown brands before opening a browser
    let want = account.map(|a| wanted(cfg, brand, a)).transpose()?;
    // `--as <org>` signs in to hold another identity — `admin/z` beside
    // `hanzo/z` — through that org's own client, so IAM signs the person in to
    // that org. An `--account` in another org names that org the same way. A
    // missing client is refused here, before any browser opens.
    let org = client_org(cfg.org.as_deref(), want.as_ref())?;
    let device = oauth::served(origin, org.as_deref()).await?;
    let before = store::active(cfg, brand);
    let ask = oauth::Ask {
        choose: chooser(choose, want.is_some(), org.is_some(), store::list(cfg, brand).len()),
        hint: want.as_ref().map(|w| w.hint.clone()),
    };
    let browser = browser_here();
    let filed = if by_device(browser, device) {
        let tokens = device::login(origin, brand, org.as_deref()).await?;
        file(cfg, brand, &tokens, org.as_deref(), before.as_ref(), &ask, want.as_ref())?
    } else {
        match oauth::login(brand, org.as_deref(), browser, &ask).await? {
            oauth::Done::Pasted(tokens) => {
                file(cfg, brand, &tokens, org.as_deref(), before.as_ref(), &ask, want.as_ref())?
            }
            oauth::Done::Browser(tokens, held) => {
                // The page says who the terminal goes on as, so that is decided
                // first; the tab is answered while the keychain write runs.
                let id = Identity::from_access_token(&tokens.access_token)?;
                let chosen = ask.choose || want.as_ref().is_some_and(|w| w.names(&id, &tokens));
                let kept = before
                    .clone()
                    .filter(|prev| !activates(Some(prev), &id, chosen, cfg.org.is_some()));
                oauth::finish(held, &tokens, kept.as_ref(), async {
                    file(cfg, brand, &tokens, org.as_deref(), before.as_ref(), &ask, want.as_ref())
                })
                .await?
            }
        }
    };
    report(cfg, brand, &filed, want.as_ref()).await;
    Ok(())
}

/// The account `--account` names: what hanzo.id is asked for (`login_hint`),
/// and the identity, when it is given as `owner/name`.
struct Wanted {
    raw: String,
    hint: String,
    id: Option<Identity>,
}

impl Wanted {
    /// Whether a token is the account that was asked for.
    fn names(&self, id: &Identity, tokens: &TokenSet) -> bool {
        match &self.id {
            Some(want) => want == id,
            None if self.raw.contains('@') => super::identity::email(&tokens.access_token)
                .is_some_and(|email| email.eq_ignore_ascii_case(&self.raw)),
            None => {
                self.raw == id.name
                    || super::identity::subject(&tokens.access_token).as_deref() == Some(self.raw.as_str())
            }
        }
    }
}

/// Read `--account`. An address is the hint as given. `owner/name` held here
/// is asked for by its subject, which no two accounts share (an address can
/// belong to one person in two orgs); one not held, by its name. Anything else
/// is a username or a subject, which IAM reads as they are.
fn wanted(cfg: &Config, brand: &str, account: &str) -> Result<Wanted> {
    let raw = account.trim();
    if raw.is_empty() {
        bail!("--account needs an `owner/name` or an email address");
    }
    if raw.contains('@') || !raw.contains('/') {
        return Ok(Wanted { raw: raw.to_string(), hint: raw.to_string(), id: None });
    }
    let Selector::Exact(id) = raw.parse::<Selector>()? else {
        bail!("--account {raw} is not an `owner/name`");
    };
    let held = if store::list(cfg, brand).contains(&id) {
        store::token_for(cfg, brand, &id).ok().flatten()
    } else {
        None
    };
    Ok(Wanted {
        raw: raw.to_string(),
        hint: subject_hint(held.as_ref(), &id),
        id: Some(id),
    })
}

/// The hint for an `owner/name`: the subject its stored token claims, else its name.
fn subject_hint(held: Option<&TokenSet>, id: &Identity) -> String {
    held.and_then(|tokens| super::identity::subject(&tokens.access_token))
        .unwrap_or_else(|| id.name.clone())
}

/// The org whose client signs this in: `--as`, else the org `--account` names
/// when that org has a client of its own. The two must agree.
fn client_org(as_org: Option<&str>, want: Option<&Wanted>) -> Result<Option<String>> {
    let owner = want.and_then(|w| w.id.as_ref()).map(|id| id.owner.as_str());
    match (as_org, owner) {
        (Some(org), Some(owner)) if org != owner => {
            bail!("`--as {org}` signs in to {org}, and --account names an identity in {owner}")
        }
        (Some(org), _) => Ok(Some(org.to_string())),
        (None, Some(owner)) if oauth::client(Some(owner)) != oauth::client(None) => Ok(Some(owner.to_string())),
        (None, _) => Ok(None),
    }
}

/// Whether hanzo.id shows its account chooser. Asked for, it does. A named
/// account needs none. Otherwise it does when the sign-in could land on the
/// wrong account: under `--as`, or holding more than one identity, where the
/// browser's latest session is a guess at which.
fn chooser(choose: bool, named: bool, under_as: bool, held: usize) -> bool {
    choose || (!named && (under_as || held > 1))
}

/// Whether a sign-in that returned `id` makes it the active identity. It does
/// when nothing was active, when it IS the active one, or when the person chose
/// it. An account nobody chose — the session the browser already held — is
/// added beside the active identity. Under `--as` the default stays as it was:
/// that identity speaks when `--as` names it.
fn activates(before: Option<&Identity>, id: &Identity, chosen: bool, under_as: bool) -> bool {
    match before {
        None => true,
        Some(prev) if prev == id => true,
        Some(_) => chosen && !under_as,
    }
}

/// What a sign-in filed: the identity, and the one active afterwards.
struct Filed {
    id: Identity,
    active: Option<Identity>,
}

/// File a token set as its own identity, activating it only as [`activates`]
/// says. Under `--as` (or an `--account` in another org) the token must be that
/// org's.
fn file(
    cfg: &mut Config,
    brand: &str,
    tokens: &TokenSet,
    org: Option<&str>,
    before: Option<&Identity>,
    ask: &oauth::Ask,
    want: Option<&Wanted>,
) -> Result<Filed> {
    oauth::owned(tokens, org)?;
    let id = Identity::from_access_token(&tokens.access_token)?;
    let chosen = ask.choose || want.is_some_and(|w| w.names(&id, tokens));
    let activate = activates(before, &id, chosen, cfg.org.is_some());
    let id = store::file(cfg, brand, tokens, activate)?;
    Ok(Filed { id, active: store::active(cfg, brand) })
}

/// The device grant only with no browser here and a client that permits it;
/// otherwise the browser flow, whose paste leg needs no browser on this machine.
fn by_device(browser: bool, device: bool) -> bool {
    !browser && device
}

/// Whether a browser can open ON THIS MACHINE. macOS and Windows always have a
/// GUI; on Linux and the BSDs a browser needs a display, and a shell with
/// neither is a container, an ssh session or CI — every one of them a place
/// where a loopback redirect lands on somebody else's computer.
fn browser_here() -> bool {
    if cfg!(any(target_os = "macos", target_os = "windows")) {
        return true;
    }
    std::env::var_os("DISPLAY").is_some() || std::env::var_os("WAYLAND_DISPLAY").is_some()
}

/// `hanzo auth login --token`: store a hanzo.id bearer directly, for a shell
/// that cannot open a browser (CI, a container, a remote host). Same identity law
/// as the browser flow — the principal comes from the token's own claims, never
/// from the caller.
pub async fn login_with_token(cfg: &mut Config, brand: &str, access_token: &str) -> Result<()> {
    let brand: &str = &store::realm(cfg, brand)?;
    let before = store::active(cfg, brand);
    let tokens = TokenSet {
        access_token: access_token.to_string(),
        token_type: "Bearer".to_string(),
        refresh_token: None,
        id_token: None,
        expires_in: None,
        scope: None,
    };
    // A token handed over is the account the person chose to hand over.
    let ask = oauth::Ask { choose: true, hint: None };
    let org = cfg.org.clone();
    let filed = file(cfg, brand, &tokens, org.as_deref(), before.as_ref(), &ask, None)?;
    report(cfg, brand, &filed, None).await;
    Ok(())
}

/// Say who signed in and who the terminal goes on as. The identity on display
/// is the token's own claim; userinfo, best effort, confirms the credential
/// works and supplies the address a person recognises.
async fn report(cfg: &Config, brand: &str, filed: &Filed, want: Option<&Wanted>) {
    let id = &filed.id;
    let tokens = store::token_for(cfg, brand, id).ok().flatten();
    let label = match &tokens {
        Some(tokens) => match tokio::time::timeout(
            std::time::Duration::from_secs(2),
            oauth::userinfo(brand, &tokens.access_token),
        )
        .await
        {
            Ok(Ok(who)) => who.email.or(who.preferred_username).unwrap_or(who.sub),
            _ => super::identity::email(&tokens.access_token).unwrap_or_else(|| id.name.clone()),
        },
        None => id.name.clone(),
    };
    println!(
        "{} Signed in to {} as {} ({})",
        "✓".green(),
        brand.cyan(),
        id.to_string().bold(),
        label.dimmed()
    );
    if let Some(want) = want.filter(|w| tokens.as_ref().is_some_and(|t| !w.names(id, t))) {
        crate::warn(&format!(
            "you asked for {}, and hanzo.id signed in {id}; `hanzo auth login --account {}` asks again",
            want.raw, want.raw
        ));
    }
    println!("{}", standing(id, filed.active.as_ref(), cfg.org.is_some()));
    let held = store::list(cfg, brand).len();
    if held > 1 {
        println!(
            "{}",
            format!("  {held} identities on {brand} — `hanzo auth list` to list, `hanzo auth use` to change")
                .dimmed()
        );
    }
}

/// The line under "Signed in as": which identity the terminal goes on as.
fn standing(id: &Identity, active: Option<&Identity>, under_as: bool) -> String {
    match active {
        Some(active) if active == id => format!("  {} {}", "active:".dimmed(), active.to_string().bold()),
        Some(active) if under_as => {
            format!("  {active} stays the default; `hanzo --as {} …` speaks as {id}", id.owner)
                .dimmed()
                .to_string()
        }
        Some(active) => format!(
            "  {} {} {}\n{}",
            "active:".dimmed(),
            active.to_string().bold(),
            "(unchanged)".dimmed(),
            format!("  `hanzo auth use {id}` makes {id} active").dimmed()
        ),
        None => format!("  no active identity — `hanzo auth use {id}`").dimmed().to_string(),
    }
}

/// `hanzo auth show [--brand] [--all]`: the ACTIVE identity, or every identity.
///
/// Listing lives here rather than behind a separate `identities` verb: one
/// question ("who am I?"), one command, one way.
pub async fn whoami(cfg: &mut Config, brand: &str, all: bool) -> Result<()> {
    let brand: &str = &store::realm(cfg, brand)?;
    oauth::server_url(brand)?; // reject unknown brands before touching the keychain

    if all {
        if store::list(cfg, brand).is_empty() {
            bail!("not signed in to {brand} — run `hanzo auth login{}`", brand_flag(brand));
        }
        println!("{}", listing(cfg, brand));
        println!("{}", "  (* = active; owner is the billing org)".dimmed());
        return Ok(());
    }

    store::within(cfg, brand)?;
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

/// Every identity for `brand`, the active one marked, each with the address
/// its stored token claims: the same `owner/name` that `login` reports and
/// `use` takes, and the address a person knows it by. No network.
fn listing(cfg: &Config, brand: &str) -> String {
    let active = store::active(cfg, brand);
    store::list(cfg, brand)
        .iter()
        .map(|id| {
            let mark = if Some(id) == active.as_ref() { "*" } else { " " };
            let email = store::token_for(cfg, brand, id)
                .ok()
                .flatten()
                .and_then(|tokens| super::identity::email(&tokens.access_token));
            match email {
                Some(email) => format!("  {mark} {id}  {}", email.dimmed()),
                None => format!("  {mark} {id}"),
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// `hanzo auth use [IDENTITY] [--brand]`: select the active identity.
pub fn switch(cfg: &mut Config, brand: &str, identity: Option<String>) -> Result<()> {
    let brand: &str = &store::realm(cfg, brand)?;
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
/// brand clears them — `hanzo dev` uses the default brand.
///
/// Signing out ENDS THE SESSION, which means telling IAM: the refresh token this
/// client holds lives 30 days, so a logout that only forgot the local copy left a
/// month of spendable access on a machine you had signed out of. Every credential
/// dropped here is revoked at the server (RFC 7009), which kills its whole
/// rotation family.
pub async fn logout(cfg: &mut Config, brand: &str, identity: Option<String>, all: bool) -> Result<()> {
    let brand: &str = &store::realm(cfg, brand)?;
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
            end_browser(brand, &removed).await;
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

    // Unnamed under `--as <org>`, the identity signed out is the one held there,
    // never the default.
    let sel = match identity {
        Some(s) => Some(s.parse::<Selector>()?),
        None => store::within(cfg, brand)?.map(Selector::Exact),
    };
    let removed = store::remove(cfg, brand, sel)?;
    revoke_all(brand, std::slice::from_ref(&removed)).await;
    println!("{} Signed out of {} as {}", "✓".green(), brand.cyan(), removed.id.to_string().bold());
    end_browser(brand, std::slice::from_ref(&removed)).await;

    // Say what is left and how to select it — never select it for them.
    match store::active(cfg, brand) {
        Some(active) => println!("{} {}", "active:".dimmed(), active.to_string().bold()),
        None if !store::list(cfg, brand).is_empty() => {
            println!(
                "{}",
                format!("  no active identity on {brand} — `hanzo auth use <owner/name>`:").dimmed()
            );
            println!("{}", listing(cfg, brand));
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
        let Some(held) = &r.held else {
            continue;
        };
        if let Err(e) = oauth::revoke(&origin, held).await {
            crate::warn(&format!(
                "signed out of {brand} locally, but {} is still live at the server ({e}) — \
                 revoke it from your account's sessions",
                r.id
            ));
        }
    }
}

/// End the browser's session at the issuer too (OIDC RP-Initiated Logout), so
/// the next `hanzo auth login` asks which account instead of continuing as the
/// one that just signed out. Once per sign-out, whatever was removed: IAM ends
/// every session that browser holds. It goes through the CLI's own client,
/// whose registered loopback brings the tab back here; with no browser on this
/// machine, or no terminal to wait at, it prints the link instead.
async fn end_browser(brand: &str, removed: &[store::Removed]) {
    let Some(origin) = super::paths::server_url_for_brand(brand) else {
        return;
    };
    if removed.is_empty() {
        return;
    }
    let host = origin.split("://").nth(1).unwrap_or(&origin).to_string();
    let open = browser_here() && std::io::stdin().is_terminal();
    match oauth::end_session(&origin, &oauth::client(None), open).await {
        Ok(true) => println!("{} Signed out of {} in this browser", "✓".green(), host.cyan()),
        Ok(false) if open => crate::warn(&format!(
            "the browser did not come back from {host}, so it may still be signed in there; \
             `hanzo auth login --choose` asks which account next time"
        )),
        Ok(false) => {}
        Err(e) => crate::warn(&format!("could not sign this browser out of {host}: {e}")),
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

#[cfg(test)]
mod tests {
    use super::*;

    use crate::iam::identity::testjwt::claims_jwt;

    fn id(s: &str) -> Identity {
        match s.parse::<Selector>().unwrap() {
            Selector::Exact(id) => id,
            Selector::Owner(_) => unreachable!(),
        }
    }

    fn tokens(claims: &str) -> TokenSet {
        TokenSet {
            access_token: claims_jwt(claims),
            token_type: "Bearer".into(),
            refresh_token: None,
            id_token: None,
            expires_in: None,
            scope: None,
        }
    }

    /// Holding several identities, the browser's latest session is a guess,
    /// so the chooser shows; a named account needs none; one identity, or
    /// none, signs in as before.
    #[test]
    fn the_chooser_shows_when_the_latest_session_would_be_a_guess() {
        assert!(chooser(false, false, false, 6), "six identities held");
        assert!(!chooser(false, false, false, 1), "one held: unchanged");
        assert!(!chooser(false, false, false, 0), "a first sign-in: unchanged");
        assert!(chooser(false, false, true, 1), "--as asks, as it always did");
        assert!(!chooser(false, true, false, 6), "--account names it");
        assert!(chooser(true, true, false, 0), "--choose always shows it");
    }

    /// The case that stranded hanzo/z: a second sign-in that hanzo.id answered
    /// from the browser's session made that account active. Unchosen, it is
    /// only added.
    #[test]
    fn only_a_chosen_account_replaces_the_active_identity() {
        let (z, grundy) = (id("hanzo/z"), id("hanzo/grundy"));
        assert!(!activates(Some(&z), &grundy, false, false), "silent SSO replaced the active identity");
        assert!(activates(Some(&z), &grundy, true, false), "the person chose it");
        assert!(activates(None, &grundy, false, false), "nothing was active");
        assert!(activates(Some(&z), &z, false, false), "the same identity, renewed");
        assert!(!activates(Some(&z), &id("admin/z"), true, true), "--as keeps the default");
    }

    /// `--account owner/name` signs in through that org's own client when it
    /// has one, and must agree with `--as`.
    #[test]
    fn an_account_in_another_org_names_that_orgs_client() {
        let want = |raw: &str| Wanted { raw: raw.into(), hint: String::new(), id: Some(id(raw)) };
        assert_eq!(client_org(None, Some(&want("hanzo/z"))).unwrap(), None);
        assert_eq!(client_org(None, Some(&want("admin/z"))).unwrap().as_deref(), Some("admin"));
        assert_eq!(client_org(Some("admin"), Some(&want("admin/z"))).unwrap().as_deref(), Some("admin"));
        assert!(client_org(Some("admin"), Some(&want("hanzo/z"))).is_err());
        assert_eq!(client_org(Some("admin"), None).unwrap().as_deref(), Some("admin"));
        assert_eq!(client_org(None, None).unwrap(), None);
    }

    /// Whether the token that came back is the account asked for, by identity,
    /// by address in any case, or by username or subject.
    #[test]
    fn the_account_asked_for_is_recognised_in_the_token() {
        let t = tokens(r#"{"owner":"hanzo","name":"z","sub":"u-9","email":"z@hanzo.ai"}"#);
        let z = id("hanzo/z");
        let ask = |raw: &str, id: Option<Identity>| Wanted { raw: raw.into(), hint: String::new(), id };
        assert!(ask("hanzo/z", Some(id("hanzo/z"))).names(&z, &t));
        assert!(!ask("admin/z", Some(id("admin/z"))).names(&z, &t));
        assert!(ask("Z@Hanzo.ai", None).names(&z, &t));
        assert!(!ask("grundy@example.com", None).names(&z, &t));
        assert!(ask("z", None).names(&z, &t));
        assert!(ask("u-9", None).names(&z, &t));
        assert!(!ask("u-1", None).names(&z, &t));
    }

    /// A held `owner/name` is asked for by its subject, which no two accounts
    /// share; one not held, by its name.
    #[test]
    fn a_held_identity_is_hinted_by_its_subject() {
        let t = tokens(r#"{"owner":"hanzo","name":"z","sub":"6f1c2a3b-4d5e-4f60-8a7b-9c0d1e2f3a4b"}"#);
        assert_eq!(subject_hint(Some(&t), &id("hanzo/z")), "6f1c2a3b-4d5e-4f60-8a7b-9c0d1e2f3a4b");
        assert_eq!(subject_hint(None, &id("hanzo/z")), "z");
    }

    /// `s` without its terminal colours.
    fn plain(s: &str) -> String {
        let mut out = String::new();
        let mut chars = s.chars();
        while let Some(c) = chars.next() {
            if c == '\x1b' {
                for c in chars.by_ref() {
                    if c == 'm' {
                        break;
                    }
                }
            } else {
                out.push(c);
            }
        }
        out
    }

    /// The line under "Signed in as" says which identity the terminal uses.
    #[test]
    fn the_report_says_which_identity_is_active() {
        let (z, grundy) = (id("hanzo/z"), id("hanzo/grundy"));
        assert_eq!(plain(&standing(&z, Some(&z), false)), "  active: hanzo/z");
        let kept = plain(&standing(&grundy, Some(&z), false));
        assert!(kept.contains("active: hanzo/z (unchanged)"), "{kept}");
        assert!(kept.contains("`hanzo auth use hanzo/grundy` makes hanzo/grundy active"), "{kept}");
        assert!(plain(&standing(&id("admin/z"), Some(&z), true)).contains("hanzo/z stays the default"));
        assert!(plain(&standing(&z, None, false)).contains("no active identity"));
    }

    /// A client without the device grant (`admin-cli`) signs in by the paste
    /// leg on a machine with no browser; one with it (`hanzo-cli`) by device
    /// code; a browser here always takes the loopback.
    #[test]
    fn the_device_grant_runs_only_headless_and_only_where_the_client_permits_it() {
        assert!(!by_device(false, false), "admin-cli, no browser: paste leg");
        assert!(by_device(false, true), "hanzo-cli, no browser: device code");
        assert!(!by_device(true, true));
        assert!(!by_device(true, false));
    }
}
