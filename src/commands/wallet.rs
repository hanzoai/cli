//! `hanzo wallet` — the wallet identity for the CLI. One metadata model, two
//! custodies, ZERO plaintext secrets.
//!
//! - Cloud custody (`kms`/`mpc`): the PQ identity. Keys are derived + held
//!   server-side (luxfi/keys, `cloud/clients/wallets`, KMS/MPC) and NEVER leave
//!   it — the CLI only ever sees the address. This is the default when signed in.
//! - Local custody: an offline secp256k1 economic key for dev. The private key /
//!   mnemonic lives in the OS keychain (`keyring`), never on disk, never printed.
//!
//! Config stores only metadata (address, custody, network). Auto-provision: any
//! command needing a wallet calls `ensure` — if none is configured, one is
//! created for you (cloud when authed, else local).

use crate::commands::network;
use crate::config::{Config, StoredWallet};
use crate::iam::{paths, store};
use anyhow::{anyhow, bail, Context, Result};
use bip32::{DerivationPath, XPrv};
use bip39::{Language, Mnemonic, MnemonicType, Seed};
use colored::*;
use k256::ecdsa::{SigningKey, VerifyingKey};
use serde_json::{json, Value};
use sha3::{Digest, Keccak256};

/// Keychain service — the SAME namespace IAM tokens use (see `iam::token`).
const KEYCHAIN_SERVICE: &str = "ai.hanzo.cli";
/// The canonical EVM account derivation path (BIP-44, coin 60).
const EVM_PATH: &str = "m/44'/60'/0'/0/0";

// ---- OS keychain (secrets NEVER touch disk) ------------------------------

fn secret_entry(address: &str) -> Result<keyring::Entry> {
    keyring::Entry::new(KEYCHAIN_SERVICE, &format!("wallet:{address}"))
        .context("opening OS keychain entry")
}

fn store_secret(address: &str, secret: &str) -> Result<()> {
    secret_entry(address)?
        .set_password(secret)
        .context("writing wallet key to OS keychain")
}

/// True when key material for `address` is present in the keychain.
pub fn has_secret(address: &str) -> bool {
    secret_entry(address)
        .and_then(|e| e.get_password().map_err(Into::into))
        .is_ok()
}

// ---- EVM address derivation (local custody only) -------------------------

fn hexlower(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

fn decode_hex(s: &str) -> Result<Vec<u8>> {
    if !s.len().is_multiple_of(2) {
        bail!("odd-length hex");
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).context("invalid hex"))
        .collect()
}

/// EVM address (lowercase 0x…) = last 20 bytes of Keccak256(uncompressed pubkey).
fn address_from_vk(vk: &VerifyingKey) -> String {
    let point = vk.to_encoded_point(false); // 0x04 || X || Y (65 bytes)
    let hash = Keccak256::digest(&point.as_bytes()[1..]);
    format!("0x{}", hexlower(&hash[12..]))
}

/// Address from a raw secp256k1 private key.
fn address_from_privkey(hex32: &str) -> Result<String> {
    let bytes = decode_hex(hex32).context("private key must be 32-byte hex")?;
    let sk = SigningKey::from_slice(&bytes).context("invalid secp256k1 private key")?;
    Ok(address_from_vk(sk.verifying_key()))
}

/// Address from a BIP-39 mnemonic (any word count) at the canonical EVM path.
fn address_from_mnemonic(phrase: &str) -> Result<String> {
    let mnemonic = Mnemonic::from_phrase(phrase, Language::English)
        .map_err(|e| anyhow!("invalid mnemonic: {e}"))?;
    let seed = Seed::new(&mnemonic, "");
    let path: DerivationPath = EVM_PATH.parse().context("derivation path")?;
    let xprv = XPrv::derive_from_path(seed.as_bytes(), &path).context("HD derivation")?;
    Ok(address_from_vk(xprv.private_key().verifying_key()))
}

/// Normalize a user-supplied secret (mnemonic OR 0x-hex private key) to
/// `(address, secret-to-store)`. No secret is ever returned to the caller for
/// display — only for keychain storage.
fn derive_local(secret: &str) -> Result<(String, String)> {
    let s = secret.trim();
    let hexlike = s.trim_start_matches("0x");
    if hexlike.len() == 64 && hexlike.bytes().all(|b| b.is_ascii_hexdigit()) {
        let addr = address_from_privkey(hexlike)?;
        return Ok((addr, format!("0x{hexlike}")));
    }
    let addr = address_from_mnemonic(s)?;
    Ok((addr, s.to_string()))
}

// ---- config helpers ------------------------------------------------------

/// Insert-or-replace a wallet in the index. Pure over `cfg` so it can be
/// re-applied against fresh on-disk state inside a `Config::update`.
fn upsert(cfg: &mut Config, w: StoredWallet, set_active: bool) {
    let addr = w.address.clone();
    cfg.wallet.wallets.retain(|x| x.address != w.address);
    cfg.wallet.wallets.push(w);
    if set_active || cfg.wallet.active.is_none() {
        cfg.wallet.active = Some(addr);
    }
}

/// `upsert` committed atomically against current on-disk state.
fn upsert_saved(cfg: &mut Config, w: StoredWallet, set_active: bool) -> Result<()> {
    cfg.update(|c| {
        upsert(c, w.clone(), set_active);
        Ok(())
    })
}

/// The active wallet, if one is configured.
pub fn active(cfg: &Config) -> Option<StoredWallet> {
    let addr = cfg.wallet.active.as_deref()?;
    cfg.wallet.wallets.iter().find(|w| w.address == addr).cloned()
}

// ---- cloud custody (KMS/MPC, PQ) -----------------------------------------

/// Provision a cloud-custody wallet via `POST /v1/wallets` (KMS/MPC). Keys are
/// derived + held server-side and never returned — we persist only the address.
///
/// The wallet belongs to the ACTIVE identity's org: the CLI sends only the
/// bearer and cloud derives the org from its `owner` claim. Provisioning while
/// `admin/z` is active therefore creates an `admin`-org wallet, not a `hanzo`
/// one — which is precisely why the identity must be explicit and switchable.
async fn cloud_provision(cfg: &mut Config, name: &str, custody: &str) -> Result<StoredWallet> {
    let net = network::active(cfg);
    let api = net.api.trim_end_matches('/');
    let (_id, tok) = store::active_token(cfg, paths::DEFAULT_BRAND)?.ok_or_else(|| {
        anyhow!("not signed in — run `hanzo login` first (or `hanzo wallet create --local`)")
    })?;
    let client = reqwest::Client::new();

    // 1) Ensure an account exists (the wallet owner).
    let acct: Value = client
        .post(format!("{api}/v1/wallets/accounts"))
        .bearer_auth(&tok.access_token)
        .json(&json!({ "name": name }))
        .send()
        .await
        .context("POST /v1/wallets/accounts")?
        .json()
        .await
        .context("parse account response")?;
    let account_id = acct
        .get("id")
        .or_else(|| acct.get("accountId"))
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("no account id in response: {acct}"))?;

    // 2) Provision the wallet under that account.
    let resp = client
        .post(format!("{api}/v1/wallets"))
        .bearer_auth(&tok.access_token)
        .json(&json!({
            "accountId": account_id,
            "name": name,
            "custody": custody,
            // Cloud binds `Chain string` (clients/wallets/custody.go): a number
            // 400s and silently demotes cloud custody to a LOCAL key.
            "chain": net.chain_id.to_string(),
        }))
        .send()
        .await
        .context("POST /v1/wallets")?;
    let status = resp.status();
    let v: Value = resp.json().await.context("parse wallet response")?;
    if !status.is_success() {
        bail!("cloud wallet provision failed ({status}): {v}");
    }
    let address = v
        .get("address")
        .and_then(|a| a.as_str())
        .ok_or_else(|| anyhow!("no address in wallet response: {v}"))?
        .to_string();
    Ok(StoredWallet {
        address,
        custody: custody.to_string(),
        label: Some(name.to_string()),
        id: v.get("id").and_then(|s| s.as_str()).map(String::from),
        network: Some(net.name),
    })
}

// ---- command entrypoints -------------------------------------------------

/// `hanzo wallet create [--local] [--custody kms|mpc] [--name …]`
pub async fn create(
    cfg: &mut Config,
    name: Option<String>,
    local: bool,
    custody: String,
) -> Result<()> {
    let name = name.unwrap_or_else(|| "default".to_string());
    let net = network::active(cfg);

    if !local {
        // Prefer cloud custody (the PQ identity). Fall back to local if the
        // service is unreachable or we are not signed in.
        match cloud_provision(cfg, &name, &custody).await {
            Ok(w) => {
                let addr = w.address.clone();
                upsert_saved(cfg, w, true)?;
                println!("{} created {}-custody wallet {}", "✓".green(), custody, addr.cyan().bold());
                println!("  {} keys held server-side (KMS/MPC) — never on this machine", "PQ".dimmed());
                return Ok(());
            }
            Err(e) => {
                eprintln!("{} cloud custody unavailable ({e}); creating a LOCAL wallet instead", "!".yellow());
            }
        }
    }

    // Local custody: fresh 12-word mnemonic, secret to the OS keychain.
    let mnemonic = Mnemonic::new(MnemonicType::Words12, Language::English);
    let phrase = mnemonic.phrase().to_string();
    let address = address_from_mnemonic(&phrase)?;
    store_secret(&address, &phrase)?;
    upsert_saved(
        cfg,
        StoredWallet {
            address: address.clone(),
            custody: "local".into(),
            label: Some(name),
            id: None,
            network: Some(net.name),
        },
        true,
    )?;
    println!("{} created local wallet {}", "✓".green(), address.cyan().bold());
    println!("  {} mnemonic stored in the OS keychain — it is NOT printed and NOT on disk", "secret".dimmed());
    Ok(())
}

/// `hanzo wallet import <mnemonic|0xprivkey> [--name …]`
pub async fn import(cfg: &mut Config, secret: String, name: Option<String>) -> Result<()> {
    let (address, to_store) = derive_local(&secret)?;
    store_secret(&address, &to_store)?;
    let net = network::active(cfg);
    upsert_saved(
        cfg,
        StoredWallet {
            address: address.clone(),
            custody: "local".into(),
            label: name,
            id: None,
            network: Some(net.name),
        },
        true,
    )?;
    println!("{} imported wallet {}", "✓".green(), address.cyan().bold());
    println!("  {} key stored in the OS keychain (never on disk, never printed)", "secret".dimmed());
    Ok(())
}

/// `hanzo wallet show`
pub fn show(cfg: &Config) -> Result<()> {
    match active(cfg) {
        None => {
            println!("{}", "no active wallet — create one with `hanzo wallet create`".dimmed());
        }
        Some(w) => {
            println!("{}", w.address.cyan().bold());
            println!("  custody  {}", w.custody);
            if let Some(l) = &w.label {
                println!("  label    {l}");
            }
            if let Some(n) = &w.network {
                println!("  network  {n}");
            }
            let where_ = if w.custody == "local" {
                if has_secret(&w.address) {
                    "OS keychain"
                } else {
                    "MISSING from keychain"
                }
            } else {
                "cloud (KMS/MPC)"
            };
            println!("  key      {}", where_.dimmed());
        }
    }
    Ok(())
}

/// `hanzo wallet address` — just the active address (scriptable).
pub fn address(cfg: &Config) -> Result<()> {
    match active(cfg) {
        Some(w) => println!("{}", w.address),
        None => bail!("no active wallet — create one with `hanzo wallet create`"),
    }
    Ok(())
}

/// `hanzo wallet list`
pub fn list(cfg: &Config) -> Result<()> {
    if cfg.wallet.wallets.is_empty() {
        println!("{}", "no wallets — create one with `hanzo wallet create`".dimmed());
        return Ok(());
    }
    let active = cfg.wallet.active.clone().unwrap_or_default();
    for w in &cfg.wallet.wallets {
        let marker = if w.address == active {
            "*".green().bold()
        } else {
            " ".normal()
        };
        println!(
            "{} {} {} {}",
            marker,
            w.address.cyan(),
            format!("[{}]", w.custody).dimmed(),
            w.label.clone().unwrap_or_default()
        );
    }
    Ok(())
}

/// `hanzo wallet use <address>`
pub fn use_wallet(cfg: &mut Config, address: String) -> Result<()> {
    if !cfg.wallet.wallets.iter().any(|w| w.address == address) {
        bail!("unknown wallet {address}. Run `hanzo wallet list`.");
    }
    cfg.update(|c| {
        c.wallet.active = Some(address.clone());
        Ok(())
    })?;
    println!("{} active wallet {}", "✓".green(), address.cyan().bold());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    // Known 12-word BIP-44 vectors pin the derivation end-to-end (BIP-39 seed
    // -> BIP-32 m/44'/60'/0'/0/0 -> secp256k1 -> Keccak256 -> EVM address).
    #[test]
    fn mnemonic_derives_known_evm_address() {
        // Canonical BIP-39 all-zero-entropy vector.
        let reference =
            "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about";
        assert_eq!(
            address_from_mnemonic(reference).unwrap(),
            "0x9858effd232b4033e47d90003d41ec34ecaeda94"
        );
        // The hardhat / anvil default mnemonic, account 0.
        let hardhat = "test test test test test test test test test test test junk";
        assert_eq!(
            address_from_mnemonic(hardhat).unwrap(),
            "0xf39fd6e51aad88f6f4ce6ab8827279cfffb92266"
        );
    }

    // A raw private key derives its well-known address (hardhat account 0 key).
    #[test]
    fn privkey_derives_known_evm_address() {
        let pk = "0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80";
        let (addr, _) = derive_local(pk).unwrap();
        assert_eq!(addr, "0xf39fd6e51aad88f6f4ce6ab8827279cfffb92266");
    }
}
