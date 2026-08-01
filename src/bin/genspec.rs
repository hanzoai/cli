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
//!     `--registry <url|path>` reads somewhere else, and may be given MORE THAN
//!     ONCE: the readings are unioned, first one wins every conflict, and every
//!     source is named in the spec's `info.description`. That is the rollout case
//!     — mid-deploy a route lives in cloud `main` before api.hanzo.ai answers it,
//!     so `--registry <wire> --registry <cloud-main drop>` keeps it from being
//!     refuted, while the wire stays first and stays on the record.
//!     A COMMITTED spec must have the wire among its sources (`tests/spec_drift.rs`
//!     asserts it): a table captured to a local file cannot refute what the live
//!     one would, and cannot carry prose the live one has since gained. That is
//!     how 149 phantom `/v1/cloud/*` commands survived, and how 41 platform
//!     commands came to print their HTTP route where their description belonged.
//!     A lone `--registry <path>` is for experiments and air-gapped runs; the
//!     pinned drop hanzoai/openapi carries (`generated/hanzo.json`, written by
//!     `make openapi OPENAPI_DIR=…`) is only as current as the last run of that
//!     target, and today it is a strict SUBSET of what api.hanzo.ai answers
//!     (26 products short).
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
//!
//! THE RULE IS ALSO A PROPERTY OF THE ARTIFACT, not only of a generator run.
//! `--verify` re-reads the committed spec (`--out`) and applies the SAME `owned` +
//! `find` predicate to it, so "no command addresses a route the server does not
//! serve" can be re-asked of the live table at any moment, by anyone, without the
//! authored master. Provenance and freshness are different questions: a spec
//! generated from the wire keeps naming the wire forever, while the router moves
//! underneath it. Recording the source answers the first; only re-asking answers
//! the second. That is what the release gate runs — a `hanzo` whose command tree
//! the server refutes is never minted — and it is what bounds the union above: a
//! reading that let an undeployed route through must become true before it ships.

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

/// The registry's evidence: the route patterns it serves, per method, the set
/// of products it owns — and, for DESCRIBED operations (zip typed ops, whose
/// prose and schemas are generated from the Go code), the operations themselves.
struct Registry {
    routes: BTreeMap<String, Vec<Vec<String>>>,
    owned: BTreeSet<String>,
    /// (METHOD, normalized path) -> (registry's own path key, the op object).
    /// Only ops carrying a summary or description are held: prose is what marks
    /// an operation as authored-in-code rather than an accident of the router.
    ops: BTreeMap<(String, String), (String, Value)>,
    /// The registry document's own component schemas — the shapes zip reflected
    /// from the live Go types, which described ops' $refs resolve against.
    schemas: Map<String, Value>,
    /// Per-product prose from the document's `tags` — each entry's description
    /// is the owning Go package's doc synopsis, lifted by the same weave that
    /// writes the subsets. Products, like operations, describe themselves.
    products: BTreeMap<String, String>,
}

/// One spelling for "the same route whatever the params are named": a literal
/// matches itself, any `{param}` is `{}`, a fiber catch-all is `{*}`.
fn norm(s: &[&str]) -> String {
    s.iter()
        .map(|x| if is_wild(x) { "{*}" } else if is_param(x) { "{}" } else { *x })
        .collect::<Vec<_>>()
        .join("/")
}

/// The one-line prose of an op: its summary, else the first line of its
/// description. Empty means the op is undescribed.
fn prose(op: &Value) -> Option<String> {
    let take = |v: &Value| v.as_str().map(|x| x.lines().next().unwrap_or("").trim().to_string());
    op.get("summary")
        .and_then(take)
        .filter(|x| !x.is_empty())
        .or_else(|| op.get("description").and_then(take).filter(|x| !x.is_empty()))
}

impl Registry {
    fn read(doc: &Value) -> Self {
        let mut routes: BTreeMap<String, Vec<Vec<String>>> = BTreeMap::new();
        let mut owned = BTreeSet::new();
        let mut ops: BTreeMap<(String, String), (String, Value)> = BTreeMap::new();
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
            for (m, op) in item.as_object().into_iter().flatten() {
                if VERBS.contains(&m.to_ascii_lowercase().as_str()) {
                    routes.entry(m.to_ascii_uppercase()).or_default().push(pat.clone());
                    if prose(op).is_some() {
                        ops.insert(
                            (m.to_ascii_uppercase(), norm(&s)),
                            (path.clone(), op.clone()),
                        );
                    }
                }
            }
        }
        let schemas = doc
            .pointer("/components/schemas")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        let mut products = BTreeMap::new();
        for t in doc.get("tags").and_then(Value::as_array).into_iter().flatten() {
            if let (Some(n), Some(d)) =
                (t.get("name").and_then(Value::as_str), t.get("description").and_then(Value::as_str))
            {
                let d = d.trim();
                if !d.is_empty() {
                    products.insert(n.to_string(), d.to_string());
                }
            }
        }
        Registry { routes, owned, ops, schemas, products }
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
fn prune(op: &Value, catch_all: Option<String>, summary: Option<String>) -> Value {
    let mut out = Map::new();
    if let Some(s) = summary.filter(|s| !s.is_empty()) {
        out.insert("summary".into(), json!(s));
    }
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

/// Every operation the spec claims that the route table refutes — the SAME rule
/// `main` generates by (`owned` decides authority, `find` decides service),
/// pointed at an ARTIFACT instead of at the authored master. One predicate, two
/// call sites: generate, then re-check what was generated.
fn refuted(reg: &Registry, spec: &Value) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for (path, item) in spec.get("paths").and_then(Value::as_object).into_iter().flatten() {
        let s = segs(path);
        if s.len() < 2 || s[0] != "v1" || is_param(s[1]) {
            continue;
        }
        for m in item.as_object().into_iter().flatten().map(|(m, _)| m) {
            if !VERBS.contains(&m.to_ascii_lowercase().as_str()) {
                continue;
            }
            let m = m.to_ascii_uppercase();
            if reg.find(&m, &s).is_none() && reg.owned.contains(s[1]) {
                out.push((m, path.clone()));
            }
        }
    }
    out
}

struct Args {
    registry: Vec<String>,
    openapi: PathBuf,
    out: PathBuf,
    verify: bool,
}

fn args() -> Args {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let mut registry: Vec<String> = std::env::var("HANZO_REGISTRY").ok().into_iter().collect();
    let mut openapi = std::env::var("HANZO_OPENAPI_DIR").map(PathBuf::from).unwrap_or_else(|_| {
        manifest.parent().map(|p| p.join("openapi")).unwrap_or_else(|| PathBuf::from("../openapi"))
    });
    let mut out = manifest.join("spec/cloud.json");
    let mut verify = false;

    let argv: Vec<String> = std::env::args().skip(1).collect();
    let mut i = 0;
    while i < argv.len() {
        let flag = argv[i].clone();
        if flag == "--verify" {
            verify = true;
            i += 1;
            continue;
        }
        let val = argv.get(i + 1).cloned().unwrap_or_else(|| panic!("{flag} needs a value"));
        match flag.as_str() {
            "--registry" => registry.push(val),
            "--openapi" => openapi = PathBuf::from(val),
            "--out" => out = PathBuf::from(val),
            other => panic!("usage: genspec [--registry <url|path>]… [--openapi <dir>] [--out <path>] [--verify]\nunknown: {other}"),
        }
        i += 2;
    }
    if registry.is_empty() {
        registry.push(DEFAULT_REGISTRY.to_string());
    }
    Args { registry, openapi, out, verify }
}

/// Read one route table: a URL is the normal case (the registry answers for
/// itself), a path is a checkout or an air-gapped run against a captured table.
async fn read_registry(src: &str) -> Value {
    if src.starts_with("http") {
        let body = reqwest::get(src)
            .await
            .and_then(|r| r.error_for_status())
            .unwrap_or_else(|e| panic!("GET {src}: {e}"))
            .text()
            .await
            .expect("read registry");
        serde_json::from_str(&body).unwrap_or_else(|e| panic!("{src} is not the JSON route table: {e}"))
    } else {
        serde_json::from_str(&std::fs::read_to_string(src).expect("read registry")).expect("parse registry")
    }
}

/// Union of several readings of ONE router at different commits — the shape the
/// rollout case actually has. During a deploy a route can live in cloud `main`
/// and not yet on api.hanzo.ai; refutation needs a product owned AND no matching
/// route, so a union treats a route as served if EITHER reading has it, which is
/// the truth mid-rollout.
///
/// The FIRST reading wins every conflict: later ones may only ADD. Pass the wire
/// first and an extra reading cannot quietly rewrite what production says. This
/// is why a union is expressed as several `--registry` values rather than one
/// pre-merged file: the sources stay named, and `info.description` records them —
/// a spec that erased the wire from its own provenance is what let 41 undescribed
/// operations ship their HTTP route as their help line.
fn union(docs: Vec<Value>) -> Value {
    let mut out = Map::new();
    let mut merge = |key: &str, doc: &Value, nested: bool| {
        let src = match nested {
            true => doc.get("components").and_then(|c| c.get(key)),
            false => doc.get(key),
        };
        let Some(src) = src.and_then(Value::as_object) else { return };
        let dst = out.entry(key.to_string()).or_insert_with(|| Value::Object(Map::new()));
        let dst = dst.as_object_mut().expect("object");
        for (k, v) in src {
            match (dst.get_mut(k), v.as_object()) {
                // Same path key in two readings: keep the earlier reading's
                // methods and add only the ones it did not have.
                (Some(have), Some(add)) if have.is_object() => {
                    let have = have.as_object_mut().expect("object");
                    for (m, op) in add {
                        have.entry(m.clone()).or_insert_with(|| op.clone());
                    }
                }
                (Some(_), _) => {}
                (None, _) => {
                    dst.insert(k.clone(), v.clone());
                }
            }
        }
    };
    for doc in &docs {
        merge("paths", doc, false);
        merge("schemas", doc, true);
    }
    let paths = out.remove("paths").unwrap_or_else(|| Value::Object(Map::new()));
    let schemas = out.remove("schemas").unwrap_or_else(|| Value::Object(Map::new()));
    // `tags` is a LIST keyed by name; first description wins, same rule.
    let mut tags: Map<String, Value> = Map::new();
    for doc in &docs {
        for t in doc.get("tags").and_then(Value::as_array).into_iter().flatten() {
            if let Some(n) = t.get("name").and_then(Value::as_str) {
                tags.entry(n.to_string()).or_insert_with(|| t.clone());
            }
        }
    }
    json!({
        "paths": paths,
        "components": {"schemas": schemas},
        "tags": tags.into_values().collect::<Vec<_>>(),
    })
}

#[tokio::main]
async fn main() {
    let a = args();

    let mut docs = Vec::new();
    for src in &a.registry {
        docs.push(read_registry(src).await);
    }
    let registry = if docs.len() == 1 { docs.remove(0) } else { union(docs) };
    let reg = Registry::read(&registry);

    // Re-check an artifact instead of writing one. Needs no authored master — the
    // spec already carries every operation and the table already answers which of
    // them are served — which is what lets the release gate run this with nothing
    // but a checkout and the network.
    if a.verify {
        let spec: Value = serde_json::from_slice(
            &std::fs::read(&a.out).unwrap_or_else(|e| panic!("read {}: {e}", a.out.display())),
        )
        .unwrap_or_else(|e| panic!("{} is not a spec: {e}", a.out.display()));
        let bad = refuted(&reg, &spec);
        for (m, p) in &bad {
            eprintln!("  refuted  {m} {p}");
        }
        eprintln!(
            "genspec --verify: {} of {} operations in {} address routes {} does not serve",
            bad.len(),
            spec.get("paths")
                .and_then(Value::as_object)
                .map(|p| p.values().filter_map(Value::as_object).map(Map::len).sum())
                .unwrap_or(0usize),
            a.out.display(),
            a.registry.join(" ∪ "),
        );
        if !bad.is_empty() {
            eprintln!("Regenerate: genspec, then genproduct.");
            std::process::exit(1);
        }
        return;
    }

    let master_path = a.openapi.join("hanzo.yaml");
    let master: Value = serde_norway::from_str(
        &std::fs::read_to_string(&master_path)
            .unwrap_or_else(|e| panic!("read {}: {e} — set --openapi <hanzoai/openapi checkout>", master_path.display())),
    )
    .expect("parse hanzo.yaml");

    let mut paths = Map::new();
    let mut emitted: BTreeSet<(String, String)> = BTreeSet::new();
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
            // Prose: the registry's own (generated from the Go doc comment) when
            // the op is typed, else whatever the authored master says.
            let key = (m.to_ascii_uppercase(), norm(&s));
            let summary = reg.ops.get(&key).and_then(|(_, rop)| prose(rop)).or_else(|| prose(op));
            emitted.insert(key);
            item_out.insert(m.to_ascii_lowercase(), prune(op, catch_all, summary));
        }
        if !item_out.is_empty() {
            paths.insert(path.clone(), Value::Object(item_out));
        }
    }

    // THE INVERSION, and the end state of the lineage contract: an operation the
    // REGISTRY describes exists, authored or not. A described op is a zip typed
    // op — its prose and schema are generated from the Go code, which makes the
    // registry the author. The hand-maintained hanzo.yaml stops deciding
    // existence for these; it remains only the shape source for ops the registry
    // cannot yet describe. As typing reaches 100%, hanzo.yaml's job goes to zero.
    let mut added = 0usize;
    for ((method, key), (rpath, rop)) in &reg.ops {
        if emitted.contains(&(method.clone(), key.clone())) {
            continue;
        }
        let s = segs(rpath);
        let pat: Vec<String> = s.iter().map(|x| x.to_string()).collect();
        let catch_all = Registry::catch_all(&pat, &s);
        added += 1;
        paths
            .entry(rpath.clone())
            .or_insert_with(|| Value::Object(Map::new()))
            .as_object_mut()
            .expect("path item")
            .insert(method.to_ascii_lowercase(), prune(rop, catch_all, prose(rop)));
    }

    let empty = Map::new();
    let schemas = master
        .get("components")
        .and_then(|c| c.get("schemas"))
        .and_then(Value::as_object)
        .unwrap_or(&empty);
    // One schema namespace: the authored master's, with the registry's laid over
    // it. On a name both define, the registry wins — its schema is reflected from
    // the live Go type, and the authored one is the hand-written approximation
    // this pipeline exists to retire.
    let mut merged: Map<String, Value> = schemas.clone();
    for (k, v) in &reg.schemas {
        merged.insert(k.clone(), v.clone());
    }
    let keep = reachable(&paths, &merged);
    let components: Map<String, Value> =
        keep.iter().filter_map(|n| merged.get(n).map(|s| (n.clone(), s.clone()))).collect();

    // The OpenAPI-native place for per-product prose: a `tags` entry per product
    // that actually has operations in this spec AND a description in the
    // registry. `genproduct` turns these into the product groups' help lines.
    let present: BTreeSet<&str> = paths.keys().filter_map(|p| segs(p).get(1).copied()).collect();
    let tags: Vec<Value> = reg
        .products
        .iter()
        .filter(|(n, _)| present.contains(n.as_str()))
        .map(|(n, d)| json!({"name": n, "description": d}))
        .collect();

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
                a.registry.join(" ∪ ")
            ),
        },
        "paths": paths,
        "components": {"schemas": components},
    });
    let mut doc = doc;
    if !tags.is_empty() {
        doc.as_object_mut().expect("doc").insert("tags".into(), json!(tags));
    }

    let bytes = serde_json::to_vec(&doc).expect("encode");
    std::fs::write(&a.out, &bytes).unwrap_or_else(|e| panic!("write {}: {e}", a.out.display()));

    let total: usize = refuted.values().sum();
    let mut worst: Vec<_> = refuted.iter().collect();
    worst.sort_by_key(|(p, n)| (std::cmp::Reverse(**n), (*p).clone()));
    eprintln!(
        "genspec: {kept} authored kept, {added} added from the registry (described ops), {total} refuted \
         ({} owned products) -> {} ({} bytes)",
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
