//! Persistent user + organization context switching (`hanzo auth switch` / `hanzo switch`).
//!
//! `Config::default_org` persists the active Cloud organization context in
//! `~/.config/hanzo/config.toml` so subsequent commands send `X-Org-Id: <org>`
//! without typing `--as <org>` on every invocation.

use anyhow::{anyhow, bail, Result};
use colored::*;
use dialoguer::{theme::ColorfulTheme, Select};
use hanzo_gen::host::claim;

use crate::config::Config;

use super::identity::{self, Identity, Selector};
use super::oauth;
use super::onboarding;
use super::paths::DEFAULT_BRAND;
use super::store;
use super::token::TokenSet;

const SUPERADMIN: &str = "admin";

pub(crate) trait IdentitySeam {
    fn switch(&self, cfg: &mut Config, brand: &str, sel: Option<Selector>) -> Result<Identity>;
    fn token_for(&self, cfg: &Config, brand: &str, id: &Identity) -> Result<Option<TokenSet>>;
}

struct LiveStore;

impl IdentitySeam for LiveStore {
    fn switch(&self, cfg: &mut Config, brand: &str, sel: Option<Selector>) -> Result<Identity> {
        store::switch(cfg, brand, sel)
    }

    fn token_for(&self, cfg: &Config, brand: &str, id: &Identity) -> Result<Option<TokenSet>> {
        store::token_for(cfg, brand, id)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SwitchOutcome {
    pub identity: Identity,
    pub context_org: Option<String>,
    pub cleared_only: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PickOption {
    pub identity: Identity,
    pub org: Option<String>,
    pub label: String,
}

type Picker<'a> = &'a dyn Fn(&[PickOption], usize) -> Result<usize>;

fn brand_flag(brand: &str) -> String {
    if brand == DEFAULT_BRAND {
        String::new()
    } else {
        format!(" --brand {brand}")
    }
}

/// True when `cfg.org` comes from the saved `cfg.default_org` override and names
/// a Cloud tenant org in which no separate IAM identity is held locally.
///
/// Commands like `hanzo whoami`, `hanzo auth show`, `hanzo auth token`, and bare
/// `hanzo auth logout` keep speaking with the active identity's token in this
/// state, whereas an explicit `--as <org>` (or `admin`, the IAM SuperAdmin realm)
/// still enforces `store::within`.
pub(crate) fn is_saved_org_override(cfg: &Config, brand: &str) -> bool {
    let Some(org) = cfg.org.as_deref() else {
        return false;
    };
    if org == SUPERADMIN || cfg.default_org.as_deref() != Some(org) {
        return false;
    }
    !store::list(cfg, brand).iter().any(|i| i.owner == org)
}

/// Format the saved/active context org line for `hanzo auth show` / `hanzo auth list`,
/// when the effective org differs from `id.owner`.
pub(crate) fn context_org_note(cfg: &Config, id: &Identity) -> Option<String> {
    let org = cfg.org.as_deref().or(cfg.default_org.as_deref())?;
    if org == id.owner {
        return None;
    }
    let suffix = if cfg.default_org.as_deref() == Some(org) {
        "(saved; `hanzo auth switch --clear-org` to reset)"
    } else {
        "(for this invocation)"
    };
    Some(format!("{} {}", org.cyan(), suffix.dimmed()))
}

/// `hanzo auth switch` / `hanzo auth use` / `hanzo switch` entrypoint.
pub fn switch(
    cfg: &mut Config,
    brand: &str,
    target: Option<String>,
    org: Option<String>,
    clear_org: bool,
) -> Result<()> {
    let brand_resolved = store::realm(cfg, brand)?;
    let tty_picker = |opts: &[PickOption], default_idx: usize| -> Result<usize> {
        let labels: Vec<&str> = opts.iter().map(|o| o.label.as_str()).collect();
        Select::with_theme(&ColorfulTheme::default())
            .with_prompt("Switch active identity / organization context")
            .items(&labels)
            .default(default_idx)
            .interact()
            .map_err(Into::into)
    };
    let picker: Option<Picker<'_>> = onboarding::interactive().then_some(&tty_picker);
    let outcome = switch_in(
        &LiveStore,
        cfg,
        &brand_resolved,
        target,
        org,
        clear_org,
        picker,
    )?;
    print_outcome(&brand_resolved, &outcome);
    Ok(())
}

fn print_outcome(brand: &str, outcome: &SwitchOutcome) {
    if outcome.cleared_only {
        println!(
            "{} Active context on {}: {} — org reset to {}",
            "✓".green(),
            brand.cyan(),
            outcome.identity.to_string().bold(),
            outcome.identity.owner.cyan()
        );
        return;
    }
    match &outcome.context_org {
        Some(org) => println!(
            "{} Active context on {}: {} acting as org {} {}",
            "✓".green(),
            brand.cyan(),
            outcome.identity.to_string().bold(),
            org.cyan().bold(),
            "(saved for all subsequent commands)".dimmed()
        ),
        None => println!(
            "{} Active identity on {}: {} — billing to {}",
            "✓".green(),
            brand.cyan(),
            outcome.identity.to_string().bold(),
            outcome.identity.owner.cyan()
        ),
    }
}

fn require_active_held(seam: &dyn IdentitySeam, cfg: &Config, brand: &str) -> Result<Identity> {
    let ids = store::list(cfg, brand);
    if ids.is_empty() {
        bail!(
            "not signed in to {brand} — run `hanzo auth login{}`",
            brand_flag(brand)
        );
    }
    let act = store::active(cfg, brand).ok_or_else(|| {
        anyhow!(
            "no active identity on {brand} — select one first:\n{}\n\n  hanzo auth switch <owner/name>",
            store::render(cfg, brand)
        )
    })?;
    if seam.token_for(cfg, brand, &act)?.is_none() {
        bail!(
            "{brand} identity {act} is indexed but its credential is not in the keychain \
             — run `hanzo auth login{}` to sign in as it again",
            brand_flag(brand)
        );
    }
    Ok(act)
}

fn save_default_org(cfg: &mut Config, brand: &str, next: Option<String>) -> Result<()> {
    if brand == DEFAULT_BRAND {
        cfg.update(|c| {
            c.default_org = next.clone();
            Ok(())
        })?;
        cfg.org = next;
    }
    Ok(())
}

fn build_pick_options(
    seam: &dyn IdentitySeam,
    cfg: &Config,
    brand: &str,
) -> (Vec<PickOption>, usize) {
    let ids = store::list(cfg, brand);
    let act = store::active(cfg, brand);
    let cur_org = cfg.default_org.as_deref();
    let mut opts = Vec::new();
    let mut default_idx = 0;

    for id in &ids {
        if Some(id) == act.as_ref() && cur_org.is_none() {
            default_idx = opts.len();
        }
        opts.push(PickOption {
            identity: id.clone(),
            org: None,
            label: format!("{id}  (org: {})", id.owner),
        });

        let mut extra_orgs = seam
            .token_for(cfg, brand, id)
            .ok()
            .flatten()
            .map(|t| identity::orgs(&t.access_token))
            .unwrap_or_default();
        if Some(id) == act.as_ref() {
            if let Some(saved) = cur_org {
                let s = saved.to_string();
                if !extra_orgs.contains(&s) {
                    extra_orgs.push(s);
                }
            }
        }
        for org in extra_orgs {
            if org == id.owner || org == SUPERADMIN {
                continue;
            }
            if ids.iter().any(|other| other.owner == org) {
                continue;
            }
            if Some(id) == act.as_ref() && cur_org == Some(org.as_str()) {
                default_idx = opts.len();
            }
            opts.push(PickOption {
                identity: id.clone(),
                label: format!("{id}  acting as org {org}"),
                org: Some(org),
            });
        }
    }
    (opts, default_idx)
}

pub(crate) fn switch_in(
    seam: &dyn IdentitySeam,
    cfg: &mut Config,
    brand: &str,
    target: Option<String>,
    org: Option<String>,
    clear_org: bool,
    interactive_pick: Option<Picker<'_>>,
) -> Result<SwitchOutcome> {
    let brand: &str = &store::realm(cfg, brand)?;
    oauth::server_url(brand)?;

    if brand != DEFAULT_BRAND && (org.is_some() || clear_org) {
        bail!("`--org` and `--clear-org` apply to the default brand (`{DEFAULT_BRAND}`)");
    }

    if let Some(ref o) = org {
        claim("org", o)?;
        if o == SUPERADMIN && !store::list(cfg, brand).iter().any(|i| i.owner == SUPERADMIN) {
            let suffix = store::active(cfg, brand)
                .map(|id| format!(" (without --as you are {id})"))
                .unwrap_or_default();
            bail!(
                "you hold no {brand} identity in {SUPERADMIN}; `hanzo --as {SUPERADMIN} auth login{}` signs one in{suffix}",
                brand_flag(brand)
            );
        }
    }

    // 1. `hanzo auth switch --clear-org`
    if clear_org && target.is_none() {
        let act = require_active_held(seam, cfg, brand)?;
        save_default_org(cfg, brand, None)?;
        return Ok(SwitchOutcome {
            identity: act,
            context_org: None,
            cleared_only: true,
        });
    }

    // 2. `hanzo auth switch <TARGET> [--org <ORG>] [--clear-org]`
    if let Some(raw) = target {
        let sel = raw.parse::<Selector>()?;
        return match sel {
            Selector::Exact(id) => {
                let switched = seam.switch(cfg, brand, Some(Selector::Exact(id)))?;
                let next_org = if clear_org {
                    None
                } else {
                    org.filter(|o| o != &switched.owner)
                };
                save_default_org(cfg, brand, next_org.clone())?;
                Ok(SwitchOutcome {
                    identity: switched,
                    context_org: next_org,
                    cleared_only: false,
                })
            }
            Selector::Owner(owner_or_org) => {
                let holds_in_org = store::list(cfg, brand)
                    .iter()
                    .any(|i| i.owner == owner_or_org);
                if holds_in_org {
                    let switched =
                        seam.switch(cfg, brand, Some(Selector::Owner(owner_or_org)))?;
                    let next_org = if clear_org {
                        None
                    } else {
                        org.filter(|o| o != &switched.owner)
                    };
                    save_default_org(cfg, brand, next_org.clone())?;
                    Ok(SwitchOutcome {
                        identity: switched,
                        context_org: next_org,
                        cleared_only: false,
                    })
                } else {
                    if owner_or_org == SUPERADMIN {
                        let suffix = store::active(cfg, brand)
                            .map(|id| format!(" (without --as you are {id})"))
                            .unwrap_or_default();
                        bail!(
                            "you hold no {brand} identity in {SUPERADMIN}; `hanzo --as {SUPERADMIN} auth login{}` signs one in{suffix}",
                            brand_flag(brand)
                        );
                    }
                    if brand != DEFAULT_BRAND {
                        return seam
                            .switch(cfg, brand, Some(Selector::Owner(owner_or_org)))
                            .map(|id| SwitchOutcome {
                                identity: id,
                                context_org: None,
                                cleared_only: false,
                            });
                    }
                    let act = require_active_held(seam, cfg, brand)?;
                    let eff_org = if clear_org {
                        None
                    } else {
                        Some(org.unwrap_or(owner_or_org))
                    };
                    let next_org = eff_org.filter(|o| o != &act.owner);
                    save_default_org(cfg, brand, next_org.clone())?;
                    Ok(SwitchOutcome {
                        identity: act,
                        context_org: next_org,
                        cleared_only: false,
                    })
                }
            }
        };
    }

    // 3. `hanzo auth switch --org <ORG>` (keep active identity, persist org override)
    if let Some(o) = org {
        let act = require_active_held(seam, cfg, brand)?;
        let next_org = (o != act.owner).then_some(o);
        save_default_org(cfg, brand, next_org.clone())?;
        return Ok(SwitchOutcome {
            identity: act,
            context_org: next_org,
            cleared_only: false,
        });
    }

    // 4. Bare `hanzo auth switch` / `hanzo switch`
    if let Some(picker) = interactive_pick {
        let (opts, default_idx) = build_pick_options(seam, cfg, brand);
        if opts.len() >= 2 {
            let idx = picker(&opts, default_idx)?;
            let chosen = opts
                .get(idx)
                .ok_or_else(|| anyhow!("invalid selection index {idx}"))?
                .clone();
            let switched = seam.switch(cfg, brand, Some(Selector::Exact(chosen.identity)))?;
            let next_org = chosen.org.filter(|o| o != &switched.owner);
            save_default_org(cfg, brand, next_org.clone())?;
            return Ok(SwitchOutcome {
                identity: switched,
                context_org: next_org,
                cleared_only: false,
            });
        }
    }

    let switched = seam.switch(cfg, brand, None)?;
    save_default_org(cfg, brand, None)?;
    Ok(SwitchOutcome {
        identity: switched,
        context_org: None,
        cleared_only: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::iam::identity::testjwt::{claims_jwt, jwt};
    use crate::iam::token::memvault::MemVault;
    use crate::iam::token::{TokenSet, Vault};

    impl<V: Vault> IdentitySeam for V {
        fn switch(&self, cfg: &mut Config, brand: &str, sel: Option<Selector>) -> Result<Identity> {
            store::switch_in(self, cfg, brand, sel)
        }

        fn token_for(&self, cfg: &Config, brand: &str, id: &Identity) -> Result<Option<TokenSet>> {
            store::token_for_in(self, cfg, brand, id)
        }
    }

    fn tokens(access: &str) -> TokenSet {
        TokenSet {
            access_token: access.to_string(),
            refresh_token: Some("rt".into()),
            id_token: None,
            token_type: "Bearer".into(),
            expires_in: None,
            scope: None,
        }
    }

    fn test_cfg() -> (tempfile::TempDir, std::path::PathBuf, Config) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let mut c = Config::default();
        c.set_path_for_test(path.clone());
        (dir, path, c)
    }

    #[test]
    fn switch_org_persists_default_org_and_resets_cleanly() {
        let (_dir, path, mut c) = test_cfg();
        let v = MemVault::new();
        store::add_in(&v, &mut c, "hanzo", &tokens(&jwt("hanzo", "james"))).unwrap();

        // Switching to a Cloud org with no separate StoredIdentity persists default_org.
        let out = switch_in(
            &v,
            &mut c,
            "hanzo",
            Some("carbon-district".into()),
            None,
            false,
            None,
        )
        .unwrap();
        assert_eq!(out.identity.to_string(), "hanzo/james");
        assert_eq!(out.context_org.as_deref(), Some("carbon-district"));
        assert_eq!(c.default_org.as_deref(), Some("carbon-district"));
        assert_eq!(c.org.as_deref(), Some("carbon-district"));
        assert!(is_saved_org_override(&c, "hanzo"));

        // Reloading Config from disk populates default_org for subsequent commands!
        let reloaded = Config::load(Some(path.clone())).unwrap();
        assert_eq!(reloaded.default_org.as_deref(), Some("carbon-district"));

        // `hanzo auth switch hanzo/james` resets stale default_org cleanly.
        let out = switch_in(
            &v,
            &mut c,
            "hanzo",
            Some("hanzo/james".into()),
            None,
            false,
            None,
        )
        .unwrap();
        assert_eq!(out.context_org, None);
        assert_eq!(Config::load(Some(path.clone())).unwrap().default_org, None);

        // `hanzo auth switch hanzo/james --org acme` switches identity AND persists org.
        let out = switch_in(
            &v,
            &mut c,
            "hanzo",
            Some("hanzo/james".into()),
            Some("acme".into()),
            false,
            None,
        )
        .unwrap();
        assert_eq!(out.context_org.as_deref(), Some("acme"));
        assert_eq!(
            Config::load(Some(path.clone()))
                .unwrap()
                .default_org
                .as_deref(),
            Some("acme")
        );

        // `hanzo auth switch --clear-org` resets default_org back to None.
        let out = switch_in(&v, &mut c, "hanzo", None, None, true, None).unwrap();
        assert!(out.cleared_only);
        assert_eq!(out.context_org, None);
        assert_eq!(Config::load(Some(path)).unwrap().default_org, None);
    }

    #[test]
    fn switch_flag_only_and_superadmin_guard_and_interactive_picker() {
        let (_dir, path, mut c) = test_cfg();
        let v = MemVault::new();
        let tok = claims_jwt(
            r#"{"owner":"hanzo","name":"james","sub":"u-1","orgs":["hanzo","carbon-district","lux"]}"#,
        );
        store::add_in(&v, &mut c, "hanzo", &tokens(&tok)).unwrap();

        // `--org carbon-district` without positional target keeps identity and saves default_org.
        let out = switch_in(
            &v,
            &mut c,
            "hanzo",
            None,
            Some("carbon-district".into()),
            false,
            None,
        )
        .unwrap();
        assert_eq!(out.identity.to_string(), "hanzo/james");
        assert_eq!(out.context_org.as_deref(), Some("carbon-district"));
        assert_eq!(
            Config::load(Some(path.clone()))
                .unwrap()
                .default_org
                .as_deref(),
            Some("carbon-district")
        );

        // SuperAdmin (`admin`) without an `admin/*` identity is ALWAYS refused.
        let err = switch_in(&v, &mut c, "hanzo", Some("admin".into()), None, false, None)
            .unwrap_err()
            .to_string();
        assert!(err.contains("no hanzo identity in admin"), "{err}");

        // Interactive picker surfaces JWT `orgs` entries alongside the base identity.
        let pick_lux = |opts: &[PickOption], _default_idx: usize| -> Result<usize> {
            opts.iter()
                .position(|o| o.org.as_deref() == Some("lux"))
                .ok_or_else(|| anyhow!("lux option missing"))
        };
        let out = switch_in(&v, &mut c, "hanzo", None, None, false, Some(&pick_lux)).unwrap();
        assert_eq!(out.context_org.as_deref(), Some("lux"));
        assert_eq!(
            Config::load(Some(path.clone()))
                .unwrap()
                .default_org
                .as_deref(),
            Some("lux")
        );

        // Non-TTY bare switch with 2 identities toggles between them and clears stale default_org.
        store::add_in(&v, &mut c, "hanzo", &tokens(&jwt("hanzo", "ops"))).unwrap();
        let toggled = switch_in(&v, &mut c, "hanzo", None, None, false, None).unwrap();
        assert_eq!(toggled.identity.to_string(), "hanzo/james");
        assert_eq!(toggled.context_org, None);
        assert_eq!(Config::load(Some(path)).unwrap().default_org, None);
    }
}
