//! Claude theme policy for `hanzo code claude` — NATIVE, no binary patching.
//!
//! Claude Code loads custom themes from `~/.claude/themes/*.json` (its own
//! `loadCustomThemes`/`watchCustomThemes`) and selects one via the `theme` key in
//! `~/.claude/settings.json`. So hanzo owns the theme DATA (a bundled JSON in
//! Claude's native schema) and just drops the file + selects it. Nothing patches
//! the Claude binary — a wrong/absent theme is ignored, never a crash (unlike the
//! earlier tweakcc binary-patch approach, which corrupted Claude 2.1.211).
//!
//! Preset themes for every harness live in hanzoai/themes.

use anyhow::Result;
use std::path::PathBuf;

/// The bundled Hanzo default theme in Claude's NATIVE theme schema.
const DRACULA: &str = include_str!("../../../assets/themes/claude/dracula.json");

/// The effective theme name for this run: an explicit `--theme` wins, else the
/// persisted `code.theme`. Trimmed + lowercased; empty or "none"/"off" disables.
pub fn effective(flag: Option<&str>, persisted: &str) -> Option<String> {
    let name = flag.unwrap_or(persisted).trim().to_lowercase();
    if name.is_empty() || name == "none" || name == "off" {
        return None;
    }
    Some(name)
}

/// Make Claude render `name`, best-effort + safe. Writes the bundled theme into
/// `~/.claude/themes/<name>.json` (only "dracula" is bundled today; another name
/// is honored if the user already saved it) and selects it in settings. NEVER
/// blocks or fails the session, and CANNOT corrupt Claude — it only writes data
/// files Claude reads.
pub fn ensure(name: &str) -> Result<()> {
    if name == "dracula" {
        if let Some(dir) = themes_dir() {
            let _ = std::fs::create_dir_all(&dir);
            let _ = std::fs::write(dir.join("dracula.json"), DRACULA);
        }
    }
    set_active(name);
    Ok(())
}

/// Claude's built-in themes are selected by bare name; a custom theme (a file in
/// ~/.claude/themes) is selected by the `custom:<name>` ref (Claude's own scheme).
const BUILTIN: &[&str] = &[
    "dark",
    "light",
    "dark-daltonized",
    "light-daltonized",
    "dark-ansi",
    "light-ansi",
];

fn theme_ref(name: &str) -> String {
    if BUILTIN.contains(&name) {
        name.to_string()
    } else {
        format!("custom:{name}")
    }
}

/// Select `name` as Claude's active theme in `~/.claude/settings.json`, preserving
/// every other setting. A custom theme is written as `custom:<name>` (Claude's own
/// selection ref). Best-effort — a missing/garbled file is skipped.
fn set_active(name: &str) {
    let Some(path) = claude_settings() else { return };
    let mut cfg: serde_json::Value = std::fs::read_to_string(&path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_else(|| serde_json::json!({}));
    if let Some(obj) = cfg.as_object_mut() {
        obj.insert("theme".into(), serde_json::Value::String(theme_ref(name)));
        if let Ok(s) = serde_json::to_string_pretty(&cfg) {
            let _ = std::fs::write(&path, s);
        }
    }
}

fn home() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}
fn claude_settings() -> Option<PathBuf> {
    Some(home()?.join(".claude").join("settings.json"))
}
fn themes_dir() -> Option<PathBuf> {
    Some(home()?.join(".claude").join("themes"))
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
    fn bundled_dracula_is_valid_json_with_a_name() {
        let t: serde_json::Value = serde_json::from_str(DRACULA).unwrap();
        assert!(t.get("name").and_then(|v| v.as_str()).is_some());
    }
}
