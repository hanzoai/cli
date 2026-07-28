//! `genspec` — build `spec/cloud.json`, the ONE spec `genproduct` derives the
//! cloud command surface from. This is the refresh seam: run it to pull the
//! surface forward, commit the result, then run `genproduct`.
//!
//! It joins the two documents that are each authoritative about half an
//! operation. The client links NEITHER — it reads a spec, which is the whole
//! point: a CLI that must import cloud to know its commands is the thing being
//! eliminated.
//!
//!   * WHAT IS SERVED — cloud's own route table, over the wire from
//!     `<api>/v1/openapi.json`. That document is a projection of the LIVE fiber
//!     router (hanzoai/cloud `openapi.Spec` reads `app.Fiber().GetRoutes()`), so a
//!     route that is not mounted cannot appear in it. Same move as zip's
//!     `CommandsFromSpec`: ask a running service what it can do.
//!
//!     `--registry <path>` reads a captured table instead — the drop the binary
//!     publishes into hanzoai/openapi (`generated/hanzo.json`, written by
//!     `make openapi OPENAPI_DIR=…`) is the pinned, offline form. Prefer the wire:
//!     the drop is only as current as the last run of that target, and today it is
//!     a strict SUBSET of what api.hanzo.ai answers (26 products short).
//!   * WHAT IT TAKES — the authored master `hanzo.yaml` from hanzoai/openapi
//!     (itself merged from the per-service specs by that repo's `merge.py`).
//!     It is the only source of request-body and query-parameter SHAPE, because
//!     most of cloud's handlers are still raw fiber handlers and the registry
//!     has no Go type to read a schema off. As handlers become zip typed ops
//!     their schemas appear in the registry document and this half shrinks.
//!
//! THE RULE — the registry refutes at PRODUCT granularity. If cloud's table has
//! any route under `/v1/<product>/`, cloud owns that product and its table is
//! complete for it: an authored operation the table lacks is not served, and is
//! dropped. If the table is silent about a product, cloud is not the authority
//! over it — the inference surface (`/v1/models`, `/v1/chat/completions`) is
//! answered at the edge by the gateway, not by this router — so nothing is
//! refuted and the authored operation stands.
//!
//! That rule is what retires the hand-maintained "this one 404s" knowledge: the
//! `gateway` subtree drops out because the registry serves none of it, not
//! because a list in `genproduct` says so.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use serde_json::{json, Map, Value};

const VERBS: [&str; 5] = ["get", "post", "put", "patch", "delete"];
const DEFAULT_REGISTRY: &str = "https://api.hanzo.ai/v1/openapi.json";

fn segs(p: &str) -> Vec<&str> {
    p.split('/').filter(|s| !s.is_empty()).collect()
}
fn is_param(s: &str) -> bool {
    s.starts_with('{') && s.ends_with('}')
}
/// A fiber `*` catch-all, which `openapi.translate` names `{wildcardN}`.
fn is_wild(s: &str) -> bool {
    is_param(s) && s.contains("wild")
}

/// The registry's evidence: the route patterns it serves, per method, and the
/// set of products it owns.
struct Registry {
    routes: BTreeMap<String, Vec<Vec<String>>>,
    owned: BTreeSet<String>,
}

impl Registry {
    fn read(doc: &Value) -> Self {
        let mut routes: BTreeMap<String, Vec<Vec<String>>> = BTreeMap::new();
        let mut owned = BTreeSet::new();
        for (path, item) in doc.get("paths").and_then(Value::as_object).into_iter().flatten() {
            let s = segs(path);
            // The bare `/v1/*` is the global fallthrough. Counting it as
            // evidence would make every conceivable path "served".
            if s.len() < 2 || s[0] != "v1" || (s.len() == 2 && is_wild(s[1])) {
                continue;
            }
            if !is_param(s[1]) {
                owned.insert(s[1].to_string());
            }
            let pat: Vec<String> = s.iter().map(|x| x.to_string()).collect();
            for m in item.as_object().into_iter().flatten().map(|(m, _)| m) {
                if VERBS.contains(&m.to_ascii_lowercase().as_str()) {
                    routes.entry(m.to_ascii_uppercase()).or_default().push(pat.clone());
                }
            }
        }
        Registry { routes, owned }
    }

    /// Segment-by-segment match against one route pattern. A literal matches
    /// itself; a `{param}` matches a `{param}` (the router's names are its own,
    /// not the spec's); a trailing `{wildcardN}` matches the rest of the path.
    fn matches(pat: &[String], path: &[&str]) -> bool {
        for (k, ps) in pat.iter().enumerate() {
            if is_wild(ps) {
                return path.len() > k;
            }
            match path.get(k) {
                None => return false,
                Some(seg) if is_param(ps) => {
                    if !is_param(seg) {
                        return false;
                    }
                }
                Some(seg) if ps != seg => return false,
                _ => {}
            }
        }
        path.len() == pat.len()
    }

    /// The route pattern that serves this operation, if any.
    fn find(&self, method: &str, path: &[&str]) -> Option<&Vec<String>> {
        self.routes.get(method)?.iter().find(|p| Self::matches(p, path))
    }

    /// The authored parameter a fiber `*` stands in for — a MULTI-SEGMENT value
    /// whose slashes are structural (a KMS secret is `sub/path/name`, an S3 key is
    /// `a/b/c.txt`). OpenAPI cannot say this, but the router does, so the fact
    /// comes from the same registry reading as existence instead of a table in the
    /// client.
    ///
    /// It counts only when the catch-all lines up ONE-TO-ONE with one authored
    /// parameter at the end of a route the router otherwise knows segment for
    /// segment. A product-level `/v1/<product>/*` is a whole subtree proxied
    /// elsewhere — it says nothing about any parameter under it, and reading it as
    /// one would send raw slashes into every id in the subtree.
    fn catch_all(pattern: &[String], path: &[&str]) -> Option<String> {
        let w = pattern.len() - 1;
        let tail = path.get(w)?;
        (is_wild(&pattern[w]) && w > 2 && path.len() == pattern.len() && is_param(tail))
            .then(|| tail.trim_start_matches('{').trim_end_matches('}').to_string())
    }
}

/// Keep only what the derivation reads: query/path parameters, the JSON request
/// body, and the router's catch-all marking. Everything else in an authored
/// operation (responses, security, summaries) describes the API to a human or an
/// SDK, not a command line, and carrying it would bloat a committed artifact
/// nobody edits.
fn prune(op: &Value, catch_all: Option<String>) -> Value {
    let mut out = Map::new();
    if let Some(p) = catch_all {
        out.insert("x-catch-all".into(), json!([p]));
    }
    if let Some(p) = op.get("parameters").filter(|v| !v.as_array().is_none_or(|a| a.is_empty())) {
        out.insert("parameters".into(), p.clone());
    }
    if let Some(schema) = op
        .get("requestBody")
        .and_then(|rb| rb.get("content"))
        .and_then(|c| c.get("application/json"))
        .and_then(|m| m.get("schema"))
    {
        out.insert("requestBody".into(), json!({"content": {"application/json": {"schema": schema}}}));
    }
    Value::Object(out)
}

/// Every `#/components/schemas/<name>` reachable from the kept operations,
/// followed transitively. A schema nothing refers to is not part of the command
/// surface and does not belong in its spec.
fn reachable(paths: &Map<String, Value>, schemas: &Map<String, Value>) -> BTreeSet<String> {
    fn refs(v: &Value, out: &mut Vec<String>) {
        match v {
            Value::Object(m) => {
                for (k, x) in m {
                    if k == "$ref" {
                        if let Some(n) = x.as_str().and_then(|r| r.strip_prefix("#/components/schemas/")) {
                            out.push(n.to_string());
                        }
                    }
                    refs(x, out);
                }
            }
            Value::Array(a) => a.iter().for_each(|x| refs(x, out)),
            _ => {}
        }
    }
    let mut queue = Vec::new();
    refs(&Value::Object(paths.clone()), &mut queue);
    let mut seen = BTreeSet::new();
    while let Some(name) = queue.pop() {
        if !seen.insert(name.clone()) {
            continue;
        }
        if let Some(s) = schemas.get(&name) {
            refs(s, &mut queue);
        }
    }
    seen
}

struct Args {
    registry: String,
    openapi: PathBuf,
    out: PathBuf,
}

fn args() -> Args {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let mut registry = std::env::var("HANZO_REGISTRY").ok();
    let mut openapi = std::env::var("HANZO_OPENAPI_DIR").map(PathBuf::from).unwrap_or_else(|_| {
        manifest.parent().map(|p| p.join("openapi")).unwrap_or_else(|| PathBuf::from("../openapi"))
    });
    let mut out = manifest.join("spec/cloud.json");

    let argv: Vec<String> = std::env::args().skip(1).collect();
    let mut i = 0;
    while i < argv.len() {
        let val = || argv.get(i + 1).cloned().unwrap_or_else(|| panic!("{} needs a value", argv[i]));
        match argv[i].as_str() {
            "--registry" => registry = Some(val()),
            "--openapi" => openapi = PathBuf::from(val()),
            "--out" => out = PathBuf::from(val()),
            other => panic!("usage: genspec [--registry <url|path>] [--openapi <dir>] [--out <path>]\nunknown: {other}"),
        }
        i += 2;
    }
    let registry = registry.unwrap_or_else(|| DEFAULT_REGISTRY.to_string());
    Args { registry, openapi, out }
}

#[tokio::main]
async fn main() {
    let a = args();

    // A URL is the normal case — the registry answers for itself. A path is for
    // a checkout or an air-gapped run against a captured table.
    let registry: Value = if a.registry.starts_with("http") {
        let body = reqwest::get(&a.registry)
            .await
            .and_then(|r| r.error_for_status())
            .unwrap_or_else(|e| panic!("GET {}: {e}", a.registry))
            .text()
            .await
            .expect("read registry");
        serde_json::from_str(&body).unwrap_or_else(|e| panic!("{} is not the JSON route table: {e}", a.registry))
    } else {
        serde_json::from_str(&std::fs::read_to_string(&a.registry).expect("read registry")).expect("parse registry")
    };
    let reg = Registry::read(&registry);

    let master_path = a.openapi.join("hanzo.yaml");
    let master: Value = serde_norway::from_str(
        &std::fs::read_to_string(&master_path)
            .unwrap_or_else(|e| panic!("read {}: {e} — set --openapi <hanzoai/openapi checkout>", master_path.display())),
    )
    .expect("parse hanzo.yaml");

    let mut paths = Map::new();
    let (mut kept, mut refuted) = (0usize, BTreeMap::<String, usize>::new());
    for (path, item) in master.get("paths").and_then(Value::as_object).into_iter().flatten() {
        let s = segs(path);
        // A `?query` path key (S3-style sub-resource selectors) is not a distinct
        // resource, and a parameterised first segment is not a product.
        if s.len() < 2 || s[0] != "v1" || is_param(s[1]) || path.contains('?') || path.contains('#') {
            continue;
        }
        let mut item_out = Map::new();
        for (m, op) in item.as_object().into_iter().flatten() {
            if !VERBS.contains(&m.to_ascii_lowercase().as_str()) {
                continue;
            }
            let route = reg.find(&m.to_ascii_uppercase(), &s);
            // Only a product the registry OWNS can be refuted at all.
            if route.is_none() && reg.owned.contains(s[1]) {
                *refuted.entry(s[1].to_string()).or_default() += 1;
                continue;
            }
            kept += 1;
            let catch_all = route.and_then(|r| Registry::catch_all(r, &s));
            item_out.insert(m.to_ascii_lowercase(), prune(op, catch_all));
        }
        if !item_out.is_empty() {
            paths.insert(path.clone(), Value::Object(item_out));
        }
    }

    let empty = Map::new();
    let schemas = master
        .get("components")
        .and_then(|c| c.get("schemas"))
        .and_then(Value::as_object)
        .unwrap_or(&empty);
    let keep = reachable(&paths, schemas);
    let components: Map<String, Value> =
        keep.iter().filter_map(|n| schemas.get(n).map(|s| (n.clone(), s.clone()))).collect();

    let doc = json!({
        "openapi": "3.1.0",
        "info": {
            "title": "Hanzo Cloud — the CLI's command surface",
            "version": master.pointer("/info/version").and_then(Value::as_str).unwrap_or("unknown"),
            "description": format!(
                "Generated by `cargo run --features genspec --bin genspec`. The authored shapes \
                 from hanzoai/openapi hanzo.yaml, keeping only what the live cloud route table at \
                 {} does not refute. Never hand-edited: `genproduct` derives src/commands/product/\
                 generated.rs from this and nothing else.",
                a.registry
            ),
        },
        "paths": paths,
        "components": {"schemas": components},
    });

    let bytes = serde_json::to_vec(&doc).expect("encode");
    std::fs::write(&a.out, &bytes).unwrap_or_else(|e| panic!("write {}: {e}", a.out.display()));

    let total: usize = refuted.values().sum();
    let mut worst: Vec<_> = refuted.iter().collect();
    worst.sort_by_key(|(p, n)| (std::cmp::Reverse(**n), (*p).clone()));
    eprintln!(
        "genspec: {kept} operations kept, {total} refuted by the registry ({} owned products) -> {} ({} bytes)",
        reg.owned.len(),
        a.out.display(),
        bytes.len()
    );
    for (p, n) in worst.iter().take(12) {
        eprintln!("  refuted {n:>4}  {p}");
    }
    if worst.len() > 12 {
        eprintln!("  … and {} more products", worst.len() - 12);
    }
}
