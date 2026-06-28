//! Hanzo IAM client: HIP-0111 OIDC Authorization-Code-with-PKCE for the CLI.
//!
//! One concern per module:
//! - `paths` — the canonical HIP-0111 endpoint URLs (no `/api/`, no legacy).
//! - `pkce`  — RFC 7636 verifier/challenge/state primitives.
//! - `token` — token-set value type + OS-keychain persistence.
//! - `oauth` — the interactive flow + userinfo (protocol mechanics, pure-ish).
//! - `login` — the `login`/`whoami`/`logout` command entrypoints (UI + glue).

pub mod login;
pub mod oauth;
pub mod paths;
pub mod pkce;
pub mod token;
