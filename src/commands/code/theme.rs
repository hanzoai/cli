//! Claude theme policy for `hanzo code claude`.
//!
//! Hanzo owns the POLICY — which theme, the default ("dracula", the vampire
//! look), the theme data — and drives tweakcc for the MECHANISM: tweakcc patches
//! Claude Code's native binary to register a custom theme (a treadmill we do NOT
//! reimplement — it tracks Claude's internals every release). We ship the theme
//! as data (`assets/themes/*.json`, tweakcc's theme schema) and apply it once per
//! (theme, install), then just set Claude's active theme on every launch.

use anyhow::Result;
use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;

/// The bundled Hanzo default theme (tweakcc theme schema). We own this data.
const DRACULA: &str = include_str!("../../../assets/themes/dracula.json");

/// The effective theme name for this run: an explicit `--theme` wins, else the
/// persisted `code.theme`. Trimmed + lowercased; empty or "none"/"off" disables.
pub fn effective(flag: Option<&str>, persisted: &str) -> Option<String> {
    let name = flag.unwrap_or(persisted).trim().to_lowercase();
    if name.is_empty() || name == "none" || name == "off" {
        return None;
    }
    Some(name)
}

/// Ensure Claude renders `name`, best-effort. Fast path: if it is already Claude's
/// active theme AND registered in the tweakcc binary patch, only the settings
/// touch runs. First time (or after a Claude update wipes the patch), it runs the
/// one-time `tweakcc --apply` to register the theme. NEVER blocks or fails the
/// session — a missing `npx`, no network, or a patch error degrades to a warning
/// and Claude launches on its own default.
pub fn ensure(name: &str) -> Result<()> {
    // Always make `name` Claude's selected theme (cheap; harmless if unregistered).
    set_active(name);

    // Only "dracula" is bundled today; another name is honored if the user already
    // registered it (we just selected it above), but we can't apply it ourselves.
    if name != "dracula" {
        return Ok(());
    }
    if applied_marker(name).exists() {
        return Ok(()); // already patched into this Claude install
    }
    if let Err(e) = apply_with_tweakcc(name) {
        super::warn(&format!(
            "theme: could not apply '{name}' via tweakcc ({e}); Claude will use its default. \
             Re-run once `npx` is available, or `--theme none` to silence."
        ));
    }
    Ok(())
}

/// Set Claude's active theme in `~/.claude/settings.json` (a shallow key set;
/// preserves every other setting). Best-effort — a missing/garbled file is skipped.
fn set_active(name: &str) {
    let Some(path) = claude_settings() else { return };
    let mut cfg: serde_json::Value = std::fs::read_to_string(&path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_else(|| serde_json::json!({}));
    if let Some(obj) = cfg.as_object_mut() {
        obj.insert("theme".into(), serde_json::Value::String(name.to_string()));
        if let Ok(s) = serde_json::to_string_pretty(&cfg) {
            let _ = std::fs::write(&path, s);
        }
    }
}

/// Register the bundled theme in tweakcc's config and patch the Claude binary.
/// Bounded so a hung `npx` can never stall a launch; touches a marker on success.
fn apply_with_tweakcc(name: &str) -> Result<()> {
    let theme: serde_json::Value = serde_json::from_str(DRACULA)?;
    let cfg_path = tweakcc_config();
    // Load or seed the tweakcc config, then upsert our theme by id (no dup).
    let mut cfg: serde_json::Value = std::fs::read_to_string(&cfg_path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_else(|| serde_json::json!({ "settings": { "themes": [] } }));
    let themes = cfg
        .pointer_mut("/settings/themes")
        .and_then(|v| v.as_array_mut());
    if let Some(themes) = themes {
        themes.retain(|t| t.get("id").and_then(|v| v.as_str()) != Some(name));
        themes.push(theme);
        if let Some(parent) = cfg_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        std::fs::write(&cfg_path, serde_json::to_string_pretty(&cfg)?)?;
    }
    // Patch the native binary with ONLY the `themes` patch. tweakcc's full
    // `--apply` runs every patch, and the non-theme patches can mis-anchor on a
    // Claude version tweakcc hasn't pinned yet (e.g. 2.1.211 vs its verified
    // 2.1.162) and corrupt the binary. `--patches themes` is the one we need and
    // is version-robust; it is idempotent and makes its own backup.
    let out = run_bounded(
        "npx",
        &["-y", "tweakcc@latest", "--apply", "--patches", "themes"],
        Duration::from_secs(300),
    )?;
    if !out {
        anyhow::bail!("tweakcc --apply did not complete");
    }
    let _ = std::fs::write(applied_marker(name), name);
    Ok(())
}

/// Run a command with a hard deadline; true iff it exited 0. Reaps on timeout.
fn run_bounded(program: &str, args: &[&str], deadline: Duration) -> Result<bool> {
    let mut child = Command::new(program)
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()?;
    let start = std::time::Instant::now();
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(status.success());
        }
        if start.elapsed() > deadline {
            let _ = child.kill();
            let _ = child.wait();
            anyhow::bail!("timed out");
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

fn home() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}
fn claude_settings() -> Option<PathBuf> {
    Some(home()?.join(".claude").join("settings.json"))
}
fn tweakcc_config() -> PathBuf {
    home()
        .map(|h| h.join(".tweakcc").join("config.json"))
        .unwrap_or_else(|| PathBuf::from(".tweakcc/config.json"))
}
fn applied_marker(name: &str) -> PathBuf {
    home()
        .map(|h| h.join(".tweakcc").join(format!(".hanzo-{name}-applied")))
        .unwrap_or_else(|| PathBuf::from(format!(".tweakcc/.hanzo-{name}-applied")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn effective_resolves_flag_over_persisted_and_disables_on_none() {
        assert_eq!(effective(Some("dracula"), "x").as_deref(), Some("dracula"));
        assert_eq!(effective(None, "dracula").as_deref(), Some("dracula"));
        assert_eq!(effective(Some("  Dracula "), "x").as_deref(), Some("dracula")); // trim+lower
        assert_eq!(effective(Some("none"), "dracula"), None);
        assert_eq!(effective(Some("off"), "dracula"), None);
        assert_eq!(effective(None, ""), None);
    }

    #[test]
    fn bundled_dracula_is_valid_tweakcc_theme() {
        let t: serde_json::Value = serde_json::from_str(DRACULA).unwrap();
        assert_eq!(t.get("id").and_then(|v| v.as_str()), Some("dracula"));
        assert!(t.get("colors").and_then(|v| v.as_object()).is_some_and(|c| !c.is_empty()));
    }
}
