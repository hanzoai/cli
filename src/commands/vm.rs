//! `hanzo vm <args…>` — the native microVM CLI (hanzoai/vm), run verbatim.
//!
//! One resolver, no reimplementation: `hanzo-vm` on PATH, else the place its
//! installer puts it (`~/.local/bin/hanzo-vm`) — and when neither exists, or the
//! found binary is older than [`VM_VERSION`], the CLI installs that pinned
//! release itself. `hanzo up` boots its k3s VM through the same
//! [`resolve_or_install`], so "where is the vm binary?" is answered in exactly
//! one place and a clean machine needs no separate install step.
//!
//! The pin is deliberate: the CLI controls which vm it spawns, never "latest at
//! runtime", so the same CLI build always boots the same vm. The release asset
//! is `hanzo-vm-v<VER>-<platform>.tar.gz` with a `.sha256` sidecar (the exact
//! names hanzoai/vm's install.sh and assets.rs use); the download is refused on
//! a digest mismatch. On macOS the fresh binary is ad-hoc signed with the
//! Virtualization.framework entitlement — unsigned, the kernel SIGKILLs it.

use crate::commands::launch;
use anyhow::{anyhow, bail, Context, Result};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

/// The hanzoai/vm release this CLI installs and spawns.
pub(crate) const VM_VERSION: &str = "2.0.0";

/// The Virtualization.framework entitlement (hanzoai/vm's `vm.entitlements`),
/// vendored so signing needs no second download.
#[cfg(target_os = "macos")]
const ENTITLEMENTS: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>com.apple.security.virtualization</key>
    <true/>
</dict>
</plist>
"#;

/// Locate an existing `hanzo-vm`: PATH first, then the installer's default.
fn resolve() -> Option<PathBuf> {
    which::which("hanzo-vm").ok().or_else(|| {
        let p = dirs::home_dir()?.join(".local/bin/hanzo-vm");
        p.is_file().then_some(p)
    })
}

/// Locate `hanzo-vm`, installing the pinned release when it is absent or older
/// than [`VM_VERSION`]. The ONE entry point — `hanzo vm` and `hanzo up` both
/// come through here, so a bare machine bootstraps instead of erroring.
pub(crate) async fn resolve_or_install() -> Result<PathBuf> {
    if let Some(bin) = resolve() {
        match binary_version(&bin) {
            Some(found) if !older(&found, VM_VERSION) => return Ok(bin),
            Some(found) => eprintln!(
                "hanzo-vm {found} at {} is older than the pinned v{VM_VERSION} — updating",
                bin.display()
            ),
            // `--version` failed: a broken install (on macOS typically an
            // unsigned binary the kernel kills). Reinstalling is the repair.
            None => eprintln!(
                "hanzo-vm at {} does not answer --version — reinstalling",
                bin.display()
            ),
        }
    }
    install().await
}

/// `hanzo vm <args…>` — exec the binary with the args verbatim. A passthrough is
/// transparent: the child owns the terminal and its exit is our exit, exactly
/// the [`launch`] contract.
pub async fn run(args: Vec<String>) -> Result<()> {
    let bin = resolve_or_install().await?;
    launch::exec(&bin, &args)
}

// ---- the bootstrap ------------------------------------------------------------

/// Download the pinned release, verify its sha256 sidecar, extract `hanzo-vm`
/// to `~/.local/bin`, and (macOS) sign it for Virtualization.framework.
async fn install() -> Result<PathBuf> {
    let platform = platform(std::env::consts::OS, std::env::consts::ARCH).ok_or_else(|| {
        anyhow!(
            "no hanzo-vm build for {}-{} — it supports macOS arm64 and Linux x86_64/aarch64",
            std::env::consts::OS,
            std::env::consts::ARCH
        )
    })?;
    let asset = format!("hanzo-vm-v{VM_VERSION}-{platform}.tar.gz");
    let url = format!("https://github.com/hanzoai/vm/releases/download/v{VM_VERSION}/{asset}");
    eprintln!("installing hanzo-vm v{VM_VERSION} ({platform}) → ~/.local/bin/hanzo-vm …");

    let http = reqwest::Client::new();
    let tarball = fetch(&http, &url).await?;
    let sidecar = String::from_utf8(fetch(&http, &format!("{url}.sha256")).await?)
        .context("the .sha256 sidecar is not utf-8")?;
    verify_sha256(&tarball, &sidecar, &asset)?;

    let dir = dirs::home_dir()
        .context("no home directory")?
        .join(".local/bin");
    std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    let bin = dir.join("hanzo-vm");
    extract(&tarball, &bin)?;
    #[cfg(target_os = "macos")]
    codesign(&bin)?;
    Ok(bin)
}

/// One GET, whole body, non-2xx is an error (a release asset either exists in
/// full or the install is off).
async fn fetch(http: &reqwest::Client, url: &str) -> Result<Vec<u8>> {
    let resp = http
        .get(url)
        .send()
        .await
        .with_context(|| format!("fetching {url}"))?;
    if !resp.status().is_success() {
        bail!("{url}: HTTP {}", resp.status());
    }
    Ok(resp
        .bytes()
        .await
        .with_context(|| format!("reading {url}"))?
        .to_vec())
}

/// Unpack the release tarball's `hanzo-vm` entry to `dest`, atomically (temp
/// file in the same directory, then rename) and executable.
fn extract(tar_gz: &[u8], dest: &Path) -> Result<()> {
    let mut archive = tar::Archive::new(flate2::read::GzDecoder::new(tar_gz));
    for entry in archive.entries().context("reading the release tarball")? {
        let mut entry = entry.context("reading a tarball entry")?;
        if entry.path().context("tarball entry path")?.file_name() != Some("hanzo-vm".as_ref()) {
            continue;
        }
        let tmp = dest.with_extension("tmp");
        entry
            .unpack(&tmp)
            .with_context(|| format!("unpacking to {}", tmp.display()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755))?;
        }
        std::fs::rename(&tmp, dest).with_context(|| format!("installing {}", dest.display()))?;
        return Ok(());
    }
    bail!("the release tarball has no hanzo-vm binary");
}

/// Ad-hoc sign with the virtualization entitlement. Without it the kernel
/// SIGKILLs the binary the moment it maps Virtualization.framework.
#[cfg(target_os = "macos")]
fn codesign(bin: &Path) -> Result<()> {
    let mut ent = tempfile::NamedTempFile::new().context("creating a temp entitlements file")?;
    std::io::Write::write_all(&mut ent, ENTITLEMENTS.as_bytes())?;
    let out = std::process::Command::new("codesign")
        .args(["--entitlements"])
        .arg(ent.path())
        .args(["--force", "-s", "-"])
        .arg(bin)
        .output()
        .context("running codesign")?;
    if !out.status.success() {
        bail!(
            "codesign failed on {} — hanzo-vm needs the virtualization entitlement to run:\n{}",
            bin.display(),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(())
}

// ---- pure helpers (unit-tested) -----------------------------------------------

/// The release platform string, exactly install.sh's spelling. `None` when no
/// build exists for the host.
fn platform(os: &str, arch: &str) -> Option<&'static str> {
    match (os, arch) {
        ("macos", "aarch64") => Some("darwin-aarch64"),
        ("linux", "x86_64") => Some("linux-x86_64"),
        ("linux", "aarch64") => Some("linux-aarch64"),
        _ => None,
    }
}

/// `hanzo-vm --version` → its semver, from the last token of the first line
/// (`hanzo-vm 2.0.0`). `None` when the binary will not run or prints no version.
fn binary_version(bin: &Path) -> Option<String> {
    let out = std::process::Command::new(bin)
        .arg("--version")
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let first = String::from_utf8_lossy(&out.stdout);
    let token = first.lines().next()?.split_whitespace().last()?;
    let v = token.trim_start_matches('v');
    parse_semver(v).map(|_| v.to_string())
}

fn parse_semver(v: &str) -> Option<(u64, u64, u64)> {
    let mut it = v.splitn(3, '.');
    let major = it.next()?.parse().ok()?;
    let minor = it.next()?.parse().ok()?;
    // Tolerate a suffix (`2.0.0-rc1`): the numeric prefix orders it.
    let patch = it
        .next()?
        .split(|c: char| !c.is_ascii_digit())
        .next()?
        .parse()
        .ok()?;
    Some((major, minor, patch))
}

/// Is `found` strictly older than `pin`? Unparseable input counts as older —
/// a version we cannot read is not one we trust to boot.
fn older(found: &str, pin: &str) -> bool {
    match (parse_semver(found), parse_semver(pin)) {
        (Some(f), Some(p)) => f < p,
        _ => true,
    }
}

/// Compare the tarball against its `.sha256` sidecar (`<hex>  <filename>`).
/// A mismatch refuses the install — never run what we cannot verify.
fn verify_sha256(bytes: &[u8], sidecar: &str, asset: &str) -> Result<()> {
    let want = sidecar
        .split_whitespace()
        .next()
        .with_context(|| format!("{asset}.sha256 is empty"))?
        .to_ascii_lowercase();
    let got = hex(&Sha256::digest(bytes));
    if got != want {
        bail!(
            "sha256 mismatch for {asset}: expected {want}, downloaded {got} — refusing to install"
        );
    }
    Ok(())
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn platform_strings_match_the_release_assets() {
        assert_eq!(platform("macos", "aarch64"), Some("darwin-aarch64"));
        assert_eq!(platform("linux", "x86_64"), Some("linux-x86_64"));
        assert_eq!(platform("linux", "aarch64"), Some("linux-aarch64"));
        assert_eq!(platform("macos", "x86_64"), None);
        assert_eq!(platform("windows", "x86_64"), None);
    }

    #[test]
    fn version_ordering_drives_the_update() {
        assert!(older("0.1.3", "2.0.0"));
        assert!(older("1.9.9", "2.0.0"));
        assert!(!older("2.0.0", "2.0.0"));
        assert!(!older("2.0.1", "2.0.0"));
        assert!(!older("10.0.0", "2.0.0"));
        // Unreadable is older: reinstall rather than trust it.
        assert!(older("garbage", "2.0.0"));
        assert!(older("", "2.0.0"));
        // A suffixed patch still orders by its numeric prefix.
        assert!(older("2.0.0-rc1", "2.0.1"));
    }

    #[test]
    fn sha256_sidecar_accepts_the_true_digest() {
        let body = b"the vm tarball";
        let sidecar = format!("{}  asset.tar.gz\n", hex(&Sha256::digest(body)));
        verify_sha256(body, &sidecar, "asset.tar.gz").unwrap();
    }

    #[test]
    fn sha256_sidecar_refuses_a_tampered_download() {
        let sidecar = format!(
            "{}  asset.tar.gz\n",
            hex(&Sha256::digest(b"the vm tarball"))
        );
        let err = verify_sha256(b"tampered bytes", &sidecar, "asset.tar.gz").unwrap_err();
        assert!(err.to_string().contains("sha256 mismatch"), "{err}");
    }

    /// A crafted tar.gz (no network): verify → extract → executable file lands.
    #[test]
    fn extract_installs_the_hanzo_vm_entry() {
        let mut tar = tar::Builder::new(Vec::new());
        let body = b"#!/bin/sh\necho hanzo-vm test\n";
        let mut hdr = tar::Header::new_gnu();
        hdr.set_size(body.len() as u64);
        hdr.set_mode(0o755);
        hdr.set_cksum();
        tar.append_data(&mut hdr, "hanzo-vm", &body[..]).unwrap();
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        std::io::Write::write_all(&mut gz, &tar.into_inner().unwrap()).unwrap();
        let tarball = gz.finish().unwrap();

        let sidecar = format!("{}  x.tar.gz", hex(&Sha256::digest(&tarball)));
        verify_sha256(&tarball, &sidecar, "x.tar.gz").unwrap();

        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("hanzo-vm");
        extract(&tarball, &dest).unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), body);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&dest).unwrap().permissions().mode() & 0o777,
                0o755
            );
        }
    }

    #[test]
    fn extract_refuses_a_tarball_without_the_binary() {
        let mut tar = tar::Builder::new(Vec::new());
        let mut hdr = tar::Header::new_gnu();
        hdr.set_size(2);
        hdr.set_mode(0o644);
        hdr.set_cksum();
        tar.append_data(&mut hdr, "README", &b"hi"[..]).unwrap();
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        std::io::Write::write_all(&mut gz, &tar.into_inner().unwrap()).unwrap();
        let tarball = gz.finish().unwrap();

        let dir = tempfile::tempdir().unwrap();
        let err = extract(&tarball, &dir.path().join("hanzo-vm")).unwrap_err();
        assert!(err.to_string().contains("no hanzo-vm binary"), "{err}");
    }
}
