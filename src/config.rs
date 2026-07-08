//! CLI configuration — the persisted state of the `hanzo` CLI.
//!
//! One file, one source of truth: `~/.config/hanzo/config.toml`. It holds only
//! NON-SECRET data — cloud endpoint, SDK paths, the selected network + any
//! custom networks, and wallet METADATA (address, custody, label). Secrets
//! (IAM tokens, local wallet keys) live in the OS keychain via `keyring`, never
//! here. See `iam::token` and `commands::wallet`.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub api_key: Option<String>,
    pub base_url: Option<String>,
    pub default_model: Option<String>,
    pub sdk_paths: SdkPaths,

    /// Selected + custom networks. Mirrors the console network model.
    #[serde(default)]
    pub network: NetworkState,

    /// Wallet metadata + the active wallet. NEVER holds key material.
    #[serde(default)]
    pub wallet: WalletState,

    /// Path this config was loaded from; where `save` writes back. Not persisted.
    #[serde(skip)]
    path: PathBuf,
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

    /// Persist the config back to its file, creating the parent directory.
    pub fn save(&self) -> Result<()> {
        let path = if self.path.as_os_str().is_empty() {
            Self::default_path()
        } else {
            self.path.clone()
        };
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)
                .with_context(|| format!("creating config dir {}", dir.display()))?;
        }
        let toml = toml::to_string_pretty(self).context("serializing config")?;
        std::fs::write(&path, toml).with_context(|| format!("writing config {}", path.display()))
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            api_key: None,
            base_url: Some("https://api.hanzo.ai".to_string()),
            default_model: Some("claude-3-opus".to_string()),
            sdk_paths: SdkPaths::default(),
            network: NetworkState::default(),
            wallet: WalletState::default(),
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
