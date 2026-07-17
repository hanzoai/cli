//! CLI configuration — the persisted state of the `hanzo` CLI.
//!
//! One file, one source of truth: `~/.config/hanzo/config.toml`. It holds only
//! NON-SECRET data — cloud endpoint, SDK paths, the selected network + any
//! custom networks, and wallet METADATA (address, custody, label). Secrets
//! (IAM tokens, local wallet keys) live in the OS keychain via `keyring`, never
//! here. See `iam::token` and `commands::wallet`.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use crate::private;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// The lock `update` takes, beside the config: `…/config.toml.lock`.
pub(crate) fn lock_path(path: &Path) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(".lock");
    path.with_file_name(name)
}

/// A cross-process exclusive lock guarding read-modify-write of the config.
///
/// The lock lives on a SEPARATE `config.toml.lock` file, never on `config.toml`
/// itself: the atomic write replaces the config's inode, so a lock held on the
/// old inode would guard nothing once the rename lands.
///
/// It is `std::fs::File::lock` — an advisory OS lock (`flock` on unix,
/// `LockFileEx` on Windows) that the KERNEL releases when the process exits. So a
/// killed or crashed `hanzo` can never wedge every other invocation behind a
/// stale lock, which is the failure mode a hand-rolled `O_EXCL` lockfile has.
struct Lock(std::fs::File);

impl Lock {
    fn acquire(path: &Path) -> Result<Self> {
        let lock = lock_path(path);
        let f = std::fs::OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(&lock)
            .with_context(|| format!("opening config lock {}", lock.display()))?;
        // Exclusive, and BLOCKING. Every writer stalls until this releases, so
        // the critical section MUST stay bounded: a parse, a write, an fsync and
        // a rename (the fsync is the real cost). Nothing that can
        // block on a human, a network or the OS keychain may run inside it: a
        // keyring read can open a GUI prompt and wait indefinitely, which would
        // hang every other `hanzo` process on the box with no explanation. See
        // `iam::store::switch_in`, which reads the keychain OUTSIDE this lock.
        f.lock()
            .with_context(|| format!("locking config {}", lock.display()))?;
        Ok(Self(f))
    }
}

impl Drop for Lock {
    fn drop(&mut self) {
        // Closing the handle would release it anyway; explicit is clearer.
        let _ = self.0.unlock();
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    /// Signed-in identities + the active one per brand. NEVER holds a token.
    #[serde(default)]
    pub auth: AuthState,

    /// Local SDK checkout paths. Defaulted like every other section so a sparse
    /// config (e.g. one that sets only `[code]`) still loads — the whole point of
    /// link-by-default is that a partial or absent config Just Works.
    #[serde(default)]
    pub sdk_paths: SdkPaths,

    /// Selected + custom networks. Mirrors the console network model.
    #[serde(default)]
    pub network: NetworkState,

    /// Wallet metadata + the active wallet. NEVER holds key material.
    #[serde(default)]
    pub wallet: WalletState,

    /// `hanzo code` defaults (the cloud-link setting; on by default).
    #[serde(default)]
    pub code: CodeState,

    /// Path this config was loaded from; where `save` writes back. Not persisted.
    #[serde(skip)]
    path: PathBuf,
}

/// The non-secret INDEX of signed-in identities. NEVER holds token material —
/// the tokens themselves are one-per-identity in the OS keychain (`iam::token`),
/// exactly as wallet key material is. This mirrors `WalletState`: the secret is
/// in the keychain, the metadata + the active pointer are here.
///
/// The index exists because the keychain has no portable enumeration API: it is
/// what lets `hanzo whoami --all` list identities offline, and what makes an
/// active identity a persisted, explicit choice rather than a guess.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct AuthState {
    /// The ACTIVE identity per brand: brand -> "owner/name". Changed ONLY by an
    /// explicit `hanzo login` / `hanzo switch` — never automatically, never as a
    /// fallback. See `iam::store`.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub active: BTreeMap<String, String>,
    /// Every identity signed in on this machine (metadata only — never secrets).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub identities: Vec<StoredIdentity>,
}

/// Non-secret identity metadata: which principal, on which brand. The token is
/// in the OS keychain under `{brand}/{owner}/{name}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredIdentity {
    /// Brand / tenant this identity was issued by (hanzo | lux | zoo | …).
    pub brand: String,
    /// The Casdoor org — ALSO the org the gateway bills and the SuperAdmin
    /// predicate (`owner == "admin"`). One value, three uses.
    pub owner: String,
    /// The Casdoor username.
    pub name: String,
}

/// Persisted defaults for `hanzo code`. NON-SECRET.
///
/// `link` streams a coding session to the user's OWN Hanzo cloud — the org is
/// derived server-side from the JWT `owner` claim, the CLI never sends one. It
/// is ON by default: a signed-in `hanzo code` links unless the user opts out.
/// Opt out per-invocation with `--no-link`, or persist the opt-out here with
/// `link = false`. The default only affects SIGNED-IN users: the privacy gate
/// in `commands::code::run` is structural and unchanged — an UNAUTHENTICATED run
/// holds no cloud client and streams nothing regardless of this default.
///
/// `#[serde(default)]` on the container fills a missing `link` from this
/// `Default` (true), so an existing config with no `[code]` table — or a
/// `[code]` table without `link` — still links; only an explicit `link = false`
/// persists the opt-out.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct CodeState {
    pub link: bool,
    /// The Claude theme `hanzo code claude` applies by default. "auto" honors the
    /// user's light/dark preference — Alucard (light Dracula) for a light theme,
    /// Dracula (dark) otherwise. Or name one ("dracula"/"alucard"); "none" disables.
    /// Overridden per-invocation with `--theme`. The user's own theme is restored
    /// when the session ends.
    pub theme: String,
}

impl Default for CodeState {
    fn default() -> Self {
        Self { link: true, theme: "auto".to_string() }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SdkPaths {
    pub python: PathBuf,
    pub go: PathBuf,
    pub rust: PathBuf,
    pub typescript: PathBuf,
}

/// The selected network + user-added custom/sovereign/local networks. Built-in
/// networks (mainnet/testnet/devnet/local) are defined in `commands::network`
/// and are NOT stored here — only overrides and the active selection are.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct NetworkState {
    /// Name of the active network (built-in or custom). None ⇒ the default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active: Option<String>,
    /// Custom networks added via `hanzo network add`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub custom: Vec<StoredNetwork>,
}

/// A network descriptor. For a sovereign L1, `network_id == chain_id`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredNetwork {
    /// Short selector, e.g. "mainnet" or "my-l1".
    pub name: String,
    /// Human label, e.g. "Hanzo Mainnet".
    pub label: String,
    /// Primary network ID (Lux/sovereign). Equals `chain_id` for a sovereign L1.
    pub network_id: u64,
    /// EVM chain ID.
    pub chain_id: u64,
    /// JSON-RPC (EVM) endpoint.
    pub rpc: String,
    /// Hanzo cloud/control API endpoint.
    pub api: String,
    /// Block explorer, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub explorer: Option<String>,
}

/// Wallet metadata + the active wallet address. Key material is in the keychain.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct WalletState {
    /// Address of the active wallet.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active: Option<String>,
    /// Known wallets (metadata only — never secrets).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub wallets: Vec<StoredWallet>,
}

/// Non-secret wallet metadata. The private key / mnemonic is NEVER stored here;
/// cloud-custody wallets keep it in KMS/MPC, local wallets in the OS keychain.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredWallet {
    /// EVM address (0x…), the wallet identity.
    pub address: String,
    /// Custody: "kms" | "mpc" (cloud) or "local" (OS keychain).
    pub custody: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// Cloud wallet id (custody=kms/mpc), for server-side sign/rotate.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// Network this wallet is scoped to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub network: Option<String>,
}

impl Config {
    /// The default config path: `${XDG_CONFIG_HOME}/hanzo/config.toml`.
    pub fn default_path() -> PathBuf {
        dirs::config_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("hanzo")
            .join("config.toml")
    }

    pub fn load(path: Option<PathBuf>) -> Result<Self> {
        let config_path = path.unwrap_or_else(Self::default_path);
        let mut cfg = if config_path.exists() {
            let content = std::fs::read_to_string(&config_path)
                .with_context(|| format!("reading config {}", config_path.display()))?;
            toml::from_str(&content)
                .with_context(|| format!("parsing config {}", config_path.display()))?
        } else {
            Self::default()
        };
        cfg.path = config_path;
        Ok(cfg)
    }

    /// The file this config reads/writes.
    pub(crate) fn effective_path(&self) -> PathBuf {
        if self.path.as_os_str().is_empty() {
            Self::default_path()
        } else {
            self.path.clone()
        }
    }

    /// Atomically read-modify-write the persisted config. THE one way to change
    /// it — there is deliberately no bare `save`.
    ///
    /// The config file is a shared mutable PLACE: several `hanzo` processes write
    /// it at once (a `hanzo code` migrating a legacy credential in one terminal
    /// while you run `hanzo login` in another). A load-mutate-save against a
    /// stale in-memory snapshot silently reverts the other process's write, and
    /// for the auth index that means landing on a principal you did not choose —
    /// the hard invariant broken by a write race rather than a cascade.
    ///
    /// So: take the cross-process lock, RE-READ current truth from disk, apply
    /// the mutation to THAT, write atomically (tmp+rename), release. `f` runs
    /// against the CURRENT on-disk state, never the caller's snapshot, so it must
    /// be a function of its inputs rather than of `self`'s prior contents — any
    /// un-persisted in-memory edit is intentionally discarded. `f` may fail, in
    /// which case nothing is written. On success `self` IS the persisted state.
    pub fn update<T>(&mut self, f: impl FnOnce(&mut Config) -> Result<T>) -> Result<T> {
        let path = self.effective_path();
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)
                .with_context(|| format!("creating config dir {}", dir.display()))?;
        }

        let _lock = Lock::acquire(&path)?;
        let mut fresh = Self::load(Some(path.clone()))?;
        let out = f(&mut fresh)?;
        fresh.write_atomic(&path)?;
        *self = fresh;
        Ok(out)
    }

    /// Write the whole config atomically, owner-only. See [`crate::private`] —
    /// the ONE way anything in this CLI writes a file it does not want torn or
    /// world-readable.
    fn write_atomic(&self, path: &Path) -> Result<()> {
        let toml = toml::to_string_pretty(self).context("serializing config")?;
        private::write(path, toml.as_bytes())
            .with_context(|| format!("writing config {}", path.display()))
    }

    /// Point this config at a throwaway file so a test that exercises `save`
    /// can never write to the developer's real `~/.config/hanzo/config.toml`
    /// (an empty `path` falls back to [`Config::default_path`]).
    #[cfg(test)]
    pub fn set_path_for_test(&mut self, path: PathBuf) {
        self.path = path;
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            auth: AuthState::default(),
            sdk_paths: SdkPaths::default(),
            network: NetworkState::default(),
            wallet: WalletState::default(),
            code: CodeState::default(),
            path: PathBuf::new(),
        }
    }
}

impl Default for SdkPaths {
    fn default() -> Self {
        let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
        Self {
            python: home.join("work/hanzo/sdk/src/py"),
            go: home.join("work/hanzo/sdk/src/go"),
            rust: home.join("work/hanzo/sdk/src/rs"),
            typescript: home.join("work/hanzo/sdk/src/js"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Link-by-default: a fresh config links a signed-in `hanzo code`.
    #[test]
    fn code_link_defaults_on() {
        assert!(CodeState::default().link);
        assert!(Config::default().code.link);
    }

    /// An existing config with no `[code]` table still links by default — the
    /// `code` field's `#[serde(default)]` fills it from `CodeState::default()`.
    #[test]
    fn absent_code_table_links_by_default() {
        let cfg: Config = toml::from_str(
            r#"
            [sdk_paths]
            python = "/p"
            go = "/g"
            rust = "/r"
            typescript = "/t"
            "#,
        )
        .expect("config parses");
        assert!(cfg.code.link);
    }

    /// A `[code]` table with no `link` key links by default — the container-level
    /// `#[serde(default)]` fills the missing field from `Default` (true).
    #[test]
    fn empty_code_table_links_by_default() {
        let code: CodeState = toml::from_str("").expect("empty table parses");
        assert!(code.link);
    }

    /// A sparse config that sets ONLY `[code]` (no `[sdk_paths]`) must still load
    /// — every non-secret section defaults. This is the exact shape a link-by-
    /// default rollout writes.
    #[test]
    fn config_with_only_code_section_loads() {
        let cfg: Config = toml::from_str("[code]\nlink = true\n").expect("sparse config loads");
        assert!(cfg.code.link);
        assert_eq!(cfg.sdk_paths.python, SdkPaths::default().python);
    }

    /// THE fleet-breaking regression guard: an existing config predating `[auth]`
    /// must load clean. A missing-field parse error here breaks EVERY command,
    /// not just auth — every field must stay serde-defaulted.
    #[test]
    fn a_config_with_no_auth_table_loads_signed_out() {
        let cfg: Config = toml::from_str("[code]\nlink = true\n").expect("config predating [auth] loads");
        assert!(cfg.auth.identities.is_empty());
        assert!(cfg.auth.active.is_empty());
    }

    /// A sparse `[auth]` table — present but empty, or carrying only one of its
    /// two fields — must also load. Both fields default independently.
    #[test]
    fn a_sparse_auth_table_loads() {
        let empty: AuthState = toml::from_str("").expect("empty [auth] parses");
        assert!(empty.identities.is_empty() && empty.active.is_empty());

        let cfg: Config = toml::from_str(
            r#"
            [auth]
            [auth.active]
            hanzo = "admin/z"
            "#,
        )
        .expect("[auth] with only `active` parses");
        assert_eq!(cfg.auth.active.get("hanzo").map(String::as_str), Some("admin/z"));
        assert!(cfg.auth.identities.is_empty());
    }

    /// The index round-trips through TOML. A signed-out config writes a bare
    /// empty `[auth]` table, exactly as `[network]`/`[wallet]` already do — and
    /// it must read back as "no identities", not as a parse error.
    #[test]
    fn auth_index_roundtrips() {
        let empty = toml::to_string_pretty(&Config::default()).unwrap();
        let back: Config = toml::from_str(&empty).expect("an empty [auth] table reloads");
        assert!(back.auth.identities.is_empty() && back.auth.active.is_empty());

        let mut cfg = Config::default();
        cfg.auth.identities.push(StoredIdentity {
            brand: "hanzo".to_string(),
            owner: "admin".to_string(),
            name: "z".to_string(),
        });
        cfg.auth.active.insert("hanzo".to_string(), "admin/z".to_string());

        let back: Config = toml::from_str(&toml::to_string_pretty(&cfg).unwrap()).expect("roundtrips");
        assert_eq!(back.auth.identities, cfg.auth.identities);
        assert_eq!(back.auth.active, cfg.auth.active);
    }

    fn tmp_cfg(tag: &str) -> Config {
        let mut c = Config::default();
        let p = std::env::temp_dir().join(format!(
            "hanzo-cfg-{tag}-{}-{:?}.toml",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_file(&p);
        let _ = std::fs::remove_file(lock_path(&p));
        c.set_path_for_test(p);
        c
    }

    /// A crash mid-write must never leave TRUNCATED TOML: `load` would then
    /// reject it and EVERY command would break, not just the one that crashed.
    /// `update` writes a temp file and renames, so a reader sees the old file or
    /// the new one — never a half-written one.
    #[test]
    fn a_write_is_atomic_and_never_leaves_a_torn_config() {
        let mut cfg = tmp_cfg("atomic");
        let path = cfg.effective_path();

        cfg.update(|c| {
            c.auth.active.insert("hanzo".to_string(), "hanzo/z".to_string());
            Ok(())
        })
        .unwrap();
        let good = std::fs::read_to_string(&path).unwrap();
        assert!(good.contains("hanzo/z"));

        cfg.update(|c| {
            c.auth.active.insert("lux".to_string(), "lux/z".to_string());
            Ok(())
        })
        .unwrap();

        // Both writes landed whole and the file still parses. (The temp file's
        // own lifecycle — reused, never orphaned — belongs to `private`, which
        // owns it; asserting it here would only re-test someone else's unit.)
        let back = Config::load(Some(path.clone())).expect("config still parses");
        assert_eq!(back.auth.active.get("hanzo").map(String::as_str), Some("hanzo/z"));
        assert_eq!(back.auth.active.get("lux").map(String::as_str), Some("lux/z"));

        let _ = std::fs::remove_file(&path);
    }

    /// A failing mutation must write NOTHING — `update` is a transaction.
    #[test]
    fn a_failed_update_leaves_the_config_untouched() {
        let mut cfg = tmp_cfg("failed");
        let path = cfg.effective_path();
        cfg.update(|c| {
            c.auth.active.insert("hanzo".to_string(), "hanzo/z".to_string());
            Ok(())
        })
        .unwrap();

        let err = cfg.update(|c| -> Result<()> {
            c.auth.active.insert("hanzo".to_string(), "admin/z".to_string());
            anyhow::bail!("mutation refused")
        });
        assert!(err.is_err());

        let back = Config::load(Some(path.clone())).unwrap();
        assert_eq!(
            back.auth.active.get("hanzo").map(String::as_str),
            Some("hanzo/z"),
            "a refused mutation must not be persisted"
        );
        let _ = std::fs::remove_file(&path);
    }

    /// `update` re-reads under the lock, so a mutation is applied to CURRENT
    /// truth rather than to the caller's snapshot — a stale in-memory copy can
    /// never revert another writer.
    #[test]
    fn update_applies_to_current_disk_state_not_a_stale_snapshot() {
        let mut a = tmp_cfg("stale");
        let path = a.effective_path();
        a.update(|c| {
            c.auth.active.insert("hanzo".to_string(), "hanzo/z".to_string());
            Ok(())
        })
        .unwrap();

        // `b` is a SECOND handle on the same file, holding a stale snapshot
        // (it never saw the write below).
        let mut b = Config::load(Some(path.clone())).unwrap();
        a.update(|c| {
            c.auth.identities.push(StoredIdentity {
                brand: "hanzo".to_string(),
                owner: "admin".to_string(),
                name: "z".to_string(),
            });
            Ok(())
        })
        .unwrap();

        // b mutates something else entirely. Its stale snapshot must NOT erase
        // a's row — the closure runs against fresh state.
        b.update(|c| {
            c.network.active = Some("local".to_string());
            Ok(())
        })
        .unwrap();

        let back = Config::load(Some(path.clone())).unwrap();
        assert_eq!(back.network.active.as_deref(), Some("local"));
        assert_eq!(back.auth.identities.len(), 1, "the other writer's row survived");
        let _ = std::fs::remove_file(&path);
    }

    /// THE race red proved, run for real against one file from many concurrent
    /// writers on separate handles.
    ///
    /// `flock` is held per open-file-description, so independent `File` handles
    /// contend even inside one process — each thread here is a faithful stand-in
    /// for a separate `hanzo` invocation. Every writer appends its own row; if
    /// the lock or the re-read were missing, writers would clobber each other and
    /// rows would be lost.
    #[test]
    fn concurrent_writers_do_not_lose_each_others_updates() {
        let base = tmp_cfg("race");
        let path = base.effective_path();
        drop(base);

        const WRITERS: usize = 8;
        std::thread::scope(|s| {
            for i in 0..WRITERS {
                let path = path.clone();
                s.spawn(move || {
                    let mut cfg = Config::load(Some(path)).unwrap();
                    cfg.update(|c| {
                        c.auth.identities.push(StoredIdentity {
                            brand: "hanzo".to_string(),
                            owner: format!("org{i}"),
                            name: "z".to_string(),
                        });
                        Ok(())
                    })
                    .unwrap();
                });
            }
        });

        let back = Config::load(Some(path.clone())).unwrap();
        assert_eq!(
            back.auth.identities.len(),
            WRITERS,
            "lost update: got {:?}",
            back.auth.identities
        );
        for i in 0..WRITERS {
            assert!(back.auth.identities.iter().any(|x| x.owner == format!("org{i}")));
        }
        let _ = std::fs::remove_file(&path);
    }

    /// `link = false` is the persisted opt-out and stays off.
    #[test]
    fn explicit_link_false_persists_the_opt_out() {
        let off: CodeState = toml::from_str("link = false").expect("parses");
        assert!(!off.link);
        let on: CodeState = toml::from_str("link = true").expect("parses");
        assert!(on.link);
    }
}
