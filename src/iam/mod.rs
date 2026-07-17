//! Hanzo IAM client: HIP-0111 OIDC Authorization-Code-with-PKCE for the CLI.
//!
//! One concern per module:
//! - `paths`    — the canonical HIP-0111 endpoint URLs (no `/api/`, no legacy).
//! - `pkce`     — RFC 7636 verifier/challenge/state primitives.
//! - `identity` — WHO a token is, derived from its own claims.
//! - `token`    — token-set value type + OS-keychain persistence, per identity.
//! - `store`    — the identity store: keychain + config index, and THE one way
//!   any command resolves the ACTIVE identity's credential.
//! - `oauth`    — the interactive flow + userinfo (protocol mechanics, pure-ish).
//! - `login`    — the `login`/`whoami`/`switch`/`logout` entrypoints (UI + glue).

pub mod identity;
pub mod login;
pub mod oauth;
pub mod paths;
pub mod pkce;
pub mod store;
pub mod token;
