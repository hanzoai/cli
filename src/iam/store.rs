//! The identity store — THE one way any command resolves a credential.
//!
//! Composes the two halves that `commands::wallet` already established: the
//! secret lives in the OS keychain (`token`), the non-secret index lives in
//! `config.toml` (`config::AuthState`). Nothing else reads a token; every
//! consumer goes through [`active_token`], so "which identity am I?" has exactly
//! one answer in exactly one place.
//!
//! HARD INVARIANT — the active identity changes ONLY by explicit user action
//! (`hanzo login`, `hanzo switch`). There is no auto-switch, no fallback and no
//! cascade: if the active identity's credential is missing, the run is
//! UNAUTHENTICATED. It never quietly becomes some other identity you happen to
//! hold. Acting as the wrong principal is worse than not acting.

use anyhow::{anyhow, bail, Context, Result};

use crate::config::{Config, StoredIdentity};

use super::identity::{Identity, Selector};
use super::oauth;
use super::paths::brand_flag;
use super::token::{self, TokenSet, Vault};

/// Add the identity `tokens` authenticates as, and make it active.
///
/// There is NO identity parameter, by design: the identity is derived from the
/// token's own claims, so no caller — not even this crate — can file a
/// credential under a principal of its choosing. Re-adding an identity that is
/// already known UPDATES it in place; it never duplicates the index row.
pub fn add(cfg: &mut Config, brand: &str, tokens: &TokenSet) -> Result<Identity> {
    add_in(&*token::vault()?, cfg, brand, tokens)
}

/// Resolve the ACTIVE identity's credential for `brand`.
///
/// `None` means "not signed in" — never "signed in as somebody else". Returns
/// the identity alongside the token because the two must not drift: callers that
/// need to know WHO they are (billing, org-scoped resume) read it from here
/// rather than re-deriving it somewhere else.
pub fn active_token(cfg: &mut Config, brand: &str) -> Result<Option<(Identity, TokenSet)>> {
    active_token_in(&*token::vault()?, cfg, brand)
}

/// Set the active identity for `brand`.
pub fn switch(cfg: &mut Config, brand: &str, sel: Option<Selector>) -> Result<Identity> {
    switch_in(&*token::vault()?, cfg, brand, sel)
}

/// Remove ONE identity: its keychain entry and its index row.
pub fn remove(cfg: &mut Config, brand: &str, sel: Option<Selector>) -> Result<Identity> {
    remove_in(&*token::vault()?, cfg, brand, sel)
}

/// Remove EVERY identity for `brand` (`hanzo logout --all`).
pub fn remove_all(cfg: &mut Config, brand: &str) -> Result<Vec<Identity>> {
    remove_all_in(&*token::vault()?, cfg, brand)
}

/// Every identity known for `brand`, in stable display order.
pub fn list(cfg: &Config, brand: &str) -> Vec<Identity> {
    let mut ids: Vec<Identity> = cfg
        .auth
        .identities
        .iter()
        .filter(|i| i.brand == brand)
        .filter_map(|i| format!("{}/{}", i.owner, i.name).parse::<Selector>().ok())
        .filter_map(|s| match s {
            Selector::Exact(id) => Some(id),
            Selector::Owner(_) => None,
        })
        .collect();
    ids.sort();
    ids.dedup();
    ids
}

/// The active identity for `brand`, if one is set AND still indexed. A pointer
/// at an unknown identity resolves to `None` — it never falls through to another.
pub fn active(cfg: &Config, brand: &str) -> Option<Identity> {
    let raw = cfg.auth.active.get(brand)?;
    let id = match raw.parse::<Selector>().ok()? {
        Selector::Exact(id) => id,
        Selector::Owner(_) => return None,
    };
    list(cfg, brand).contains(&id).then_some(id)
}

/// The reserved org whose membership IS the SuperAdmin predicate, server-side.
/// Named here ONLY to explain a refusal — never to decide one.
const ADMIN_ORG: &str = "admin";

/// Explain a server REFUSAL (403) in terms of the identity model — THE one place
/// any command turns an opaque 403 into the action that fixes it.
///
/// PURE, AND ONLY EVER AFTER THE FACT. This decides nothing and gates nothing:
/// a command calls it only once the SERVER has already refused. The server is
/// the SOLE grantor — it applies its gate (commerce's `MayMintMoney`: the
/// internal service token, or `IsSuperAdmin()` ⟺ this org) to the token IT
/// verified. Our `owner` comes from an unverified local decode that LABELS
/// STORAGE ONLY (`identity.rs`), so branching on it to decide whether to SEND
/// would invent an authorization decision out of a value its holder can forge —
/// and would refuse callers the server would have admitted (an `admin` token is
/// not the only mint principal). Reading it to EXPLAIN a refusal costs no
/// authority and closes the loop the deposit-403 incident opened.
///
/// Over VALUES, not places (the active identity + the ones we hold), so it is
/// reachable from any command and testable without a keychain: pair it with
/// [`active`] and [`list`]. `None` when there is nothing honest to say.
pub fn refusal_hint(active: &Identity, held: &[Identity]) -> Option<String> {
    // Already in the reserved org: the server refused a SuperAdmin, so switching
    // identity is not the remedy and claiming it would be a lie. Say nothing and
    // let the server's own reason stand alone.
    if active.owner == ADMIN_ORG {
        return None;
    }
    let admins: Vec<&Identity> = held.iter().filter(|i| i.owner == ADMIN_ORG).collect();
    let remedy = match admins.as_slice() {
        // Name only an identity we KNOW we hold — never a guessed `admin/<name>`
        // that `switch` would then reject.
        [one] => format!("You also hold {one} — switch to it and retry:\n\n      hanzo switch {one}"),
        // `switch` resolves a bare owner itself, and lists when it is ambiguous.
        // Never re-implement that here.
        [_, ..] => format!(
            "You hold several `{ADMIN_ORG}` identities — switch to one and retry:\n\n      hanzo switch {ADMIN_ORG}"
        ),
        [] => format!("You hold no `{ADMIN_ORG}` identity — sign in as one:\n\n      hanzo login"),
    };
    Some(format!(
        "\n  You are {active}; this needs the reserved `{ADMIN_ORG}` org (SuperAdmin).\n  {remedy}\n"
    ))
}

/// Render identities for a human, marking the active one.
pub fn render(cfg: &Config, brand: &str) -> String {
    let act = active(cfg, brand);
    list(cfg, brand)
        .iter()
        .map(|id| {
            let mark = if Some(id) == act.as_ref() { "*" } else { " " };
            format!("  {mark} {id}")
        })
        .collect::<Vec<_>>()
        .join("\n")
}

// ---- the vault-parameterised core (unit-testable; see `token::Vault`) -------

pub(crate) fn add_in(
    v: &dyn Vault,
    cfg: &mut Config,
    brand: &str,
    tokens: &TokenSet,
) -> Result<Identity> {
    oauth::server_url(brand)?; // reject unknown brands before touching the keychain
    let id = Identity::from_access_token(&tokens.access_token)?;
    token::store(v, brand, &id, tokens)?;
    cfg.update(|c| {
        index(c, brand, &id);
        set_active(c, brand, &id); // `hanzo login` IS the explicit user action
        Ok(())
    })?;
    // An explicit login to this brand supersedes any pre-multi-identity
    // credential filed under the bare brand: the user just re-authenticated, so
    // the old blob is dead weight. This is also the escape hatch that clears a
    // legacy entry whose claims cannot be read — `hanzo login` always works.
    v.remove(token::legacy_key(brand))?;
    Ok(id)
}

/// Set the active identity, verifying we actually HOLD its credential.
///
/// The keychain read happens OUTSIDE `cfg.update`, and MUST stay outside.
/// `update` holds an exclusive cross-process lock across its closure, while a
/// keyring read can block for as long as a human takes: the OS keychain
/// auto-locks on idle, and reading it then opens a GUI prompt and waits (the
/// `keyring` crate sets no timeout). Reading it under the lock would stall EVERY
/// other `hanzo` process that writes config — including a `hanzo code` whose
/// migration calls `update` — with no explanation and the prompt in another
/// window. Switching stays instant and never prompts while holding the lock.
///
/// Correctness does not need it in the transaction, because THE KEYCHAIN IS NOT
/// IN THE TRANSACTION: `token::store` and `token::delete` all run outside the
/// lock, so an in-lock read would be TOCTOU anyway — a concurrent `logout` can
/// delete the credential the instant this releases. The real guarantee is the
/// in-lock re-resolve below, which catches a concurrent change and fails closed.
/// Hoisting the KEYCHAIN READ preserves every SAFETY case; the resolve itself
/// still has to happen under the lock, which is why it happens twice.
pub(crate) fn switch_in(
    v: &dyn Vault,
    cfg: &mut Config,
    brand: &str,
    sel: Option<Selector>,
) -> Result<Identity> {
    oauth::server_url(brand)?; // reject unknown brands before touching anything
    let target = match &sel {
        Some(s) => resolve_selector(cfg, brand, s)?,
        None => toggle_target(cfg, brand)?,
    };
    // The index is only a pointer; the credential is the thing. Switching onto an
    // indexed-but-unheld identity would print a billing org we have no token for
    // and leave every later command saying "not signed in". Never advertise money
    // we cannot verify — fail closed instead.
    if token::load(v, brand, &target)?.is_none() {
        bail!(
            "{brand} identity {target} is indexed but its credential is not in the keychain \
             — run `hanzo login{}` to sign in as it again",
            brand_flag(brand)
        );
    }
    cfg.update(|c| {
        // Re-RESOLVE under the lock against fresh state, and refuse if it moved.
        //
        // A membership check alone would be equivalent to this only for
        // `Selector::Exact`. For a bare owner or the toggle, membership is not
        // the only precondition — UNAMBIGUITY is too, and a concurrent login can
        // change it: `hanzo switch hanzo` would silently pick one where a fresh
        // resolve refuses as ambiguous. Re-resolving turns that silent divergence
        // into a fail-closed refusal, subsumes the membership check, and restores
        // parity with `remove_in`, which already re-resolves under the lock. Both
        // are pure functions of the index, so this costs nothing.
        let fresh = match &sel {
            Some(s) => resolve_selector(c, brand, s)?,
            None => toggle_target(c, brand)?,
        };
        if fresh != target {
            bail!(
                "identities on {brand} changed while switching — verified {target}, now resolves \
                 to {fresh}. Nothing changed; re-run `hanzo switch`."
            );
        }
        set_active(c, brand, &fresh);
        Ok(())
    })?;
    Ok(target)
}

pub(crate) fn active_token_in(
    v: &dyn Vault,
    cfg: &mut Config,
    brand: &str,
) -> Result<Option<(Identity, TokenSet)>> {
    oauth::server_url(brand)?; // reject unknown brands before touching the keychain
    migrate_in(v, cfg, brand)?;

    let Some(id) = active(cfg, brand) else {
        return Ok(None);
    };
    // NO FALLBACK, NO CASCADE. A missing credential for the active identity
    // means unauthenticated — we never reach for another identity's token.
    let Some(tokens) = token::load(v, brand, &id)? else {
        return Ok(None);
    };
    // The slot must hold what it claims to hold. `add` files by DERIVED
    // identity, so this always holds; checking it means a hand-edited config or
    // a foreign keychain write cannot make us present the wrong principal.
    let claimed = Identity::from_access_token(&tokens.access_token)
        .context("identifying the stored credential")?;
    if claimed != id {
        bail!(
            "stored credential for {brand} identity {id} actually identifies as {claimed} — \
             refusing to use it; run `hanzo login{}`",
            brand_flag(brand)
        );
    }
    Ok(Some((id, tokens)))
}

/// Forwards-only migration of the pre-multi-identity credential (keyed by the
/// bare brand) into its identity slot. ONE SHOT: it re-files, indexes, and
/// DELETES the old key. There is no dual-read and no compat layer behind it.
fn migrate_in(v: &dyn Vault, cfg: &mut Config, brand: &str) -> Result<Option<Identity>> {
    let legacy = token::legacy_key(brand);
    let Some(json) = v.get(legacy)? else {
        return Ok(None);
    };
    let tokens: TokenSet =
        serde_json::from_str(&json).context("reading the credential stored by an older `hanzo`")?;
    let id = Identity::from_access_token(&tokens.access_token).with_context(|| {
        format!(
            "the credential stored by an older `hanzo` carries no identity — \
             run `hanzo login{}` to replace it (or `hanzo logout --all{}` to clear it)",
            brand_flag(brand),
            brand_flag(brand)
        )
    })?;

    // Order is the crash-safety argument: write the new slot BEFORE dropping the
    // old one, so an interrupted migration re-runs cleanly and never loses the
    // only copy of a credential.
    token::store(v, brand, &id, &tokens)?;
    cfg.update(|c| {
        index(c, brand, &id);
        // Carrying a prior login forward is not a switch. If an identity is
        // already active, migration must NOT steal the pointer — that would be
        // the auto-switch this module forbids.
        //
        // This check runs INSIDE the update, i.e. against fresh on-disk state
        // under the lock. That is what makes it correct under a race: a migration
        // that started before a concurrent `hanzo login` finished still sees that
        // login's pointer here and leaves it alone. Deciding on the caller's
        // stale snapshot would silently revert the user's explicit choice — on
        // the real fleet, demoting them off the identity they just picked.
        if active(c, brand).is_none() {
            set_active(c, brand, &id);
        }
        Ok(())
    })?;
    v.remove(legacy)?;
    Ok(Some(id))
}

pub(crate) fn remove_in(
    v: &dyn Vault,
    cfg: &mut Config,
    brand: &str,
    sel: Option<Selector>,
) -> Result<Identity> {
    oauth::server_url(brand)?; // reject unknown brands before touching the keychain
    // Resolve + de-index atomically against fresh state, THEN drop the secret.
    // Index-first is the crash-safe order: the index is the only reference, so an
    // interrupted logout leaves an unreferenced secret (harmless, and `login`
    // re-files it) rather than a pointer to a credential that is already gone.
    let target = cfg.update(|c| {
        let target = match &sel {
            Some(s) => resolve_selector(c, brand, s)?,
            None => active(c, brand).ok_or_else(|| {
                anyhow!(
                    "no active identity on {brand} — name one:\n{}\n\n  hanzo logout <owner/name>",
                    render(c, brand)
                )
            })?,
        };
        c.auth
            .identities
            .retain(|i| !(i.brand == brand && i.owner == target.owner && i.name == target.name));
        // The pointer must never dangle at a removed identity — and must never
        // slide onto a surviving one either. Signing out of the active identity
        // leaves you signed out, not silently signed in as somebody else.
        if c.auth.active.get(brand).map(String::as_str) == Some(target.to_string().as_str()) {
            c.auth.active.remove(brand);
        }
        Ok(target)
    })?;
    token::delete(v, brand, &target)?;
    Ok(target)
}

pub(crate) fn remove_all_in(v: &dyn Vault, cfg: &mut Config, brand: &str) -> Result<Vec<Identity>> {
    oauth::server_url(brand)?; // reject unknown brands before touching the keychain
    let ids = cfg.update(|c| {
        let ids = list(c, brand);
        c.auth.identities.retain(|i| i.brand != brand);
        c.auth.active.remove(brand);
        Ok(ids)
    })?;
    for id in &ids {
        token::delete(v, brand, id)?;
    }
    // Leave nothing addressable behind, including a pre-multi-identity blob.
    v.remove(token::legacy_key(brand))?;
    Ok(ids)
}

// ---- pure index + selection logic ------------------------------------------

/// Idempotent upsert — re-login updates in place, never duplicates a row.
fn index(cfg: &mut Config, brand: &str, id: &Identity) {
    let row = StoredIdentity {
        brand: brand.to_string(),
        owner: id.owner.clone(),
        name: id.name.clone(),
    };
    if !cfg.auth.identities.contains(&row) {
        cfg.auth.identities.push(row);
    }
}

fn set_active(cfg: &mut Config, brand: &str, id: &Identity) {
    cfg.auth.active.insert(brand.to_string(), id.to_string());
}

/// Resolve a user-supplied selector against the identities that ALREADY exist.
/// Selecting is not labeling: this can only ever return a stored identity.
fn resolve_selector(cfg: &Config, brand: &str, sel: &Selector) -> Result<Identity> {
    let ids = list(cfg, brand);
    if ids.is_empty() {
        bail!("not signed in to {brand} — run `hanzo login{}`", brand_flag(brand));
    }
    match sel {
        Selector::Exact(id) => {
            if ids.contains(id) {
                Ok(id.clone())
            } else {
                bail!(
                    "no {brand} identity {id}. Known:\n{}",
                    render(cfg, brand)
                )
            }
        }
        Selector::Owner(owner) => {
            let matches: Vec<&Identity> = ids.iter().filter(|i| &i.owner == owner).collect();
            match matches.as_slice() {
                [] => bail!(
                    "no {brand} identity in org {owner}. Known:\n{}",
                    render(cfg, brand)
                ),
                [one] => Ok((*one).clone()),
                many => bail!(
                    "{owner} is ambiguous — {} identities in that org. Name one:\n{}",
                    many.len(),
                    many.iter()
                        .map(|i| format!("  {i}"))
                        .collect::<Vec<_>>()
                        .join("\n")
                ),
            }
        }
    }
}

/// `hanzo switch` with no argument: toggle when the choice is unambiguous.
fn toggle_target(cfg: &Config, brand: &str) -> Result<Identity> {
    let ids = list(cfg, brand);
    match ids.len() {
        0 => bail!("not signed in to {brand} — run `hanzo login{}`", brand_flag(brand)),
        1 => Ok(ids[0].clone()),
        2 => {
            let cur = active(cfg, brand).ok_or_else(|| {
                anyhow!(
                    "no active identity on {brand} — name one:\n{}\n\n  hanzo switch <owner/name>",
                    render(cfg, brand)
                )
            })?;
            ids.into_iter()
                .find(|i| i != &cur)
                .ok_or_else(|| anyhow!("nothing to switch to on {brand}"))
        }
        n => bail!(
            "{n} identities on {brand} — name the one you want:\n{}\n\n  hanzo switch <owner/name>",
            render(cfg, brand)
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::iam::identity::testjwt::jwt;
    use crate::iam::token::memvault::MemVault;

    const ADMIN: &str = "admin/z";
    const ORG: &str = "hanzo/z";

    fn tokens(access: &str) -> TokenSet {
        TokenSet {
            access_token: access.to_string(),
            token_type: "Bearer".to_string(),
            refresh_token: None,
            id_token: None,
            expires_in: None,
            scope: None,
        }
    }

    /// A config that never writes to the real `~/.config/hanzo/config.toml`.
    fn cfg() -> Config {
        let mut c = Config::default();
        c.set_path_for_test(std::env::temp_dir().join(format!(
            "hanzo-store-test-{}-{:?}.toml",
            std::process::id(),
            std::thread::current().id()
        )));
        c
    }

    fn sel(s: &str) -> Option<Selector> {
        Some(s.parse::<Selector>().unwrap())
    }

    /// Sign in as both of z@hanzo.ai's provisioned identities.
    fn both(v: &MemVault, c: &mut Config) {
        add_in(v, c, "hanzo", &tokens(&jwt("admin", "z"))).unwrap();
        add_in(v, c, "hanzo", &tokens(&jwt("hanzo", "z"))).unwrap();
    }

    // ---- the incident ------------------------------------------------------

    /// THE regression: a second login must not clobber the first.
    #[test]
    fn two_identities_coexist_and_the_second_login_does_not_clobber_the_first() {
        let (v, mut c) = (MemVault::new(), cfg());
        both(&v, &mut c);

        assert_eq!(
            list(&c, "hanzo").iter().map(|i| i.to_string()).collect::<Vec<_>>(),
            vec![ADMIN, ORG]
        );
        // Both credentials are still addressable — the incident was that the
        // SuperAdmin token was unreachable from the CLI.
        assert!(v.has("hanzo/admin/z"));
        assert!(v.has("hanzo/hanzo/z"));
        // The most recent login is active.
        assert_eq!(active(&c, "hanzo").unwrap().to_string(), ORG);
    }

    /// Switching identity switches the BILLING org for free: `owner` is the
    /// billing key, so there is no billing selector to get out of sync.
    #[test]
    fn switch_flips_the_active_identity_and_therefore_the_billing_org() {
        let (v, mut c) = (MemVault::new(), cfg());
        both(&v, &mut c);
        assert_eq!(active(&c, "hanzo").unwrap().owner, "hanzo");

        switch_in(&v, &mut c, "hanzo", sel(ADMIN)).unwrap();

        let (id, tok) = active_token_in(&v, &mut c, "hanzo").unwrap().unwrap();
        assert_eq!(id.to_string(), ADMIN);
        // `owner == "admin"` is the SuperAdmin predicate the commerce gate reads
        // server-side — reachable from the CLI at last.
        assert_eq!(id.owner, "admin");
        assert_eq!(tok.access_token, jwt("admin", "z"));
    }

    /// EVERY consumer resolves through this one seam, so asserting the seam
    /// asserts all of them. The structural half of this claim (that no consumer
    /// bypasses it) is pinned by `no_consumer_bypasses_the_active_identity_seam`.
    #[test]
    fn every_consumer_follows_the_active_identity() {
        let (v, mut c) = (MemVault::new(), cfg());
        both(&v, &mut c);

        for want in [ADMIN, ORG, ADMIN] {
            switch_in(&v, &mut c, "hanzo", sel(want)).unwrap();
            // The `hanzo code` routing bearer, the wallet's cloud-custody bearer
            // and `whoami` all call exactly this.
            let (id, tok) = active_token_in(&v, &mut c, "hanzo").unwrap().unwrap();
            assert_eq!(id.to_string(), want);
            assert_eq!(tok.access_token, jwt(&id.owner, &id.name));
        }
    }

    // ---- explaining a refusal (the deposit-403 loop, closed) ---------------

    fn ident(s: &str) -> Identity {
        // Derived from claims, as everywhere else — there is no other way.
        let (owner, name) = s.split_once('/').unwrap();
        Identity::from_access_token(&jwt(owner, name)).unwrap()
    }

    /// THE PAYOFF: a 403 while acting as the org owner names the SuperAdmin
    /// identity we actually hold, and the ONE command that gets there.
    #[test]
    fn a_refusal_names_the_superadmin_identity_we_hold_and_the_switch() {
        let hint = refusal_hint(&ident(ORG), &[ident(ORG), ident(ADMIN)]).unwrap();
        assert!(hint.contains("You are hanzo/z"), "{hint}");
        assert!(hint.contains("admin"), "must name the reserved org: {hint}");
        assert!(hint.contains("hanzo switch admin/z"), "must be actionable: {hint}");
    }

    /// A SuperAdmin refused is NOT an identity problem: suggesting a switch to
    /// the org they are already in would be nonsense. Say nothing instead.
    #[test]
    fn a_refusal_of_a_superadmin_suggests_nothing() {
        assert!(refusal_hint(&ident(ADMIN), &[ident(ADMIN), ident(ORG)]).is_none());
    }

    /// We never suggest switching to an identity we do not hold — `switch` would
    /// only fail. Name the honest remedy instead.
    #[test]
    fn a_refusal_without_any_admin_identity_says_sign_in_not_switch() {
        let hint = refusal_hint(&ident(ORG), &[ident(ORG)]).unwrap();
        assert!(hint.contains("hold no `admin` identity"), "{hint}");
        assert!(hint.contains("hanzo login"), "{hint}");
        assert!(!hint.contains("hanzo switch"), "must not suggest an impossible switch: {hint}");
    }

    /// Several SuperAdmin identities: defer to `switch`'s own bare-owner
    /// resolution + ambiguity listing rather than re-implementing it here.
    #[test]
    fn a_refusal_with_several_admin_identities_defers_to_switch() {
        let hint = refusal_hint(&ident(ORG), &[ident(ADMIN), ident("admin/ops")]).unwrap();
        assert!(hint.contains("hanzo switch admin\n") || hint.contains("hanzo switch admin"), "{hint}");
        assert!(hint.contains("several"), "{hint}");
    }

    // ---- the hard invariant: active changes ONLY by explicit user action ----

    /// A missing credential for the active identity means UNAUTHENTICATED. It
    /// must never cascade onto another identity we happen to hold.
    #[test]
    fn a_missing_credential_never_falls_back_to_another_identity() {
        let (v, mut c) = (MemVault::new(), cfg());
        both(&v, &mut c);
        switch_in(&v, &mut c, "hanzo", sel(ADMIN)).unwrap();

        // The active identity's credential vanishes (revoked / keychain wiped).
        v.remove("hanzo/admin/z").unwrap();

        assert!(
            active_token_in(&v, &mut c, "hanzo").unwrap().is_none(),
            "must report NOT SIGNED IN, never fall back to hanzo/z"
        );
        // The pointer did not move on its own.
        assert_eq!(active(&c, "hanzo").unwrap().to_string(), ADMIN);
    }

    /// Reading a credential must never move the active pointer.
    #[test]
    fn resolving_a_credential_never_moves_the_active_pointer() {
        let (v, mut c) = (MemVault::new(), cfg());
        both(&v, &mut c);
        switch_in(&v, &mut c, "hanzo", sel(ADMIN)).unwrap();
        for _ in 0..3 {
            active_token_in(&v, &mut c, "hanzo").unwrap();
            assert_eq!(active(&c, "hanzo").unwrap().to_string(), ADMIN);
        }
    }

    /// Signing out of the ACTIVE identity leaves you signed OUT — not silently
    /// signed in as the identity that happens to remain.
    #[test]
    fn logout_of_the_active_identity_does_not_slide_onto_the_survivor() {
        let (v, mut c) = (MemVault::new(), cfg());
        both(&v, &mut c);
        switch_in(&v, &mut c, "hanzo", sel(ADMIN)).unwrap();

        remove_in(&v, &mut c, "hanzo", sel(ADMIN)).unwrap();

        assert!(active(&c, "hanzo").is_none(), "no auto-switch to the survivor");
        assert!(active_token_in(&v, &mut c, "hanzo").unwrap().is_none());
        // The survivor is untouched and still selectable.
        assert_eq!(
            list(&c, "hanzo").iter().map(|i| i.to_string()).collect::<Vec<_>>(),
            vec![ORG]
        );
        assert!(v.has("hanzo/hanzo/z"));
    }

    // ---- logout ------------------------------------------------------------

    /// `logout admin/z` leaves `hanzo/z` intact, and the pointer stays coherent.
    #[test]
    fn logout_one_identity_leaves_the_other_intact_and_active() {
        let (v, mut c) = (MemVault::new(), cfg());
        both(&v, &mut c); // hanzo/z ends up active
        assert_eq!(active(&c, "hanzo").unwrap().to_string(), ORG);

        remove_in(&v, &mut c, "hanzo", sel(ADMIN)).unwrap();

        // Removing a NON-active identity must not disturb the pointer.
        assert_eq!(active(&c, "hanzo").unwrap().to_string(), ORG);
        assert!(!v.has("hanzo/admin/z"), "keychain entry is gone");
        assert!(v.has("hanzo/hanzo/z"));
        let (id, _) = active_token_in(&v, &mut c, "hanzo").unwrap().unwrap();
        assert_eq!(id.to_string(), ORG);
    }

    #[test]
    fn logout_all_removes_every_identity_and_the_pointer() {
        let (v, mut c) = (MemVault::new(), cfg());
        both(&v, &mut c);

        let removed = remove_all_in(&v, &mut c, "hanzo").unwrap();

        assert_eq!(removed.len(), 2);
        assert!(list(&c, "hanzo").is_empty());
        assert!(active(&c, "hanzo").is_none());
        assert!(v.keys().is_empty(), "no credential left behind: {:?}", v.keys());
    }

    /// Brand isolation: logging out of one brand leaves another brand alone.
    #[test]
    fn logout_all_is_scoped_to_one_brand() {
        let (v, mut c) = (MemVault::new(), cfg());
        add_in(&v, &mut c, "hanzo", &tokens(&jwt("hanzo", "z"))).unwrap();
        add_in(&v, &mut c, "lux", &tokens(&jwt("lux", "z"))).unwrap();

        remove_all_in(&v, &mut c, "hanzo").unwrap();

        assert!(list(&c, "hanzo").is_empty());
        assert_eq!(list(&c, "lux").len(), 1);
        assert_eq!(active(&c, "lux").unwrap().to_string(), "lux/z");
    }

    // ---- re-login is an update, not a duplicate ----------------------------

    #[test]
    fn relogin_as_the_same_identity_updates_in_place() {
        let (v, mut c) = (MemVault::new(), cfg());
        both(&v, &mut c);
        switch_in(&v, &mut c, "hanzo", sel(ADMIN)).unwrap();

        // A fresh token for an identity we already hold (the ordinary case: the
        // old one expired). Same claims, different token material.
        let refreshed = TokenSet {
            refresh_token: Some("NEW_RT".to_string()),
            ..tokens(&jwt("admin", "z"))
        };
        add_in(&v, &mut c, "hanzo", &refreshed).unwrap();

        assert_eq!(c.auth.identities.len(), 2, "no duplicate index row");
        assert_eq!(list(&c, "hanzo").len(), 2);
        let (id, tok) = active_token_in(&v, &mut c, "hanzo").unwrap().unwrap();
        assert_eq!(id.to_string(), ADMIN);
        assert_eq!(tok.refresh_token.as_deref(), Some("NEW_RT"), "token replaced");
        assert_eq!(v.keys(), vec!["hanzo/admin/z", "hanzo/hanzo/z"]);
    }

    // ---- identity is derived, never supplied -------------------------------

    /// There is no API that files a token under a caller-chosen name: `add_in`
    /// takes only the token. A token claiming `hanzo/z` therefore CANNOT be made
    /// to occupy `admin/z`'s slot, which is what the commerce gate keys on.
    #[test]
    fn a_token_cannot_be_filed_under_another_principals_slot() {
        let (v, mut c) = (MemVault::new(), cfg());
        // The strongest available attempt at mislabeling: name the slot in the
        // config index by hand, then hand over a token for a different principal.
        c.auth.active.insert("hanzo".to_string(), ADMIN.to_string());

        let filed = add_in(&v, &mut c, "hanzo", &tokens(&jwt("hanzo", "z"))).unwrap();

        assert_eq!(filed.to_string(), ORG, "filed by its OWN claims");
        assert!(v.has("hanzo/hanzo/z"));
        assert!(!v.has("hanzo/admin/z"), "the admin slot was never written");
        let (id, _) = active_token_in(&v, &mut c, "hanzo").unwrap().unwrap();
        assert_eq!(id.to_string(), ORG);
    }

    /// A hand-edited config pointing at a slot cannot make us present the wrong
    /// principal: the slot's token self-identifies, and a mismatch fails closed.
    #[test]
    fn a_tampered_index_row_cannot_relabel_a_credential() {
        let (v, mut c) = (MemVault::new(), cfg());
        add_in(&v, &mut c, "hanzo", &tokens(&jwt("hanzo", "z"))).unwrap();

        // Forge the index: claim the org token is the SuperAdmin identity, and
        // plant it in the SuperAdmin slot.
        v.set("hanzo/admin/z", &serde_json::to_string(&tokens(&jwt("hanzo", "z"))).unwrap())
            .unwrap();
        c.auth.identities.push(StoredIdentity {
            brand: "hanzo".to_string(),
            owner: "admin".to_string(),
            name: "z".to_string(),
        });
        c.auth.active.insert("hanzo".to_string(), ADMIN.to_string());

        let err = active_token_in(&v, &mut c, "hanzo").unwrap_err().to_string();
        assert!(err.contains("identifies as"), "must fail closed: {err}");
    }

    /// A non-JWT bearer carries no identity and cannot be stored at all.
    #[test]
    fn a_token_without_claims_is_refused() {
        let (v, mut c) = (MemVault::new(), cfg());
        assert!(add_in(&v, &mut c, "hanzo", &tokens("hk-not-a-jwt")).is_err());
        assert!(v.keys().is_empty());
        assert!(c.auth.identities.is_empty());
    }

    // ---- migration: forwards-only, one shot --------------------------------

    /// The pre-multi-identity entry (keyed by the bare brand) re-files itself,
    /// gets indexed, becomes active, and the legacy key is GONE.
    #[test]
    fn legacy_entry_migrates_once_and_the_old_key_is_gone() {
        let (v, mut c) = (MemVault::new(), cfg());
        v.set("hanzo", &serde_json::to_string(&tokens(&jwt("hanzo", "z"))).unwrap())
            .unwrap();

        let (id, tok) = active_token_in(&v, &mut c, "hanzo").unwrap().unwrap();

        assert_eq!(id.to_string(), ORG);
        assert_eq!(tok.access_token, jwt("hanzo", "z"));
        assert_eq!(list(&c, "hanzo").iter().map(|i| i.to_string()).collect::<Vec<_>>(), vec![ORG]);
        assert_eq!(active(&c, "hanzo").unwrap().to_string(), ORG);
        // One shot: the legacy key is gone and only the identity slot remains.
        assert!(!v.has("hanzo"), "legacy bare-brand key must be deleted");
        assert_eq!(v.keys(), vec!["hanzo/hanzo/z"]);
    }

    /// Migration carries a prior login forward; it must never STEAL an active
    /// pointer the user chose (that would be an auto-switch).
    #[test]
    fn migration_never_steals_an_active_pointer() {
        let (v, mut c) = (MemVault::new(), cfg());
        both(&v, &mut c);
        switch_in(&v, &mut c, "hanzo", sel(ADMIN)).unwrap();
        // A stale legacy blob for the OTHER identity turns up.
        v.set("hanzo", &serde_json::to_string(&tokens(&jwt("hanzo", "z"))).unwrap())
            .unwrap();

        let (id, _) = active_token_in(&v, &mut c, "hanzo").unwrap().unwrap();

        assert_eq!(id.to_string(), ADMIN, "the user's choice stands");
        assert!(!v.has("hanzo"), "legacy key still consumed");
    }

    /// RED'S RACE, run for real against one config file on disk.
    ///
    /// Interleaving:
    ///   A: `hanzo code` starts migrating the legacy credential (in flight)
    ///   B: `hanzo login` explicitly sets active = hanzo/z, and saves
    ///   A: migration finishes
    ///
    /// A must NOT land the user on the legacy identity. On the real fleet the
    /// legacy key is the ORG owner, so the reachable direction is DEMOTION —
    /// silently reproducing the deposit-403 incident this branch exists to fix.
    /// The `active(c).is_none()` check runs inside `update`, against fresh state
    /// under the lock, so B's explicit choice wins.
    #[test]
    fn a_migration_in_flight_cannot_revert_a_concurrent_explicit_login() {
        let (v, mut a) = (MemVault::new(), cfg());
        // A and B are two `hanzo` processes over the SAME config file.
        let mut b = Config::load(Some(a.effective_path())).unwrap();

        // The pre-multi-identity credential A is about to migrate. A has ALREADY
        // read this blob — it is in flight — which is why B's login below is
        // staged as its two committed effects (credential + index) rather than
        // through `add_in`: a real `add_in` also consumes the legacy key, and
        // that consumption is precisely what has not reached A yet.
        v.set("hanzo", &serde_json::to_string(&tokens(&jwt("admin", "z"))).unwrap())
            .unwrap();

        // B logs in explicitly, and COMMITS, while A's migration is in flight.
        let org = Identity::from_access_token(&jwt("hanzo", "z")).unwrap();
        token::store(&v, "hanzo", &org, &tokens(&jwt("hanzo", "z"))).unwrap();
        b.update(|c| {
            index(c, "hanzo", &org);
            set_active(c, "hanzo", &org);
            Ok(())
        })
        .unwrap();
        assert_eq!(active(&b, "hanzo").unwrap().to_string(), ORG);

        // A finishes. `a` still holds its STALE pre-login snapshot.
        assert!(active(&a, "hanzo").is_none(), "A's snapshot predates B's login");
        let (id, _) = active_token_in(&v, &mut a, "hanzo").unwrap().unwrap();

        // B's explicit choice stands, in A's own view and on disk.
        assert_eq!(id.to_string(), ORG, "migration must not demote the user off their choice");
        let disk = Config::load(Some(a.effective_path())).unwrap();
        assert_eq!(active(&disk, "hanzo").unwrap().to_string(), ORG);
        // ...and B's index row was not erased by A's stale snapshot.
        assert_eq!(
            list(&disk, "hanzo").iter().map(|i| i.to_string()).collect::<Vec<_>>(),
            vec![ADMIN, ORG],
            "both identities survive; neither writer clobbered the other"
        );
        let _ = std::fs::remove_file(a.effective_path());
    }

    /// A vault that reports whether the config lock was HELD while it was read.
    ///
    /// This is the blind spot every other test here has: `MemVault` returns
    /// instantly, so a keychain read taken under the config lock looks identical
    /// to one taken outside it. The real keyring does not return instantly — it
    /// can open a GUI prompt and wait on a human — and a read under the lock
    /// stalls every other `hanzo` process that writes config.
    struct LockProbeVault<'a> {
        inner: &'a MemVault,
        lock_file: std::path::PathBuf,
        lock_was_held: std::cell::Cell<bool>,
    }

    impl<'a> LockProbeVault<'a> {
        fn new(inner: &'a MemVault, cfg: &Config) -> Self {
            Self {
                inner,
                lock_file: crate::config::lock_path(&cfg.effective_path()),
                lock_was_held: std::cell::Cell::new(false),
            }
        }

        /// Can an independent handle take the config lock right now? `flock` is
        /// held per open-file-description, so a separate `open` contends even
        /// inside one process — exactly as another `hanzo` process would.
        fn probe(&self) {
            let f = std::fs::OpenOptions::new()
                .create(true)
                .read(true)
                .write(true)
                .truncate(false)
                .open(&self.lock_file)
                .unwrap();
            match f.try_lock() {
                Ok(()) => {
                    let _ = f.unlock();
                }
                Err(_) => self.lock_was_held.set(true),
            }
        }
    }

    impl Vault for LockProbeVault<'_> {
        fn get(&self, key: &str) -> Result<Option<String>> {
            self.probe();
            self.inner.get(key)
        }
        fn set(&self, key: &str, value: &str) -> Result<()> {
            self.probe();
            self.inner.set(key, value)
        }
        fn remove(&self, key: &str) -> Result<bool> {
            self.probe();
            self.inner.remove(key)
        }
    }

    /// MED-4: the keychain must NEVER be touched while the config lock is held.
    ///
    /// A keyring read can block on a human (auto-locked collection → GUI prompt,
    /// no timeout). Under the lock that hangs every other `hanzo` process on the
    /// box. `switch` in particular promised to be instant and prompt-free.
    #[test]
    fn no_keychain_access_happens_while_the_config_lock_is_held() {
        let (v, mut c) = (MemVault::new(), cfg());
        both(&v, &mut c);

        let probe = LockProbeVault::new(&v, &c);
        switch_in(&probe, &mut c, "hanzo", sel(ADMIN)).unwrap();
        assert!(
            !probe.lock_was_held.get(),
            "switch read the keychain while holding the config lock — a keyring \
             prompt would stall every other `hanzo` process on the box"
        );
        assert_eq!(active(&c, "hanzo").unwrap().to_string(), ADMIN);

        // The same must hold for every other keychain-touching operation.
        let probe = LockProbeVault::new(&v, &c);
        add_in(&probe, &mut c, "hanzo", &tokens(&jwt("zoo", "z"))).unwrap();
        assert!(!probe.lock_was_held.get(), "login touched the keychain under the lock");

        let probe = LockProbeVault::new(&v, &c);
        remove_in(&probe, &mut c, "hanzo", sel("zoo/z")).unwrap();
        assert!(!probe.lock_was_held.get(), "logout touched the keychain under the lock");

        let probe = LockProbeVault::new(&v, &c);
        active_token_in(&probe, &mut c, "hanzo").unwrap();
        assert!(!probe.lock_was_held.get(), "resolve touched the keychain under the lock");

        let probe = LockProbeVault::new(&v, &c);
        remove_all_in(&probe, &mut c, "hanzo").unwrap();
        assert!(!probe.lock_was_held.get(), "logout --all touched the keychain under the lock");
        let _ = std::fs::remove_file(c.effective_path());
    }

    /// The migration path writes the keychain too — and it is the one on the
    /// `hanzo code` critical path, so a stall there is worst of all.
    #[test]
    fn migration_does_not_touch_the_keychain_under_the_lock() {
        let (v, mut c) = (MemVault::new(), cfg());
        v.set("hanzo", &serde_json::to_string(&tokens(&jwt("hanzo", "z"))).unwrap())
            .unwrap();

        let probe = LockProbeVault::new(&v, &c);
        let (id, _) = active_token_in(&probe, &mut c, "hanzo").unwrap().unwrap();

        assert_eq!(id.to_string(), ORG);
        assert!(
            !probe.lock_was_held.get(),
            "migration touched the keychain under the config lock"
        );
        let _ = std::fs::remove_file(c.effective_path());
    }

    /// LOW-1: never print a billing org for a credential we do not hold.
    #[test]
    fn switching_onto_an_indexed_but_unheld_identity_fails_closed() {
        let (v, mut c) = (MemVault::new(), cfg());
        both(&v, &mut c);
        switch_in(&v, &mut c, "hanzo", sel(ORG)).unwrap();

        // The credential goes away (revoked / keychain wiped) but the index row
        // remains — exactly the state that made `switch` lie about billing.
        v.remove("hanzo/admin/z").unwrap();

        let err = switch_in(&v, &mut c, "hanzo", sel(ADMIN)).unwrap_err().to_string();
        assert!(err.contains("not in the keychain"), "{err}");
        assert!(err.contains("hanzo login"), "must be actionable: {err}");
        // A refused switch changes nothing.
        assert_eq!(active(&c, "hanzo").unwrap().to_string(), ORG);
    }

    /// An unidentifiable legacy blob fails CLOSED with an actionable message —
    /// it is never silently dropped, and never used unlabeled.
    #[test]
    fn an_unidentifiable_legacy_entry_fails_closed_and_login_clears_it() {
        let (v, mut c) = (MemVault::new(), cfg());
        v.set("hanzo", &serde_json::to_string(&tokens("hk-legacy-key")).unwrap())
            .unwrap();

        let err = active_token_in(&v, &mut c, "hanzo").unwrap_err().to_string();
        assert!(err.contains("hanzo login"), "must be actionable: {err}");

        // The advertised escape hatch actually works.
        add_in(&v, &mut c, "hanzo", &tokens(&jwt("hanzo", "z"))).unwrap();
        assert!(!v.has("hanzo"), "login supersedes the legacy blob");
        let (id, _) = active_token_in(&v, &mut c, "hanzo").unwrap().unwrap();
        assert_eq!(id.to_string(), ORG);
    }

    // ---- selection ---------------------------------------------------------

    #[test]
    fn switch_resolves_a_bare_owner_when_unambiguous() {
        let (v, mut c) = (MemVault::new(), cfg());
        both(&v, &mut c);
        assert_eq!(switch_in(&v, &mut c, "hanzo", sel("admin")).unwrap().to_string(), ADMIN);
        assert_eq!(active(&c, "hanzo").unwrap().to_string(), ADMIN);
    }

    #[test]
    fn a_bare_owner_that_is_ambiguous_is_refused_and_lists() {
        let (v, mut c) = (MemVault::new(), cfg());
        add_in(&v, &mut c, "hanzo", &tokens(&jwt("hanzo", "z"))).unwrap();
        add_in(&v, &mut c, "hanzo", &tokens(&jwt("hanzo", "ops"))).unwrap();

        let err = switch_in(&v, &mut c, "hanzo", sel("hanzo")).unwrap_err().to_string();
        assert!(err.contains("ambiguous"), "{err}");
        assert!(err.contains("hanzo/ops") && err.contains("hanzo/z"), "{err}");
    }

    /// Bare `hanzo switch` with exactly two identities toggles.
    #[test]
    fn bare_switch_toggles_between_exactly_two() {
        let (v, mut c) = (MemVault::new(), cfg());
        both(&v, &mut c);
        assert_eq!(active(&c, "hanzo").unwrap().to_string(), ORG);
        assert_eq!(switch_in(&v, &mut c, "hanzo", None).unwrap().to_string(), ADMIN);
        assert_eq!(switch_in(&v, &mut c, "hanzo", None).unwrap().to_string(), ORG);
    }

    /// Bare `hanzo switch` with more than two is ambiguous: list, do not guess.
    #[test]
    fn bare_switch_with_more_than_two_lists_and_refuses() {
        let (v, mut c) = (MemVault::new(), cfg());
        both(&v, &mut c);
        add_in(&v, &mut c, "hanzo", &tokens(&jwt("zoo", "z"))).unwrap();

        let err = switch_in(&v, &mut c, "hanzo", None).unwrap_err().to_string();
        assert!(err.contains("3 identities"), "{err}");
        assert!(err.contains("hanzo switch <owner/name>"), "{err}");
        // The active identity is untouched by a refused switch.
        assert_eq!(active(&c, "hanzo").unwrap().to_string(), "zoo/z");
    }

    #[test]
    fn switching_to_an_unknown_identity_is_refused_and_changes_nothing() {
        let (v, mut c) = (MemVault::new(), cfg());
        both(&v, &mut c);
        assert!(switch_in(&v, &mut c, "hanzo", sel("nope/x")).is_err());
        assert!(switch_in(&v, &mut c, "hanzo", sel("nope")).is_err());
        assert_eq!(active(&c, "hanzo").unwrap().to_string(), ORG);
    }

    #[test]
    fn commands_refuse_an_unknown_brand_before_touching_the_keychain() {
        let (v, mut c) = (MemVault::new(), cfg());
        for r in [
            add_in(&v, &mut c, "bogus", &tokens(&jwt("hanzo", "z"))).map(|_| ()),
            active_token_in(&v, &mut c, "bogus").map(|_| ()),
            remove_in(&v, &mut c, "bogus", sel(ORG)).map(|_| ()),
            remove_all_in(&v, &mut c, "bogus").map(|_| ()),
            switch_in(&v, &mut c, "bogus", sel(ORG)).map(|_| ()),
        ] {
            assert!(r.is_err(), "unknown brand must be rejected");
        }
        assert!(v.keys().is_empty(), "keychain was touched: {:?}", v.keys());
    }

    #[test]
    fn nothing_is_signed_in_by_default() {
        let (v, mut c) = (MemVault::new(), cfg());
        assert!(active_token_in(&v, &mut c, "hanzo").unwrap().is_none());
        assert!(active(&c, "hanzo").is_none());
        assert!(list(&c, "hanzo").is_empty());
    }

    /// The structural half of "every consumer follows the active identity":
    /// no consumer may reach a credential except through this module's seam.
    ///
    /// This is the claim the behavioural tests cannot make on their own —
    /// `every_consumer_follows_the_active_identity` proves the seam is correct,
    /// and this proves nothing goes around it. The old bug was exactly a
    /// per-brand `token::load(brand)` at six call sites; if a seventh appears,
    /// or one regresses, this fails.
    #[test]
    fn no_consumer_bypasses_the_active_identity_seam() {
        // The consumers, verbatim (`include_str!` is compile-time — these paths
        // are checked by the compiler, so a moved file breaks the build loudly).
        for (name, src) in [
            ("commands/billing.rs", include_str!("../commands/billing.rs")),
            ("commands/code/mod.rs", include_str!("../commands/code/mod.rs")),
            ("commands/kms.rs", include_str!("../commands/kms.rs")),
            ("commands/product/mod.rs", include_str!("../commands/product/mod.rs")),
            ("commands/wallet.rs", include_str!("../commands/wallet.rs")),
            ("main.rs", include_str!("../main.rs")),
            ("iam/login.rs", include_str!("login.rs")),
            ("iam/onboarding.rs", include_str!("onboarding.rs")),
        ] {
            for banned in ["token::load", "token::store", "token::delete", "token::keyring"] {
                assert!(
                    !src.contains(banned),
                    "{name} calls {banned} directly — every consumer must resolve the ACTIVE \
                     identity via iam::store, or a second identity silently acts as the first"
                );
            }
        }
    }

    /// The index is METADATA. A token must never reach `config.toml`.
    #[test]
    fn the_config_index_never_holds_token_material() {
        let (v, mut c) = (MemVault::new(), cfg());
        both(&v, &mut c);
        let toml = toml::to_string_pretty(&c).unwrap();
        // The index IS written (identity metadata + the active pointer) ...
        assert!(toml.contains("[auth.active]"));
        assert!(toml.contains("[[auth.identities]]"));
        assert!(toml.contains("admin"));
        // ... but never any token material.
        assert!(!toml.contains(&jwt("admin", "z")));
        assert!(!toml.contains("access_token"));
        assert!(!toml.contains("eyJ"), "no JWT material in config: {toml}");
    }
}
