//! `hanzo engine` & `hanzo pull` — pull, list, and serve local AI models on this machine.
//!
//! Model weights live in the standard Hugging Face Hub cache (`$HF_HOME/hub` or
//! `~/.cache/huggingface/hub`) so `hanzo-engine` (`hf-hub`), `vllm`, and `transformers`
//! all share one copy on disk with zero re-downloading.

use anyhow::{anyhow, bail, Context, Result};
use colored::*;
use std::io::Write;
use std::path::{Path, PathBuf};

use crate::commands::launch;

/// Resolve the engine binary. NOT `hanzo` on PATH — that is THIS CLI; the engine
/// ships as `hanzo-engine` (or point `HANZO_ENGINE_BIN` at a build of it).
fn engine_bin() -> Option<PathBuf> {
    launch::resolve("HANZO_ENGINE_BIN", &["hanzo-engine"])
}

/// Resolve friendly short names to canonical Hugging Face model IDs while leaving
/// any explicit `org/model` or local path untouched.
pub fn resolve_model_id(input: &str) -> &str {
    match input.trim() {
        "zen-nano" | "zen-nano-0.6b" => "zenlm/zen-nano-0.6b",
        "zen-embedding" | "zen-embedding-0.6b" => "zenlm/zen-embedding-0.6B-GGUF",
        "kai" | "kai-1" => "hanzoai/kai-1",
        "kai-agent" | "kai-1-agent" => "hanzoai/kai-1-agent",
        "zen5-coder" | "zen5-coder-32b" => "hanzoai/zen5-coder-32b",
        other => other,
    }
}

/// Locate the local Hugging Face Hub cache directory (`$HF_HUB_CACHE`, `$HF_HOME/hub`,
/// or `~/.cache/huggingface/hub`).
pub fn hf_hub_dir() -> PathBuf {
    if let Ok(d) = std::env::var("HF_HUB_CACHE") {
        if !d.trim().is_empty() {
            return PathBuf::from(d);
        }
    }
    if let Ok(d) = std::env::var("HF_HOME") {
        if !d.trim().is_empty() {
            return PathBuf::from(d).join("hub");
        }
    }
    dirs::home_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join(".cache")
        .join("huggingface")
        .join("hub")
}

/// Convert `org/repo` into `models--org--repo` inside the hub cache.
fn repo_cache_dir(hub_dir: &Path, repo_id: &str) -> PathBuf {
    let folder = format!("models--{}", repo_id.replace('/', "--"));
    hub_dir.join(folder)
}

/// Return the resolved snapshot path if `repo_id` is already present in `hub_dir`.
pub fn cached_snapshot(hub_dir: &Path, repo_id: &str) -> Option<PathBuf> {
    let base = repo_cache_dir(hub_dir, repo_id);
    if let Ok(rev) = std::fs::read_to_string(base.join("refs").join("main")) {
        let snap = base.join("snapshots").join(rev.trim());
        if snap.is_dir() {
            return Some(snap);
        }
    }
    let snaps = std::fs::read_dir(base.join("snapshots")).ok()?;
    for entry in snaps.flatten() {
        if entry.path().is_dir() {
            return Some(entry.path());
        }
    }
    None
}

/// Native pure-Rust Hugging Face Hub downloader when `hf` / `huggingface-cli` is
/// not installed on PATH. Writes into `models--<org>--<repo>/snapshots/<sha>/` +
/// `refs/main` so `hanzo-engine` (`hf-hub`) loads the cached snapshot offline.
async fn pull_native_hf(repo_id: &str, hub_dir: &Path) -> Result<PathBuf> {
    if !repo_id.contains('/') {
        bail!(
            "model `{repo_id}` must be `<org>/<model>` (for example `zenlm/zen-nano-0.6b`) or a known alias (`zen-nano`)"
        );
    }
    let api_base = std::env::var("HF_ENDPOINT").unwrap_or_else(|_| "https://huggingface.co".to_string());
    let api_base = api_base.trim_end_matches('/');
    let token = std::env::var("HF_TOKEN")
        .ok()
        .or_else(|| std::env::var("HUGGING_FACE_HUB_TOKEN").ok())
        .filter(|s| !s.trim().is_empty());

    let client = reqwest::Client::builder()
        .user_agent(format!("hanzo-cli/{}", env!("CARGO_PKG_VERSION")))
        .build()?;

    let meta_url = format!("{api_base}/api/models/{repo_id}");
    let mut req = client.get(&meta_url);
    if let Some(ref t) = token {
        req = req.bearer_auth(t);
    }
    let resp = req
        .send()
        .await
        .with_context(|| format!("fetching model metadata from {meta_url}"))?;
    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        bail!("model `{repo_id}` lookup failed ({status}): {body}");
    }
    let meta: serde_json::Value = resp.json().await.context("parsing Hugging Face model metadata")?;
    let sha = meta["sha"]
        .as_str()
        .filter(|s| !s.is_empty())
        .unwrap_or("main")
        .to_string();
    let siblings = meta["siblings"]
        .as_array()
        .ok_or_else(|| anyhow!("model `{repo_id}` metadata contained no file list (`siblings`)"))?;

    let base = repo_cache_dir(hub_dir, repo_id);
    let refs_dir = base.join("refs");
    let snap_dir = base.join("snapshots").join(&sha);
    std::fs::create_dir_all(&refs_dir)?;
    std::fs::create_dir_all(&snap_dir)?;

    let files: Vec<&str> = siblings
        .iter()
        .filter_map(|s| s["rfilename"].as_str())
        .filter(|f| !f.starts_with(".git"))
        .collect();

    if files.is_empty() {
        bail!("model `{repo_id}` lists no downloadable files");
    }

    for (idx, rfile) in files.iter().enumerate() {
        let dest = snap_dir.join(rfile);
        if dest.exists() && std::fs::metadata(&dest).map(|m| m.len() > 0).unwrap_or(false) {
            println!("  {} [{}/{}] {} (cached)", "•".green(), idx + 1, files.len(), rfile);
            continue;
        }
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let url = format!("{api_base}/{repo_id}/resolve/{sha}/{rfile}");
        print!("  {} [{}/{}] {} ...", "↓".cyan(), idx + 1, files.len(), rfile);
        let _ = std::io::stdout().flush();

        let mut dl = client.get(&url);
        if let Some(ref t) = token {
            dl = dl.bearer_auth(t);
        }
        let mut res = dl
            .send()
            .await
            .with_context(|| format!("downloading {url}"))?;
        if !res.status().is_success() {
            println!();
            bail!("failed to download `{rfile}` from {url}: HTTP {}", res.status());
        }

        let part = dest.with_extension("part");
        let mut out = std::fs::File::create(&part)
            .with_context(|| format!("creating {}", part.display()))?;
        let mut total: u64 = 0;
        while let Some(chunk) = res.chunk().await.with_context(|| format!("reading {rfile}"))? {
            out.write_all(&chunk)?;
            total += chunk.len() as u64;
        }
        out.flush()?;
        std::fs::rename(&part, &dest)?;
        println!(" {:.2} MiB", total as f64 / (1024.0 * 1024.0));
    }

    std::fs::write(refs_dir.join("main"), &sha)?;
    Ok(snap_dir)
}

/// `hanzo pull MODEL` / `hanzo engine pull MODEL` / `hanzo sandbox pull MODEL` —
/// download model weights to the local Hugging Face Hub cache (or target node).
pub async fn pull(model: &str, node: &str) -> Result<()> {
    let resolved = resolve_model_id(model);
    if resolved != model {
        println!(
            "{} Resolved `{}` → `{}`",
            "→".cyan(),
            model,
            resolved.cyan().bold()
        );
    }

    let hub_dir = hf_hub_dir();
    println!(
        "{} Pulling `{}` to {} (target: {})...",
        "→".cyan(),
        resolved.cyan().bold(),
        hub_dir.display(),
        node
    );

    let force_native = std::env::var("HANZO_PULL_NATIVE")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false);

    let used_cli = if !force_native {
        if let Ok(hf_bin) = which::which("hf") {
            let status = std::process::Command::new(hf_bin)
                .args(["download", resolved])
                .status()
                .context("running `hf download`")?;
            if !status.success() {
                bail!("`hf download {resolved}` exited with {status}");
            }
            true
        } else if let Ok(hf_cli) = which::which("huggingface-cli") {
            let status = std::process::Command::new(hf_cli)
                .args(["download", resolved])
                .status()
                .context("running `huggingface-cli download`")?;
            if !status.success() {
                bail!("`huggingface-cli download {resolved}` exited with {status}");
            }
            true
        } else {
            false
        }
    } else {
        false
    };

    let snap = if used_cli {
        cached_snapshot(&hub_dir, resolved)
            .unwrap_or_else(|| repo_cache_dir(&hub_dir, resolved))
    } else {
        pull_native_hf(resolved, &hub_dir).await?
    };

    println!(
        "{} Ready: `{}` ({})",
        "✓".green().bold(),
        resolved.bold(),
        snap.display()
    );
    println!(
        "  Serve locally on :1234:  {}",
        format!("hanzo engine serve {resolved}").cyan()
    );
    Ok(())
}

fn dir_bytes(path: &Path) -> u64 {
    let mut sum = 0u64;
    if let Ok(entries) = std::fs::read_dir(path) {
        for entry in entries.flatten() {
            let p = entry.path();
            if let Ok(meta) = std::fs::symlink_metadata(&p) {
                if meta.is_dir() {
                    sum += dir_bytes(&p);
                } else if meta.is_file() {
                    sum += meta.len();
                }
            }
        }
    }
    sum
}

/// `hanzo engine list` — list locally cached models in the Hugging Face Hub cache.
pub fn list() -> Result<()> {
    let hub_dir = hf_hub_dir();
    let mut rows: Vec<(String, u64, String)> = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&hub_dir) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if let Some(rest) = name.strip_prefix("models--") {
                let repo_id = rest.replacen("--", "/", 1);
                let path = entry.path();
                let bytes = dir_bytes(&path.join("blobs")).max(dir_bytes(&path.join("snapshots")));
                let rev = std::fs::read_to_string(path.join("refs").join("main"))
                    .map(|s| s.trim().chars().take(8).collect::<String>())
                    .unwrap_or_else(|_| "-".to_string());
                rows.push((repo_id, bytes, rev));
            }
        }
    }
    rows.sort_by(|a, b| a.0.cmp(&b.0));

    if rows.is_empty() {
        println!(
            "No local models cached in {}. Pull one with `hanzo pull zenlm/zen-nano-0.6b`.",
            hub_dir.display()
        );
        return Ok(());
    }

    println!("{:<44} {:>10}  {}", "MODEL", "SIZE", "REV");
    for (repo, bytes, rev) in rows {
        let size = if bytes >= 1024 * 1024 * 1024 {
            format!("{:.2} GiB", bytes as f64 / (1024.0 * 1024.0 * 1024.0))
        } else {
            format!("{:.1} MiB", bytes as f64 / (1024.0 * 1024.0))
        };
        println!("{:<44} {:>10}  {}", repo, size, rev);
    }
    Ok(())
}

/// `hanzo engine serve MODEL [--overlay FILE] [-- engine args…]`
pub async fn serve(model: String, overlay: Option<String>, mut passthrough: Vec<String>) -> Result<()> {
    let bin = engine_bin().ok_or_else(|| {
        anyhow!(
            "engine not found. Set HANZO_ENGINE_BIN=/path/to/hanzo-engine (from \
             github.com/hanzoai/engine), or put `hanzo-engine` on PATH (we do not build the engine \
             here — CI/CD does)."
        )
    })?;
    let resolved = resolve_model_id(&model).to_string();
    println!(
        "{} serving {} on a local /v1 chat-completions endpoint",
        "→".cyan(),
        resolved.cyan().bold()
    );
    if let Some(ref ov) = overlay {
        println!(
            "  {} applying .pleo overlay: {}",
            "•".green(),
            ov.bold()
        );
        passthrough.push("--overlay".to_string());
        passthrough.push(ov.clone());
    }
    let mut argv = vec!["serve".to_string(), "-m".to_string(), resolved];
    argv.extend(passthrough);
    launch::exec(&bin, &argv)
}

/// `hanzo engine up` — bring up the native bare-metal GPU engine & router
pub async fn up() -> Result<()> {
    println!("{}", "Bringing up native bare-metal GPU inference engine & router...".cyan().bold());
    let res = reqwest::get("http://127.0.0.1:1235/health").await;
    if res.is_err() {
        println!("  {} Starting hanzo-router (:1235)...", "→".cyan());
        let _ = std::process::Command::new("systemctl")
            .args(["--user", "restart", "hanzo-router.service"])
            .status();
    } else {
        println!("  {} hanzo-router is active (:1235)", "●".green());
    }

    let telem = crate::commands::monitor::collect_telemetry().await;
    crate::commands::monitor::render_dashboard(&telem);
    Ok(())
}

/// `hanzo engine down` — stop the native bare-metal GPU engine & router
pub async fn down() -> Result<()> {
    println!("{}", "Stopping native bare-metal GPU inference engine & router...".cyan().bold());
    let _ = std::process::Command::new("systemctl")
        .args(["--user", "stop", "hanzo-router.service"])
        .status();
    println!("  {} hanzo-router stopped", "●".yellow());
    Ok(())
}

/// `hanzo engine status` — non-interactive inference telemetry snapshot
pub async fn status() -> Result<()> {
    let telem = crate::commands::monitor::collect_telemetry().await;
    crate::commands::monitor::render_dashboard(&telem);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_friendly_aliases_and_preserves_explicit_repos() {
        assert_eq!(resolve_model_id("zen-nano"), "zenlm/zen-nano-0.6b");
        assert_eq!(resolve_model_id("zen-nano-0.6b"), "zenlm/zen-nano-0.6b");
        assert_eq!(resolve_model_id("kai-1"), "hanzoai/kai-1");
        assert_eq!(resolve_model_id("zen5-coder"), "hanzoai/zen5-coder-32b");
        assert_eq!(resolve_model_id("Qwen/Qwen3-8B"), "Qwen/Qwen3-8B");
    }

    #[test]
    fn finds_cached_snapshot_from_refs_main() {
        let dir = tempfile::tempdir().expect("tempdir");
        let model_dir = dir.path().join("models--zenlm--zen-nano-0.6b");
        let snap_dir = model_dir.join("snapshots").join("abc12345def");
        std::fs::create_dir_all(&snap_dir).unwrap();
        std::fs::create_dir_all(model_dir.join("refs")).unwrap();
        std::fs::write(model_dir.join("refs").join("main"), "abc12345def\n").unwrap();
        std::fs::write(snap_dir.join("config.json"), b"{}").unwrap();

        let found = cached_snapshot(dir.path(), "zenlm/zen-nano-0.6b").expect("snapshot exists");
        assert_eq!(found, snap_dir);
    }
}
