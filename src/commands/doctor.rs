//! Read-only installation checks; release verification downloads into a temporary directory.
use anyhow::{bail, Context, Result};
use hanzo_client::Method;
use sha2::{Digest, Sha256};
use std::{io::Read, path::Path, time::Duration};
use tokio::process::Command;

use crate::{commands::product::Seam, config::Config};

const TOOLS: &[(&str, &str, &str, &str)] = &[
    ("hanzo", "hanzo", "hanzoai/cli", ""),
    ("hanzo-dev", "dev", "hanzoai/dev", ""),
    (
        "hanzo-gen",
        "gen",
        "hanzoai/gen",
        ".github/workflows/release.yml",
    ),
    ("hanzo-mcp", "hanzo-mcp", "hanzoai/mcp", ""),
    ("hanzo-zet", "zt", "hanzozt/zt", ""),
];

async fn output(command: &mut Command, seconds: u64) -> Result<std::process::Output> {
    command.kill_on_drop(true);
    tokio::time::timeout(Duration::from_secs(seconds), command.output())
        .await
        .context("command timed out")?
        .context("could not execute command")
}

fn digest(path: &Path) -> Result<Vec<u8>> {
    let mut file = std::fs::File::open(path)?;
    let mut hash = Sha256::new();
    let mut bytes = [0; 65536];
    loop {
        let n = file.read(&mut bytes)?;
        if n == 0 {
            break;
        }
        hash.update(&bytes[..n]);
    }
    Ok(hash.finalize().to_vec())
}

async fn verify(path: &Path, binary: &str, repo: &str, signer: &str) -> Result<bool> {
    let dir = tempfile::tempdir()?;
    let script = dir.path().join("install.sh");
    std::fs::write(&script, include_str!("../../install.sh"))?;
    let bin = dir.path().join("bin");
    let result = output(
        Command::new("sh")
            .arg(&script)
            .env("HANZO_INSTALL_REPO", repo)
            .env("HANZO_INSTALL_BIN", binary)
            .env("HANZO_INSTALL_ALIAS", "")
            .env("HANZO_INSTALL_PREFIX", &bin)
            .env("HANZO_INSTALL_SIGNER", signer)
            .env("HANZO_INSTALL_STRICT", "1")
            .env_remove("HANZO_VERSION"),
        180,
    )
    .await?;
    if !result.status.success() {
        bail!("release provenance could not be verified; check GitHub CLI, login and release attestations with the strict installer");
    }
    let filename = format!("{binary}{}", std::env::consts::EXE_SUFFIX);
    Ok(digest(path)? == digest(&bin.join(filename))?)
}

pub async fn run(cfg: &mut Config, verify_releases: bool, offline: bool) -> Result<()> {
    println!("Hanzo installation diagnostics");
    let mut failures = 0;
    for &(name, legacy, repo, signer) in TOOLS {
        let path = which::which(name).or_else(|_| which::which(legacy));
        let Ok(path) = path else {
            println!("WARN {name}: not installed; run the Hanzo installer");
            continue;
        };
        match output(Command::new(&path).arg("--version"), 5).await {
            Ok(result) if result.status.success() && !result.stdout.is_empty() => {
                let text = String::from_utf8_lossy(&result.stdout);
                let version = text.lines().next().unwrap_or("no version output");
                println!("OK   {name}: {version} — {}", path.display());
            }
            _ => {
                println!("FAIL {name}: cannot run {} --version", path.display());
                failures += 1;
            }
        }
        if name != legacy {
            if let Ok(other) = which::which(legacy) {
                if std::fs::canonicalize(&other).ok() != std::fs::canonicalize(&path).ok() {
                    println!(
                        "WARN {legacy}: a separate executable at {}; check PATH precedence",
                        other.display()
                    );
                }
            }
        }
        if verify_releases {
            match verify(&path, legacy, repo, signer).await {
                Ok(true) => println!("OK   {name}: matches the checksummed, provenance-verified current release"),
                Ok(false) => println!("WARN {name}: differs from the current release; it may be an older version or a local build"),
                Err(err) => { println!("FAIL {name}: {err}"); failures += 1; }
            }
        }
    }
    if !verify_releases {
        println!("INFO Release integrity not checked; use hanzo doctor --verify (downloads current releases)");
    }
    if offline {
        println!("INFO Authentication not checked in offline mode");
    } else {
        let checked = tokio::time::timeout(Duration::from_secs(15), async {
            let seam = Seam::open(cfg, crate::iam::paths::DEFAULT_BRAND).await?;
            seam.send(Method::GET, "/v1/usage/summary", &[], None).await
        })
        .await;
        match checked {
            Ok(Ok(reply))
                if reply.status == 200
                    && reply
                        .body
                        .pointer("/scope/org")
                        .and_then(serde_json::Value::as_str)
                        .is_some_and(|org| !org.is_empty()) =>
            {
                println!("OK   Authentication: API accepted the active account")
            }
            Ok(Ok(reply)) => println!(
                "WARN Authentication: API returned {}; check hanzo auth show",
                reply.status
            ),
            Ok(Err(_)) => println!(
                "WARN Authentication unavailable; check hanzo auth show or hanzo auth login"
            ),
            Err(_) => println!("WARN Authentication check timed out"),
        }
    }
    if let Ok(program) = which::which("nvidia-smi") {
        match output(
            Command::new(program)
                .args(["--query-gpu=name,driver_version", "--format=csv,noheader"]),
            5,
        )
        .await
        {
            Ok(result) if result.status.success() => println!(
                "OK   NVIDIA GPU: {}",
                String::from_utf8_lossy(&result.stdout).trim()
            ),
            _ => println!(
                "WARN NVIDIA driver probe failed; cloud coding does not require a local GPU"
            ),
        }
    } else if which::which("rocminfo").is_ok() {
        match output(&mut Command::new("rocminfo"), 5).await {
            Ok(result) if result.status.success() => println!("OK   ROCm runtime responds"),
            _ => println!("WARN ROCm runtime probe failed"),
        }
    } else {
        println!("INFO No NVIDIA/ROCm runtime detected; local GPU capability is unverified and cloud coding remains available");
    }
    if failures != 0 {
        bail!("{failures} installation checks failed");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn integrity_comparison_reads_bytes_not_filenames() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a");
        let b = dir.path().join("b");
        std::fs::write(&a, b"release").unwrap();
        std::fs::write(&b, b"release").unwrap();
        assert_eq!(digest(&a).unwrap(), digest(&b).unwrap());
        std::fs::write(&b, b"changed").unwrap();
        assert_ne!(digest(&a).unwrap(), digest(&b).unwrap());
    }
}
