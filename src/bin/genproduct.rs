//! `genproduct` — regenerate `src/commands/product/generated.rs` from a COMMITTED
//! OpenAPI snapshot. Run by hand when the snapshot changes; the output is checked
//! in, so the `hanzo` binary never fetches a spec at runtime.
//!
//! It reads only the router SHAPE — method, path, path params — because that is
//! all cloud's router-derived spec carries (no request/response schemas exist).
//! It folds every `/v1` path into one (product, resource nodes, verb, method,
//! path template, params) coordinate via a TOTAL fold over path segments, then
//! emits pure DATA: no host, no URL, no auth. See `commands::product`.
//!
//! Usage: `cargo run --bin genproduct [-- <spec.json> <out.rs>]`.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::PathBuf;

use serde_json::Value;

const VERBS: [&str; 5] = ["GET", "POST", "PUT", "PATCH", "DELETE"];
/// Local commands own these bare names; the generated tree never claims them.
/// `code` is NOT excluded — its verbs mount under the `hanzo code` wrapper.
const EXCLUDE: [&str; 5] = ["agent", "kms", "billing", "deploy", "openapi.json"];
/// Collapse a coordinate served by several methods; prefer the most-specific.
const METHOD_PRIORITY: [&str; 5] = ["PATCH", "PUT", "POST", "DELETE", "GET"];

fn is_param(s: &str) -> bool {
    s.starts_with('{') && s.ends_with('}')
}
fn is_wild(s: &str) -> bool {
    is_param(s) && (s.contains("wild") || s.contains('*'))
}
fn pname(s: &str) -> &str {
    s.trim_start_matches('{').trim_end_matches('}')
}
fn segs(path: &str) -> Vec<&str> {
    path.split('/').filter(|s| !s.is_empty()).collect()
}

/// A route strictly extends P by ≥1 segment ⇒ P is a GROUP node.
fn has_child(p: &str, all: &[String]) -> bool {
    let pre = format!("{}/", p.trim_end_matches('/'));
    all.iter().any(|k| k != p && k.trim_end_matches('/').starts_with(&pre))
}

/// A group node is a COLLECTION iff it has an immediate param child `P/{x}`.
fn is_collection(p: &str, all: &[String]) -> bool {
    let ps = segs(p);
    all.iter().any(|k| {
        let ks = segs(k);
        ks.len() == ps.len() + 1 && ks[..ps.len()] == ps[..] && is_param(ks[ks.len() - 1])
    })
}

/// The command tokens after the product, with the tenant-org scope
/// (`orgs/{org}`) elided — its param binds to the caller's `owner` at dispatch,
/// so it is never a user positional. This is the ONE scope rule, mirrored by
/// `product::fill_path` (a param preceded by `orgs` = owner).
fn cmd_tokens(sg: &[&str]) -> Vec<String> {
    let mut out = Vec::new();
    let mut j = 1usize;
    // product == "orgs" ⇒ the tenant param is segs[1]; elide it, keep the product.
    if sg[0] == "orgs" && sg.len() > 1 && is_param(sg[1]) {
        j = 2;
    }
    while j < sg.len() {
        if sg[j] == "orgs" && j + 1 < sg.len() && is_param(sg[j + 1]) {
            j += 2; // elide the `orgs` literal AND its param
            continue;
        }
        out.push(sg[j].to_string());
        j += 1;
    }
    out
}

struct Coord {
    product: String,
    nodes: Vec<String>,
    verb: String,
    method: String,
    path: String,
    params: Vec<String>,
}

/// THE total fold: (method, path) → one coordinate, or None when the path is a
/// wildcard/whole-cloud catch-all (not enumerable).
fn fold(method: &str, path: &str, all: &[String]) -> Option<Coord> {
    let sg = segs(path);
    let sg = &sg[1..]; // drop "v1"
    if sg.is_empty() || is_wild(sg[0]) {
        return None;
    }
    let product = sg[0].to_string();
    let ct = cmd_tokens(sg);
    let params: Vec<String> = ct.iter().filter(|s| is_param(s)).map(|s| pname(s).to_string()).collect();
    let p_trim = path.trim_end_matches('/').to_string();

    if ct.is_empty() {
        let verb = match method {
            "GET" => if is_collection(&p_trim, all) { "list" } else { "get" },
            "POST" => "create",
            "PUT" | "PATCH" => "update",
            _ => "rm",
        };
        return Some(Coord { product, nodes: vec![], verb: verb.into(), method: method.into(), path: path.into(), params });
    }

    // Build the resource-node chain. A literal names its level. A param names its
    // level ONLY in a param-stack (a param immediately followed by another param),
    // where there is no literal noun to distinguish depth — this keeps the fold
    // total (0 arity collisions) without inventing nouns anywhere else.
    let mut nodes = Vec::new();
    for i in 0..ct.len() {
        let tok = &ct[i];
        let terminal = i == ct.len() - 1;
        let next = if terminal { None } else { Some(&ct[i + 1]) };
        if is_param(tok) {
            if !terminal {
                if let Some(n) = next {
                    if is_param(n) {
                        nodes.push(pname(tok).to_string());
                    }
                }
            }
            continue;
        }
        if terminal {
            break; // terminal literal is the verb, handled below
        }
        nodes.push(tok.clone());
    }

    let last = &ct[ct.len() - 1];
    let verb: String = if is_param(last) {
        match method {
            "GET" => "get",
            "DELETE" => "rm",
            _ => "update",
        }
        .into()
    } else if has_child(&p_trim, all) {
        let v = match method {
            "GET" => if is_collection(&p_trim, all) { "list" } else { "get" },
            "POST" => "create",
            "PUT" | "PATCH" => "update",
            _ => "rm",
        };
        nodes.push(last.clone());
        v.into()
    } else {
        last.clone()
    };
    Some(Coord { product, nodes, verb, method: method.into(), path: path.into(), params })
}

fn method_rank(m: &str) -> usize {
    METHOD_PRIORITY.iter().position(|x| *x == m).unwrap_or(usize::MAX)
}

fn main() {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let args: Vec<String> = std::env::args().skip(1).collect();
    let spec = args.first().map(PathBuf::from).unwrap_or_else(|| manifest.join("spec/openapi.json"));
    let out = args.get(1).map(PathBuf::from).unwrap_or_else(|| manifest.join("src/commands/product/generated.rs"));

    let d: Value = serde_json::from_str(&fs::read_to_string(&spec).expect("read spec")).expect("parse spec");
    let paths = d.get("paths").and_then(Value::as_object).expect("spec.paths");

    let all: Vec<String> = paths.keys().filter(|k| k.starts_with("/v1/")).cloned().collect();

    // Fold every concrete op; collapse multi-method coordinates by priority.
    let mut coords: BTreeMap<(String, Vec<String>, String), Coord> = BTreeMap::new();
    let mut pure_passthrough: BTreeSet<String> = BTreeSet::new();
    let mut prod_has_concrete: BTreeSet<String> = BTreeSet::new();
    let mut prod_has_wild: BTreeSet<String> = BTreeSet::new();

    for k in &all {
        let sg = segs(k);
        let product = sg[1].to_string();
        if is_wild(sg[1]) {
            continue;
        }
        let tail_wild = sg[2..].iter().any(|s| is_wild(s));
        let ops = paths.get(k).and_then(Value::as_object).unwrap();
        for m in ops.keys() {
            let m = m.to_uppercase();
            if !VERBS.contains(&m.as_str()) {
                continue;
            }
            if tail_wild {
                prod_has_wild.insert(product.clone());
                continue;
            }
            let is_bare_or_health = sg.len() == 2 || (sg.len() == 3 && sg[2] == "health");
            if !is_bare_or_health {
                prod_has_concrete.insert(product.clone());
            }
            if EXCLUDE.contains(&product.as_str()) {
                continue;
            }
            if let Some(c) = fold(&m, k, &all) {
                let key = (c.product.clone(), c.nodes.clone(), c.verb.clone());
                match coords.get(&key) {
                    Some(prev) if method_rank(&prev.method) <= method_rank(&c.method) => {}
                    _ => {
                        coords.insert(key, c);
                    }
                }
            }
        }
    }
    // A product that is ALL wildcard (no enumerable concrete route) is a pure
    // passthrough: it gets a `hanzo api`-style forward, not a broken empty tree.
    for p in &prod_has_wild {
        if !prod_has_concrete.contains(p) && !EXCLUDE.contains(&p.as_str()) && !is_param(p) {
            pure_passthrough.insert(p.clone());
        }
    }
    // A passthrough product owns its whole name — drop the one enumerable route it
    // has (a `/health` folds to a `health` verb) so it is never BOTH a product and
    // a passthrough (`hanzo base health` still works via the forward).
    coords.retain(|k, _| !pure_passthrough.contains(&k.0));

    // ---- emit ----
    let mut s = String::new();
    s.push_str("//! @generated by `cargo run --bin genproduct` from the committed spec\n");
    s.push_str("//! snapshot at `spec/openapi.json`. DO NOT EDIT BY HAND.\n//!\n");
    s.push_str("//! Pure DATA: (product, resource nodes, verb, method, /v1 path template,\n");
    s.push_str("//! params). No host, no absolute URL, no auth — pinned by a trust-boundary\n");
    s.push_str("//! test. Behavior lives in `super` (bind args -> the one `api::call` seam).\n\n");
    s.push_str("use super::Op;\n\n");
    s.push_str(&format!("/// {} coordinates across {} products, folded from the router shape.\n", coords.len(), coords.values().map(|c| c.product.clone()).collect::<BTreeSet<_>>().len()));
    s.push_str("pub(crate) static OPS: &[Op] = &[\n");
    for c in coords.values() {
        let nodes = c.nodes.iter().map(|n| format!("{:?}", n)).collect::<Vec<_>>().join(", ");
        let params = c.params.iter().map(|p| format!("{:?}", p)).collect::<Vec<_>>().join(", ");
        s.push_str(&format!(
            "    Op {{ product: {:?}, nodes: &[{}], verb: {:?}, method: {:?}, path: {:?}, params: &[{}] }},\n",
            c.product, nodes, c.verb, c.method, c.path, params
        ));
    }
    s.push_str("];\n\n");
    s.push_str("/// Pure catch-all products (`/v1/<p>/*`) — no enumerable subcommands; a\n");
    s.push_str("/// passthrough forwards an arbitrary sub-path through the same seam.\n");
    s.push_str("pub(crate) static PASSTHROUGH: &[&str] = &[\n");
    for p in &pure_passthrough {
        s.push_str(&format!("    {:?},\n", p));
    }
    s.push_str("];\n");

    fs::write(&out, s).expect("write generated.rs");
    eprintln!(
        "genproduct: {} coordinates, {} passthrough products -> {}",
        coords.len(),
        pure_passthrough.len(),
        out.display()
    );
}
