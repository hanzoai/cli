//! Model providers a `hanzo code` session can authenticate against, and the
//! seam that files their API keys.
//!
//! A provider KEY is a secret, so it rides the SAME portable credential store as
//! IAM tokens and wallet keys (`token::vault`) — never the config, never argv,
//! never a log. This module is that store's provider-key FILING, exactly as
//! `store` is its identity filing and `commands::wallet` its key filing: three
//! disjoint key namespaces over one `Vault`, so nothing collides
//! (`provider/openai` vs `{brand}/{owner}/{name}` vs `wallet:0x…`).
//!
//! WHICH provider is active is non-secret INDEX data and lives in the config
//! (`auth.provider`), mirroring how the active identity and active wallet are
//! indexed there — the store has no portable enumeration API.

use anyhow::{bail, Result};

use super::token::{self, Vault};

/// A model provider the CLI can route a coding session through.
///
/// `Hanzo` is the gateway (api.hanzo.ai) — unified billing, every model, and the
/// path the session link needs; `OpenAI`/`Anthropic` are the model vendors' own
/// APIs, reached directly with the user's own key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Provider {
    Hanzo,
    OpenAI,
    Anthropic,
}

impl Provider {
    /// The stable slug used in the Vault slot and the `auth.provider` index.
    pub fn slug(self) -> &'static str {
        match self {
            Provider::Hanzo => "hanzo",
            Provider::OpenAI => "openai",
            Provider::Anthropic => "anthropic",
        }
    }

    /// A human label for prompts and confirmations.
    pub fn label(self) -> &'static str {
        match self {
            Provider::Hanzo => "Hanzo",
            Provider::OpenAI => "OpenAI",
            Provider::Anthropic => "Anthropic",
        }
    }

    /// Parse a provider selector (the `--provider` flag or the config index).
    /// Accepts the vendor's common alias so `--provider chatgpt|claude` work.
    pub fn parse(s: &str) -> Result<Provider> {
        match s.trim().to_ascii_lowercase().as_str() {
            "hanzo" => Ok(Provider::Hanzo),
            "openai" | "chatgpt" | "gpt" => Ok(Provider::OpenAI),
            "anthropic" | "claude" => Ok(Provider::Anthropic),
            other => bail!("unknown provider '{other}' (expected: hanzo | openai | anthropic)"),
        }
    }

    /// Detect the provider from an API key's prefix — the "paste a key" path:
    /// `hk-` is a Hanzo gateway key, `sk-ant-` an Anthropic key, and any other
    /// `sk-` an OpenAI key. `None` when no known prefix matches, so a mystery
    /// string is refused rather than filed under a guess.
    ///
    /// Order matters: `sk-ant-` is a more specific prefix than `sk-`, so it is
    /// tested first.
    pub fn detect(key: &str) -> Option<Provider> {
        let k = key.trim();
        if k.starts_with("hk-") {
            Some(Provider::Hanzo)
        } else if k.starts_with("sk-ant-") {
            Some(Provider::Anthropic)
        } else if k.starts_with("sk-") {
            Some(Provider::OpenAI)
        } else {
            None
        }
    }

    /// The Vault slot this provider's key is filed under. Disjoint from identity
    /// keys (`{brand}/{owner}/{name}`) and wallet keys (`wallet:0x…`).
    fn slot(self) -> String {
        format!("provider/{}", self.slug())
    }
}

/// Reject a key that could not be a clean single-line secret. A stored key is
/// later handed to a child process as an ENV VALUE (`ANTHROPIC_API_KEY` etc.), so
/// a newline or control char in it would be an injection vector and a whitespace
/// would silently corrupt the credential — refuse it at the boundary.
pub fn check_key(key: &str) -> Result<()> {
    if key.is_empty() {
        bail!("empty key");
    }
    if key.len() > 8192 {
        bail!("key is too long ({} > 8192 bytes)", key.len());
    }
    if let Some(bad) = key.chars().find(|c| c.is_whitespace() || c.is_control()) {
        bail!("key contains an unsupported character {bad:?} (a key is a single line with no whitespace)");
    }
    Ok(())
}

/// Store `key` for `provider` in the portable credential store.
pub fn set_key(provider: Provider, key: &str) -> Result<()> {
    set_key_in(&*token::vault()?, provider, key)
}

/// Load the stored key for `provider`, if any.
pub fn key(provider: Provider) -> Result<Option<String>> {
    key_in(&*token::vault()?, provider)
}

/// Remove `provider`'s stored key. Returns whether one existed.
pub fn clear(provider: Provider) -> Result<bool> {
    clear_in(&*token::vault()?, provider)
}

// ---- the vault-parameterised core (unit-testable; see `token::Vault`) -------

pub(crate) fn set_key_in(v: &dyn Vault, provider: Provider, key: &str) -> Result<()> {
    let key = key.trim();
    check_key(key)?;
    v.set(&provider.slot(), key)
}

pub(crate) fn key_in(v: &dyn Vault, provider: Provider) -> Result<Option<String>> {
    v.get(&provider.slot())
}

pub(crate) fn clear_in(v: &dyn Vault, provider: Provider) -> Result<bool> {
    v.remove(&provider.slot())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::iam::token::memvault::MemVault;

    #[test]
    fn detect_maps_prefixes_to_providers() {
        assert_eq!(Provider::detect("hk-abc123"), Some(Provider::Hanzo));
        assert_eq!(Provider::detect("sk-ant-api03-xyz"), Some(Provider::Anthropic));
        assert_eq!(Provider::detect("sk-proj-abc"), Some(Provider::OpenAI));
        assert_eq!(Provider::detect("sk-abc"), Some(Provider::OpenAI));
        // sk-ant- is tested BEFORE the looser sk- so Anthropic never reads as OpenAI.
        assert_eq!(Provider::detect("sk-ant-"), Some(Provider::Anthropic));
        // Leading/trailing space is tolerated (the paste path trims).
        assert_eq!(Provider::detect("  hk-abc  "), Some(Provider::Hanzo));
        // No known prefix -> refused, never filed under a guess.
        assert_eq!(Provider::detect("bogus"), None);
        assert_eq!(Provider::detect(""), None);
        assert_eq!(Provider::detect("ghp_github_token"), None);
    }

    #[test]
    fn parse_accepts_names_and_aliases() {
        assert_eq!(Provider::parse("hanzo").unwrap(), Provider::Hanzo);
        assert_eq!(Provider::parse("OpenAI").unwrap(), Provider::OpenAI);
        assert_eq!(Provider::parse("chatgpt").unwrap(), Provider::OpenAI);
        assert_eq!(Provider::parse("anthropic").unwrap(), Provider::Anthropic);
        assert_eq!(Provider::parse("claude").unwrap(), Provider::Anthropic);
        assert!(Provider::parse("gemini").is_err());
    }

    #[test]
    fn slug_and_slot_are_stable_and_disjoint() {
        assert_eq!(Provider::Hanzo.slug(), "hanzo");
        assert_eq!(Provider::OpenAI.slot(), "provider/openai");
        assert_eq!(Provider::Anthropic.slot(), "provider/anthropic");
        // A provider slot can never collide with an identity slot ({brand}/…) or a
        // wallet slot (wallet:…) — the namespaces are disjoint by prefix.
        assert!(Provider::Hanzo.slot().starts_with("provider/"));
    }

    #[test]
    fn check_key_rejects_unclean_secrets() {
        assert!(check_key("sk-ant-clean_KEY-123").is_ok());
        assert!(check_key("").is_err());
        assert!(check_key("has space").is_err());
        assert!(check_key("has\nnewline").is_err());
        assert!(check_key("has\ttab").is_err());
        assert!(check_key("ctrl\u{0}byte").is_err());
        assert!(check_key(&"x".repeat(9000)).is_err());
    }

    #[test]
    fn key_roundtrips_through_the_vault_seam() {
        let v = MemVault::new();
        assert!(key_in(&v, Provider::Anthropic).unwrap().is_none(), "absent key reads None");

        set_key_in(&v, Provider::Anthropic, "  sk-ant-secret  ").unwrap(); // trimmed on store
        assert_eq!(key_in(&v, Provider::Anthropic).unwrap().as_deref(), Some("sk-ant-secret"));
        // A different provider is a different slot — no clobber.
        set_key_in(&v, Provider::OpenAI, "sk-openai").unwrap();
        assert_eq!(key_in(&v, Provider::OpenAI).unwrap().as_deref(), Some("sk-openai"));
        assert_eq!(key_in(&v, Provider::Anthropic).unwrap().as_deref(), Some("sk-ant-secret"));

        assert_eq!(v.keys(), vec!["provider/anthropic", "provider/openai"]);

        assert!(clear_in(&v, Provider::Anthropic).unwrap());
        assert!(key_in(&v, Provider::Anthropic).unwrap().is_none());
        assert!(!clear_in(&v, Provider::Anthropic).unwrap(), "clearing what is gone is not an error");
        // Clearing one provider leaves the other intact.
        assert_eq!(key_in(&v, Provider::OpenAI).unwrap().as_deref(), Some("sk-openai"));
    }

    #[test]
    fn a_provider_key_never_collides_with_an_identity_or_wallet_slot() {
        let v = MemVault::new();
        // Identity-shaped and wallet-shaped keys coexist with provider keys.
        v.set("hanzo/admin/z", "IDENTITY").unwrap();
        v.set("wallet:0xabc", "WALLET").unwrap();
        set_key_in(&v, Provider::OpenAI, "sk-openai").unwrap();
        assert_eq!(v.get("hanzo/admin/z").unwrap().as_deref(), Some("IDENTITY"));
        assert_eq!(v.get("wallet:0xabc").unwrap().as_deref(), Some("WALLET"));
        assert_eq!(key_in(&v, Provider::OpenAI).unwrap().as_deref(), Some("sk-openai"));
    }
}
