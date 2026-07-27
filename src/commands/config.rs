//! `hanzo config` — manage LOCAL CLI settings (`~/.config/hanzo/config.toml`).
//!
//! `list` prints the whole document; `get KEY` reads one dotted key
//! (`network.active`, `code.link`); `set KEY VALUE` writes one. A `set` goes
//! through the ONE atomic writer (`Config::update`: lock, re-read, tmp+rename) so
//! it never races another `hanzo` process, and it REFUSES a value that would make
//! the document unparseable — a settings editor must never corrupt the file the
//! rest of the CLI reads. Non-secret settings only: tokens and wallet keys live
//! in the credential vault, never here (`config list` can never leak one).

use anyhow::{anyhow, bail, Context, Result};
use serde_json::Value;

use crate::config::Config;

/// `hanzo config list` — the whole config as pretty TOML.
pub fn list(cfg: &Config) -> Result<()> {
    print!("{}", toml::to_string_pretty(cfg).context("serializing config")?);
    Ok(())
}

/// `hanzo config get KEY` — one dotted key.
pub fn get(cfg: &Config, key: &str) -> Result<()> {
    let v = tree(cfg)?;
    let found = walk(&v, key).ok_or_else(|| anyhow!("no such key: {key}"))?;
    println!("{}", render(found));
    Ok(())
}

/// `hanzo config set KEY VALUE` — set one dotted key, verified.
pub fn set(cfg: &mut Config, key: &str, value: &str) -> Result<()> {
    cfg.update(|fresh| {
        let mut v = tree(fresh)?;
        place(&mut v, key, scalar(value))?;
        // Round-trip: the mutated document MUST still deserialize to a valid
        // Config, or we refuse — never write a file `Config::load` would reject.
        // `path` is `#[serde(skip)]`, so it is absent here and defaulted back on
        // load; `update` writes with its own captured path regardless.
        *fresh = serde_json::from_value(v)
            .with_context(|| format!("`{key} = {value}` would make the config invalid"))?;
        Ok(())
    })?;
    println!("{key} = {value}");
    Ok(())
}

/// The config as a navigable JSON tree (its Serialize mirror; `path` is skipped).
fn tree(cfg: &Config) -> Result<Value> {
    serde_json::to_value(cfg).context("reading config")
}

/// Follow a dotted path through nested objects.
fn walk<'a>(v: &'a Value, key: &str) -> Option<&'a Value> {
    key.split('.').try_fold(v, |cur, seg| cur.get(seg))
}

/// Set a dotted path, creating intermediate objects. REFUSES to traverse through
/// an existing non-object (e.g. `code.link.x` when `code.link` is a bool) — it
/// never clobbers a scalar to make room for a deeper key.
fn place(v: &mut Value, key: &str, val: Value) -> Result<()> {
    let segs: Vec<&str> = key.split('.').collect();
    if segs.iter().any(|s| s.is_empty()) {
        bail!("invalid key: {key:?}");
    }
    let mut cur = v;
    for (i, seg) in segs.iter().enumerate() {
        let obj = cur
            .as_object_mut()
            .ok_or_else(|| anyhow!("cannot set `{key}`: `{seg}` is not a table"))?;
        if i + 1 == segs.len() {
            obj.insert(seg.to_string(), val);
            return Ok(());
        }
        match obj.get(*seg) {
            Some(existing) if !existing.is_object() => {
                bail!("cannot set `{key}`: `{seg}` is not a table")
            }
            Some(_) => {}
            None => {
                obj.insert(seg.to_string(), Value::Object(Default::default()));
            }
        }
        cur = obj.get_mut(*seg).unwrap();
    }
    Ok(())
}

/// Interpret a CLI value at its natural JSON type: `true`/`false` → bool, an
/// integer or float → number, everything else → string. So `config set code.link
/// false` stores a real bool the typed Config expects, not the string "false".
fn scalar(s: &str) -> Value {
    match s {
        "true" => return Value::Bool(true),
        "false" => return Value::Bool(false),
        _ => {}
    }
    if let Ok(i) = s.parse::<i64>() {
        return Value::from(i);
    }
    if let Ok(f) = s.parse::<f64>() {
        return Value::from(f);
    }
    Value::String(s.to_string())
}

/// Render a found value for `get`: a bare string prints its inner text (pipes
/// clean); a structure prints compact JSON.
fn render(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scalar_types_are_natural() {
        assert_eq!(scalar("true"), Value::Bool(true));
        assert_eq!(scalar("false"), Value::Bool(false));
        assert_eq!(scalar("42"), Value::from(42i64));
        assert_eq!(scalar("enso"), Value::String("enso".into()));
    }

    #[test]
    fn walk_reads_a_dotted_key() {
        let v = serde_json::json!({"code": {"link": true, "theme": "dracula"}});
        assert_eq!(walk(&v, "code.theme"), Some(&Value::String("dracula".into())));
        assert_eq!(walk(&v, "code.link"), Some(&Value::Bool(true)));
        assert_eq!(walk(&v, "code.missing"), None);
    }

    #[test]
    fn place_sets_and_creates_intermediate_tables() {
        let mut v = serde_json::json!({"code": {"link": true}});
        place(&mut v, "code.theme", Value::String("nord".into())).unwrap();
        assert_eq!(walk(&v, "code.theme"), Some(&Value::String("nord".into())));
        place(&mut v, "a.b.c", Value::from(1i64)).unwrap();
        assert_eq!(walk(&v, "a.b.c"), Some(&Value::from(1i64)));
        // Setting THROUGH a scalar is refused, not silently reshaped.
        assert!(place(&mut v, "code.link.x", Value::from(1i64)).is_err());
    }
}
