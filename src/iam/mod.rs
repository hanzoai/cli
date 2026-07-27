//! Hanzo IAM client: HIP-0111 OIDC Authorization-Code-with-PKCE for the CLI.
//!
//! One concern per module:
<<<<<<< HEAD
//! - `paths` — the canonical HIP-0111 endpoint URLs (no `/api/`, no legacy).
//! - `pkce`  — RFC 7636 verifier/challenge/state primitives.
//! - `token` — token-set value type + OS-keychain persistence.
//! - `oauth` — the interactive flow + userinfo (protocol mechanics, pure-ish).
//! - `login` — the `login`/`whoami`/`logout` command entrypoints (UI + glue).

pub mod login;
pub mod oauth;
pub mod paths;
pub mod pkce;
=======
//! - `paths`       — the canonical HIP-0111 endpoint URLs (no `/api/`, no legacy).
//! - `pkce`        — RFC 7636 verifier/challenge/state primitives.
//! - `identity`    — WHO a token is, derived from its own claims.
//! - `token`       — token-set value type + the portable `Vault` seam.
//! - `store`       — the identity store: keychain + config index, and THE one way
//!   any command resolves the ACTIVE identity's credential.
//! - `provider`    — provider (openai/anthropic/hanzo) API-key filing over the
//!   SAME `Vault`; the model-credential seam, disjoint from identity + wallet.
//! - `oauth`       — the interactive flow + userinfo (protocol mechanics, pure-ish).
//! - `login`       — the `login`/`whoami`/`switch`/`logout` entrypoints (UI + glue).
//! - `onboarding`  — the fresh-machine greeting + the multi-provider login picker.
//! - `secret`      — the ONE stdin-secret law (never argv); shared by onboarding
//!   (keys + identity tokens), the `kms` secret plane, and `connector add`
//!   (`resolve_token`: `--token -`/pipe/hidden-prompt, argv refused).

pub mod identity;
pub mod login;
pub mod oauth;
pub mod onboarding;
pub mod paths;
pub mod pkce;
pub mod provider;
pub mod secret;
pub mod store;
>>>>>>> 907d98a8
pub mod token;
