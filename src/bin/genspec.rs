//! `genspec` — build `spec/cloud.json`, the ONE spec `genproduct` derives the
//! cloud command surface from. This is the refresh seam: run it to pull the
//! surface forward, commit the result, then run `genproduct`.
//!
//! THE SOURCE IS ONE DOCUMENT: cloud's own `openapi.yaml`. It ENUMERATES — every
//! operation it carries is an operation the CLI has — and its own shape is the
//! shape, because zip reflects that off the live Go type. The authored master is
//! a SUPPLEMENT: it is read at the document's own addresses to fill in a request
//! body or a query parameter the document does not yet carry, and it is read on
//! its own only where the document is NOT the authority (below). It no longer
//! decides that anything exists that the document describes.
//!
//! That is the whole difference from a refuter. A refuter is asked "is this
//! authored operation real?" and can only answer about operations someone thought
//! to author; 244 operations took their request body from a hand-written
//! approximation while cloud published the shape reflected off the Go type, and 66
//! more carried no body at all because the master was silent about an operation
//! the document types. Asking the document FIRST makes both cases the same case.
//!
//! The client links NEITHER document — it reads a spec, which is the whole
//! point: a CLI that must import cloud to know its commands is the thing being
//! eliminated.
//!
//!   * WHAT IS SERVED — cloud's own route table, either over the wire from
//!     `<api>/v1/openapi.json` or as the `openapi.yaml` committed at a release
//!     tag. Same document, two encodings; `read_registry` takes either.
//!
//!     WHAT THAT DOCUMENT IS, precisely, because the rule below rests on it and
//!     the earlier answer here was wrong: it is the weave of the per-app subsets
//!     each app binary emits FROM ITS OWN ROUTER (`<app> openapi`, embedded by
//!     `plugin/embed.go`), not a read of the serving process's router. The light
//!     host mounts no subsystem, so it cannot read one. That makes the document
//!     a projection of the routers AT BUILD TIME — true of the binary that was
//!     released, and true of production only because hanzoai/cloud's release
//!     gate regenerates every subset from source and refuses to ship a tree
//!     where they disagree (`make -f mk/fleet.mk surface-check`). The claim is
//!     no weaker for being indirect, but it is a different claim, and stating it
//!     as "the live router" is how three renamed billing routes shipped
//!     published-dead and served-undocumented from one binary.
//!
//!     So a route absent from the document was absent from the router of the
//!     binary that emitted it — which is what the refutation rule needs.
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
//!   * WHAT IT SUPPLEMENTS — the authored master `hanzo.yaml` from hanzoai/openapi
//!     (itself merged from the per-service specs by that repo's `merge.py`).
//!     It carries request-body and query-parameter SHAPE for operations whose
//!     handlers are still raw fiber handlers, so the registry has no Go type to
//!     read a schema off. As handlers become zip typed ops their schemas appear
//!     in the document and this half shrinks toward nothing.
//!
//! THE RULE — the document is the authority at PRODUCT granularity. If it has any
//! route under `/v1/<product>/`, cloud owns that product and its table is complete
//! for it: an authored operation the table lacks is not served, and is dropped. If
//! the table is silent about a product, cloud is not the authority over it — the
//! inference surface (`/v1/models`, `/v1/chat/completions`) is answered at the edge,
//! not by this router — so nothing is refuted and the authored operation stands.
//!
//! One consequence worth naming, because it is the reason the master is still
//! read at all: a `/v1/<product>/*` catch-all says everything under that prefix
//! REACHES the mounted service, and says nothing about what the service serves
//! there. `find` matches through the door, so the master may enumerate behind it —
//! that is where the iam and bot subtrees come from. The day those services publish
//! their own documents, the door becomes a list and the master's job there ends.
//!
//! That rule is what retires the hand-maintained "this one 404s" knowledge: the
//! `gateway` subtree drops out because the registry serves none of it, not
//! because a list in `genproduct` says so.
//!
//! THE RULE IS ALSO A PROPERTY OF THE ARTIFACT, not only of a generator run —
//! and there are TWO properties, because there are two ways for a capture to
//! stop being true. Recording the source in `info.description` answers neither;
//! provenance is not freshness.
//!
//!   `--check`  IS THE SPEC STILL ITS INPUTS? Re-runs the whole generation
//!     against a PINNED document and refuses if the bytes differ from `--out`.
//!     This is the gate that fires when cloud ships: a release hands this repo
//!     `openapi.yaml` at its own sha (`HANZO_REGISTRY`), and a capture that no
//!     longer projects it goes red instead of silently describing last week's
//!     surface. Nothing is written — a check that repairs what it measures
//!     reports success for a tree that was wrong when the job started.
//!
//!   IS THE SPEC STILL TRUE OF PRODUCTION? is the OTHER question, and it is not
//!     asked here. `--verify` used to ask half of it — the same `owned` + `find`
//!     predicate pointed at the committed spec — and it was deleted rather than
//!     kept beside a stronger gate asking the same thing. It could only refute
//!     inside a product the live table OWNS, which leaves everything behind a `*`
//!     relay door and the whole edge-answered inference plane un-refutable, and it
//!     never asked the other direction at all. `driftgate` applies that predicate
//!     AND asks the running host wherever the table can only answer with a door or
//!     a silence, in BOTH directions. One predicate, one place; `make verify`.
//!
//! One document, two questions, and a capture has to survive both. This binary
//! answers the first; `src/bin/driftgate.rs` answers the second.

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
    /// EVERY served operation, described or not: existence is the router's
    /// answer, and whether an operation also has a sentence to its name is a
    /// separate question, asked by whoever projects it. (`genproduct` refuses to
    /// build a command with nothing to say — see its bare-summary assert.)
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
            // A parameterised first segment is not a product, so nothing under it
            // is an operation OF one — it is the global fallthrough wearing a
            // longer path.
            let product = !is_param(s[1]);
            if product {
                owned.insert(s[1].to_string());
            }
            let pat: Vec<String> = s.iter().map(|x| x.to_string()).collect();
            for (m, op) in item.as_object().into_iter().flatten() {
                if VERBS.contains(&m.to_ascii_lowercase().as_str()) {
                    routes.entry(m.to_ascii_uppercase()).or_default().push(pat.clone());
                    if product {
                        ops.insert((m.to_ascii_uppercase(), norm(&s)), (path.clone(), op.clone()));
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

/// An operation's query/path parameters, if it declares any.
fn params(op: &Value) -> Option<&Value> {
    op.get("parameters").filter(|v| !v.as_array().is_none_or(|a| a.is_empty()))
}
/// An operation's JSON request-body schema, if it declares one.
fn body(op: &Value) -> Option<&Value> {
    op.pointer("/requestBody/content/application~1json/schema")
}

/// Keep only what the derivation reads: query/path parameters, the JSON request
/// body, the router's catch-all marking, and the one line a command needs to say
/// what it does. Everything else (responses, security, examples) describes the API
/// to a human or an SDK, not a command line, and carrying it would bloat a
/// committed artifact nobody edits.
///
/// Two documents may describe one operation. `op` is the AUTHORITY and `alt` the
/// supplement, consulted only for a field the authority does not carry — the same
/// rule the schema namespace below already runs on, applied to the operation
/// instead of only to the `#/components` it points at. Where both speak, the shape
/// reflected from the Go type wins over the hand-written approximation this
/// pipeline exists to retire.
fn prune(op: &Value, alt: Option<&Value>, catch_all: Option<String>, summary: Option<String>) -> Value {
    let mut out = Map::new();
    if let Some(s) = summary.filter(|s| !s.is_empty()) {
        out.insert("summary".into(), json!(s));
    }
    if let Some(p) = catch_all {
        out.insert("x-catch-all".into(), json!([p]));
    }
    if let Some(p) = params(op).or_else(|| alt.and_then(params)) {
        out.insert("parameters".into(), p.clone());
    }
    if let Some(schema) = body(op).or_else(|| alt.and_then(body)) {
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
    registry: Vec<String>,
    openapi: PathBuf,
    out: PathBuf,
    check: bool,
}

fn args() -> Args {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let mut registry: Vec<String> = std::env::var("HANZO_REGISTRY").ok().into_iter().collect();
    let mut openapi = std::env::var("HANZO_OPENAPI_DIR").map(PathBuf::from).unwrap_or_else(|_| {
        manifest.parent().map(|p| p.join("openapi")).unwrap_or_else(|| PathBuf::from("../openapi"))
    });
    let mut out = manifest.join("spec/cloud.json");
    let mut check = false;

    let argv: Vec<String> = std::env::args().skip(1).collect();
    let mut i = 0;
    while i < argv.len() {
        let flag = argv[i].clone();
        if flag == "--check" {
            check = true;
            i += 1;
            continue;
        }
        let val = argv.get(i + 1).cloned().unwrap_or_else(|| panic!("{flag} needs a value"));
        match flag.as_str() {
            "--registry" => registry.push(val),
            "--openapi" => openapi = PathBuf::from(val),
            "--out" => out = PathBuf::from(val),
            other => panic!("usage: genspec [--registry <url|path>]… [--openapi <dir>] [--out <path>] [--check]\nunknown: {other}"),
        }
        i += 2;
    }
    if registry.is_empty() {
        registry.push(DEFAULT_REGISTRY.to_string());
    }
    Args { registry, openapi, out, check }
}

/// Read one route table: a URL is the normal case (the registry answers for
/// itself), a path is a checkout, a release's pinned `openapi.yaml`, or an
/// air-gapped run against a captured table.
///
/// JSON or YAML — the SAME document is served as `/v1/openapi.json` and
/// committed as `openapi.yaml`, and which encoding a reading arrives in says
/// nothing about what it means. Try JSON, fall back to YAML: cheap, and it is
/// what lets `HANZO_REGISTRY` name a git object at a release tag instead of a
/// live host.
async fn read_registry(src: &str) -> (Value, String) {
    let body = if src.starts_with("http") {
        reqwest::get(src)
            .await
            .and_then(|r| r.error_for_status())
            .unwrap_or_else(|e| panic!("GET {src}: {e}"))
            .text()
            .await
            .expect("read registry")
    } else {
        std::fs::read_to_string(src).unwrap_or_else(|e| panic!("read {src}: {e}"))
    };
    let digest = {
        use sha2::Digest;
        format!("{:x}", sha2::Sha256::digest(body.as_bytes()))
    };
    let doc = serde_json::from_str(&body)
        .or_else(|_| serde_norway::from_str(&body))
        .unwrap_or_else(|e| panic!("{src} is not the route table as JSON or YAML: {e}"));
    (doc, digest)
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
    let mut digests = Vec::new();
    for src in &a.registry {
        let (doc, digest) = read_registry(src).await;
        docs.push(doc);
        digests.push(digest);
    }
    let registry = if docs.len() == 1 { docs.remove(0) } else { union(docs) };
    let reg = Registry::read(&registry);

    // WHAT the document is, not WHERE this run happened to read it. A provenance
    // line naming a file path or a URL makes the artifact's bytes a function of
    // the reader — the same document read from a checkout and from the wire
    // produced two different specs, so `--check` could never be byte-exact — and
    // it cannot answer the question that matters: WHICH DEPLOY IS THIS? A digest
    // and a release ref answer both. The digest is computed here, over the bytes
    // actually read, so the artifact self-verifies; the ref comes from
    // HANZO_SPEC_REF, which is the one thing a reader cannot derive from bytes
    // (hanzoai/cloud's release sets it when it hands this repo its document).
    let spec_ref = std::env::var("HANZO_SPEC_REF").unwrap_or_else(|_| "unpinned".into());
    let provenance = format!("hanzoai/cloud@{spec_ref} sha256:{}", digests.join("+"));


    let master_path = a.openapi.join("hanzo.yaml");
    let master: Value = serde_norway::from_str(
        &std::fs::read_to_string(&master_path)
            .unwrap_or_else(|e| panic!("read {}: {e} — set --openapi <hanzoai/openapi checkout>", master_path.display())),
    )
    .expect("parse hanzo.yaml");

    // The master, indexed by the SAME address the document uses, so two readings
    // of one operation can be JOINED instead of chosen between. A `?query` path
    // key (S3-style sub-resource selectors) is not a distinct resource, and a
    // parameterised first segment is not a product.
    let authored: BTreeMap<(String, String), (&String, &Value)> = master
        .get("paths")
        .and_then(Value::as_object)
        .into_iter()
        .flatten()
        .filter(|(path, _)| {
            let s = segs(path);
            s.len() >= 2 && s[0] == "v1" && !is_param(s[1]) && !path.contains('?') && !path.contains('#')
        })
        .flat_map(|(path, item)| {
            let s = segs(path);
            item.as_object()
                .into_iter()
                .flatten()
                .filter(|(m, _)| VERBS.contains(&m.to_ascii_lowercase().as_str()))
                .map(move |(m, op)| ((m.to_ascii_uppercase(), norm(&s)), (path, op)))
        })
        .collect();

    let mut paths = Map::new();

    // WHAT IS SERVED. The document enumerates. Each of its operations exists, and
    // carries its own prose and its own shape; the master is joined in at the same
    // address to fill in a body or a parameter list the document has not typed yet.
    for ((method, key), (rpath, rop)) in &reg.ops {
        let s = segs(rpath);
        let pat: Vec<String> = s.iter().map(|x| x.to_string()).collect();
        let alt = authored.get(&(method.clone(), key.clone())).map(|(_, op)| *op);
        let summary = prose(rop).or_else(|| alt.and_then(prose));
        paths
            .entry(rpath.clone())
            .or_insert_with(|| Value::Object(Map::new()))
            .as_object_mut()
            .expect("path item")
            .insert(
                method.to_ascii_lowercase(),
                prune(rop, alt, Registry::catch_all(&pat, &s), summary),
            );
    }
    let served = reg.ops.len();

    // WHAT THE DOCUMENT DOES NOT DESCRIBE. An authored operation stands only where
    // the document is not the authority over it: a product it never mentions, or a
    // route it answers through a `/v1/<product>/*` door, which says the request
    // reaches the service and nothing about what the service does with it. Anywhere
    // else, silence from a product the document owns is a refusal.
    let (mut kept, mut refuted) = (0usize, BTreeMap::<String, usize>::new());
    for ((method, key), (path, op)) in &authored {
        if reg.ops.contains_key(&(method.clone(), key.clone())) {
            continue;
        }
        let s = segs(path);
        let route = reg.find(method, &s);
        // Only a product the document OWNS can be refuted at all.
        if route.is_none() && reg.owned.contains(s[1]) {
            *refuted.entry(s[1].to_string()).or_default() += 1;
            continue;
        }
        kept += 1;
        let catch_all = route.and_then(|r| Registry::catch_all(r, &s));
        paths
            .entry((*path).clone())
            .or_insert_with(|| Value::Object(Map::new()))
            .as_object_mut()
            .expect("path item")
            .insert(method.to_ascii_lowercase(), prune(op, None, catch_all, prose(op)));
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
                "Generated by `cargo run --features genspec --bin genspec`. The SOURCE is the Hanzo \
                 Cloud API document {provenance} — it enumerates the operations and carries their \
                 reflected shapes. The authored master hanzoai/openapi hanzo.yaml supplements it: \
                 joined at the same address for a body or parameter the document has not typed, and \
                 read on its own only where the document is not the authority (a product it never \
                 mentions, or a route it answers through a catch-all door). Never hand-edited: \
                 `genproduct` derives src/commands/product/generated.rs from this and nothing else."
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

    // FRESHNESS OF THE PROJECTION — the same generation, asked of the artifact
    // instead of written over it. `spec/cloud.json` is a projection of one
    // document; re-projecting that document and finding different bytes means
    // the committed capture is not what its inputs say it is, and every command
    // `genproduct` derives from it inherits the lie.
    //
    // This is D1's gate and it is why nothing is written here: a check that
    // repairs what it measures reports success for a tree that was wrong when
    // the job started. It reads the document from `HANZO_REGISTRY` (the
    // client: lane sets it to `openapi.yaml` at the release's sha), so it asks
    // about a PINNED document — which is a different question from the one
    // `driftgate` asks, and both are worth asking. Provenance says where the spec
    // came from; --check says the spec still equals its inputs; `driftgate` says
    // the running server still serves it, and still commands everything it serves.
    if a.check {
        let have = std::fs::read(&a.out).unwrap_or_else(|e| panic!("read {}: {e}", a.out.display()));
        if have == bytes {
            eprintln!(
                "genspec --check: {} is current with {} ({} bytes)",
                a.out.display(),
                a.registry.join(" ∪ "),
                bytes.len()
            );
            return;
        }
        let ops = |v: &Value| -> BTreeSet<(String, String)> {
            let mut s = BTreeSet::new();
            for (p, item) in v.get("paths").and_then(Value::as_object).into_iter().flatten() {
                for m in item.as_object().into_iter().flatten().map(|(m, _)| m) {
                    s.insert((m.to_ascii_uppercase(), p.clone()));
                }
            }
            s
        };
        let now = ops(&doc);
        let then = serde_json::from_slice::<Value>(&have).map(|v| ops(&v)).unwrap_or_default();
        for (m, p) in now.difference(&then).take(20) {
            eprintln!("  + {m} {p}");
        }
        for (m, p) in then.difference(&now).take(20) {
            eprintln!("  - {m} {p}");
        }
        eprintln!(
            "genspec --check: STALE — {} would gain {} operations and lose {} ({} bytes vs {})",
            a.out.display(),
            now.difference(&then).count(),
            then.difference(&now).count(),
            bytes.len(),
            have.len(),
        );
        eprintln!("Regenerate: genspec, then genproduct, and commit both.");
        std::process::exit(1);
    }

    std::fs::write(&a.out, &bytes).unwrap_or_else(|e| panic!("write {}: {e}", a.out.display()));

    let total: usize = refuted.values().sum();
    let mut worst: Vec<_> = refuted.iter().collect();
    worst.sort_by_key(|(p, n)| (std::cmp::Reverse(**n), (*p).clone()));
    eprintln!(
        "genspec: {served} served by the document, {kept} authored kept (unowned products and \
         catch-all doors), {total} refuted ({} owned products) -> {} ({} bytes)",
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
