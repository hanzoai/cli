//! The Hanzo terminal page — the client `ttyd --index` serves.
//!
//! ttyd's own page is a fine terminal and the wrong surface for this product. It
//! cannot be themed to match the console that frames it, it has no touch
//! affordances, and — being somebody else's document inside a cross-origin frame
//! — it swallows every keystroke the workspace would like to hear. Owning the
//! page answers all three. ttyd itself is untouched; only `--index` changes.
//!
//! The page is assembled here rather than shipped as one file so the vendored
//! library and the part we wrote stay separate on disk, where they can be updated
//! and read independently. It is written to the data dir at link time because
//! `--index` takes a path, and rewritten every run so a stale copy from an older
//! release can never outlive its binary.

use anyhow::{Context, Result};
use std::path::PathBuf;

const INDEX: &str = include_str!("../../assets/term/index.html");
const XTERM_JS: &str = include_str!("../../assets/term/xterm.js");
const XTERM_CSS: &str = include_str!("../../assets/term/xterm.css");
const FIT_JS: &str = include_str!("../../assets/term/addon-fit.js");
const CLIENT_JS: &str = include_str!("../../assets/term/client.js");

/// The assembled page.
///
/// Everything is INLINE. ttyd serves exactly one document and no assets, so a
/// page that referenced `./xterm.js` would ask ttyd for a file it does not have
/// and render nothing — and inlining also means the terminal needs no network
/// beyond the socket it already holds.
pub fn page() -> String {
    INDEX
        .replace("/*__XTERM_CSS__*/", XTERM_CSS)
        .replace("/*__XTERM_JS__*/", XTERM_JS)
        .replace("/*__FIT_JS__*/", FIT_JS)
        .replace("/*__CLIENT_JS__*/", CLIENT_JS)
}

/// Write the page and hand back its path, for `ttyd --index`.
pub fn install() -> Result<PathBuf> {
    let dir = dirs::data_local_dir()
        .ok_or_else(|| anyhow::anyhow!("no data directory to write the terminal page into"))?
        .join("hanzo")
        .join("term");
    std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    let path = dir.join("index.html");
    // Rewritten every run: the page is a build artifact of THIS binary, and a
    // copy left by an older one would be served forever otherwise.
    crate::private::write(&path, page().as_bytes()).context("writing the terminal page")?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every placeholder is filled. A page that shipped with one intact would
    /// serve a comment where a library belongs and render a blank terminal.
    #[test]
    fn the_page_has_no_holes_left_in_it() {
        let p = page();
        for hole in ["__XTERM_CSS__", "__XTERM_JS__", "__FIT_JS__", "__CLIENT_JS__"] {
            assert!(!p.contains(hole), "{hole} was never substituted");
        }
    }

    /// The library is really inlined, not referenced — ttyd serves ONE document
    /// and no assets, so a `src="./xterm.js"` would ask it for a file it does not
    /// have and render nothing.
    #[test]
    fn nothing_is_fetched_from_anywhere() {
        let p = page();
        assert!(p.contains("Terminal"), "xterm itself must be in the page");
        assert!(p.contains("FitAddon"), "and the fit addon");
        assert!(!p.contains("src=\"./"), "no relative asset may be referenced");
        assert!(!p.contains("src=\"http"), "and nothing may be fetched remotely");
        assert!(
            !p.contains("cdn.jsdelivr") && !p.contains("unpkg.com"),
            "a terminal must not depend on a CDN being up",
        );
    }

    /// The client forwards the workspace's chords and NOTHING else. Ctrl belongs
    /// to the shell, Alt is readline's Meta, ⌘ is the browser's — a workspace that
    /// takes one of those breaks the terminal it contains.
    #[test]
    fn only_the_workspace_chord_is_intercepted() {
        let js = CLIENT_JS;
        assert!(js.contains("!e.ctrlKey || !e.altKey"), "the chord is ctrl+alt, and nothing else is taken");
        assert!(js.contains("AltGraph"), "AltGr reports as ctrl+alt and must be excluded");
        assert!(js.contains("return true"), "everything else reaches the pty");
    }

    /// A soft keyboard has no Esc, Ctrl, Tab or arrows, so a touch device without
    /// this row can read a terminal and not use one.
    #[test]
    fn a_touch_device_gets_the_keys_it_has_no_hardware_for() {
        let js = CLIENT_JS;
        for k in ["esc", "tab", "ctrl", "^C"] {
            assert!(js.contains(k), "the key row must carry `{k}`");
        }
        assert!(js.contains("pointer: coarse"), "and only where there is no keyboard");
        assert!(js.contains("visualViewport"), "docked above the software keyboard");
    }
}
