//! `genspec` — build `spec/cloud.json`, the ONE spec `genproduct` derives the
//! cloud command surface from. This is the refresh seam: run it to pull the
//! surface forward, commit the result, then run `genproduct`.
//!
//! ONE INPUT. ONE AUTHORITY. There is no second document.
//!
//! The input is hanzoai/cloud's `openapi.yaml` at a release ref — the document
//! cloud EMITS from its own code. Every operation in it is an operation the CLI
//! has; every operation it lacks is an operation the CLI does not have. Prose,
//! request bodies, query parameters and the catch-all marking all come from that
//! one reading, because zip reflects them off the live Go types and zipdoc lifts
//! the handler's doc comment. Nothing is joined in, supplemented, patched or
//! excepted.
//!
//! THAT IS THE WHOLE POINT, and it is what makes a phantom command STRUCTURALLY
//! IMPOSSIBLE rather than merely rare. A phantom is a command addressing a route
//! no code registers. It can only exist if something other than the code gets to
//! say what exists. Until 1.9.34 something did: the hand-authored master
//! hanzoai/openapi `hanzo.yaml` was read on its own wherever the document was
//! held not to be "the authority" — a product it never mentioned, or a route it
//! answered through a `/v1/<product>/*` door — and 71 operations entered
//! `spec/cloud.json` that way. Two of them addressed nothing at all. Sixty-six of
//! the remaining sixty-nine "matched" only a `{wildcardN}` door, which answers
//! identically for a real path and an invented one, so their evidence was
//! unfalsifiable by construction. A door is not a list.
//!
//! WHAT THE DOCUMENT IS, precisely, because the rule rests on it: the weave of
//! the per-app subsets each app binary emits FROM ITS OWN ROUTER (`<app>
//! openapi`, embedded by `plugin/embed.go`), not a read of the serving process's
//! router. The light host mounts no subsystem, so it cannot read one. That makes
//! the document a projection of the routers AT BUILD TIME — true of the binary
//! that was released, and true of production because hanzoai/cloud's release gate
//! regenerates every subset from source and refuses to ship a tree where they
//! disagree (`make -f mk/fleet.mk surface-check`). Measured 2026-08-03: the
//! document at tag v1.801.383 and `api.hanzo.ai/v1/openapi.json` carry the
//! IDENTICAL 2333 operations, zero either way. The artifact and the deploy are
//! the same thing.
//!
//! BY VALUE, AT A REF — never a live host. `--registry <url|path>` names the
//! document; `.spec-lock` records which ref and digest it was, and
//! `HANZO_SPEC_REF` carries the ref into the artifact's provenance. There is no
//! default, because the obvious default is the trap: `api.hanzo.ai` names a HOST,
//! a host cannot say which deploy it was, and a capture built from one keeps
//! claiming it forever while the router moves underneath. (The near-miss is
//! worse than the miss — `api.hanzo.ai/openapi.json`, without the `/v1/`, answers
//! 200 with 367126 bytes of the console's HTML.)
//!
//! ONE READING, not a union. `--registry` used to be repeatable so a mid-rollout
//! route could survive being refuted by a wire that had not caught up. Refutation
//! is gone — nothing proposes an operation for the document to refute — so the
//! union went with it. Needing a newer route means re-pinning `.spec-lock`, which
//! is one line and leaves a record.
//!
//! WHAT A DOOR COSTS, counted every run. A `/v1/<product>/{wildcardN}` route is
//! the document saying "a subtree is relayed through here" without naming what is
//! behind it. Those are the only places a generated CLI cannot reach a served
//! operation, and they are exactly where the deleted master was enumerating. So
//! the count is REPORTED and PINNED as a ceiling (`DOORS`): free to fall as
//! cloud types those relays, and it cannot rise without somebody saying why. A
//! counted boundary derived from the document beats a 3.5MB hand file describing
//! what might be behind it.
//!
//! Two questions, and a capture has to survive both:
//!
//!   `--check`  IS THE SPEC STILL ITS INPUT? Re-runs the whole generation against
//!     the PINNED document and refuses if the bytes differ from `--out`. This is
//!     the gate that fires when cloud ships. Nothing is written — a check that
//!     repairs what it measures reports success for a tree that was wrong when
//!     the job started.
//!
//!   IS THE SPEC STILL TRUE OF PRODUCTION? is the OTHER question, asked by
//!     `src/bin/driftgate.rs` (`make verify`), which holds the shipped surface
//!     and the running host against each other in both directions.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use serde_json::{json, Map, Value};

const VERBS: [&str; 5] = ["get", "post", "put", "patch", "delete"];

/// How many `/v1/<product>/{wildcardN}` relay doors the document carries — the
/// addresses where it declares a subtree reachable without naming what is in it.
/// A CEILING: free to fall as cloud types those relays into ops, and it may not
/// rise without a person saying why in the commit. This is the ONLY place the
/// CLI's surface is knowably short of what cloud serves, and it is a number
/// derived from the document rather than a parallel file guessing at the
/// contents.
///
/// 11 -> 7 on the document this tree pins: cloud typed `exec`, `files`,
/// `licensing` and `upload` into real operations. The ceiling did not follow them
/// down, so for four releases it admitted four doors that no longer existed — and
/// a ceiling with slack in it is not a ceiling, it is a number that would have
/// let four NEW relays appear without a word. Today: bot collections dns download
/// sbom sentry tasks.
const DOORS: usize = 7;

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

/// The document's operations, its schemas, its per-product prose — and the doors
/// it admits to. Nothing else is read, because nothing else is asked.
struct Registry {
    /// (METHOD, the document's own path key) -> the operation object. EVERY
    /// operation the document carries: existence is the router's answer, lifted
    /// once, and whether an operation also has a sentence to its name is a
    /// separate question asked by whoever projects it. (`genproduct` refuses to
    /// build a command with nothing to say — see its bare-summary assert.)
    ops: BTreeMap<(String, String), Value>,
    /// The document's own component schemas — the shapes zip reflected from the
    /// live Go types, which the kept operations' `$ref`s resolve against.
    schemas: Map<String, Value>,
    /// Per-product prose from the document's `tags` — each entry's description is
    /// the owning Go package's doc synopsis, lifted by the same weave that writes
    /// the subsets. Products, like operations, describe themselves.
    products: BTreeMap<String, String>,
    /// `/v1/<product>/{wildcardN}` — a whole subtree relayed elsewhere. Counted,
    /// never enumerated: what is behind a door is the mounted service's to
    /// publish, and guessing at it is what the deleted master was for.
    doors: BTreeSet<String>,
}

/// Does the serving binary call this address a legacy spelling of another one?
///
/// SERVED and PUBLISHED are different questions, and this is the only place the
/// answers differ. `/v1/iam/get-users` and `/v1/iam/users` are two live routes on
/// ONE handler; nothing in a route table relates them, so hanzoai/iam tags the
/// older spelling `compat` and hanzoai/cloud's weave carries the tag out.
///
/// A compat op is REAL — it is served, and a caller using it gets an answer — but
/// it does not become a command. Publishing both would put two spellings of one
/// operation in `hanzo iam --help`, with nothing telling a customer which to use;
/// the whole point of the tag is that somebody already decided.
fn is_compat(op: &Value) -> bool {
    op.get("tags")
        .and_then(Value::as_array)
        .is_some_and(|t| t.iter().any(|x| x.as_str() == Some("compat")))
}

/// The one-line prose of an op: its summary, else the first line of its
/// description. Empty means the op is undescribed — a missing Go doc comment in
/// hanzoai/cloud, which `genproduct` refuses to paper over.
fn prose(op: &Value) -> Option<String> {
    let take = |v: &Value| v.as_str().map(|x| x.lines().next().unwrap_or("").trim().to_string());
    op.get("summary")
        .and_then(take)
        .filter(|x| !x.is_empty())
        .or_else(|| op.get("description").and_then(take).filter(|x| !x.is_empty()))
}

impl Registry {
    fn read(doc: &Value) -> Self {
        let mut ops: BTreeMap<(String, String), Value> = BTreeMap::new();
        let mut doors = BTreeSet::new();
        for (path, item) in doc.get("paths").and_then(Value::as_object).into_iter().flatten() {
            let s = segs(path);
            // The bare `/v1/*` is the global fallthrough; a parameterised first
            // segment is that fallthrough wearing a longer path. Neither names a
            // product, so nothing under either is an operation OF one.
            if s.len() < 2 || s[0] != "v1" || is_param(s[1]) {
                continue;
            }
            if s.len() == 3 && is_wild(s[2]) {
                doors.insert(s[1].to_string());
            }
            for (m, op) in item.as_object().into_iter().flatten() {
                if VERBS.contains(&m.to_ascii_lowercase().as_str()) && !is_compat(op) {
                    ops.insert((m.to_ascii_uppercase(), path.clone()), op.clone());
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
        Registry { ops, schemas, products, doors }
    }
}

/// The multi-segment path parameter a fiber `*` stands in for — a value whose
/// slashes are STRUCTURAL (a KMS secret is `sub/path/name`, an S3 key is
/// `a/b/c.txt`). OpenAPI cannot say this, but the route pattern does, so the
/// runtime keeps those slashes raw instead of `%2F`-escaping them into one
/// opaque segment the backend 404s.
///
/// It counts only where the catch-all is a PARAMETER: last segment, and deeper
/// than `/v1/<product>/*`. A product-level catch-all is a DOOR — a whole subtree
/// proxied elsewhere — and reading it as a parameter would send raw slashes into
/// every id in the subtree. Same predicate, two readings, one place: what is not
/// a parameter here is counted as a door in `Registry::read`.
fn catch_all(s: &[&str]) -> Option<String> {
    let w = s.len() - 1;
    (w > 2 && is_wild(s[w])).then(|| s[w].trim_start_matches('{').trim_end_matches('}').to_string())
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
fn prune(op: &Value, rest: Option<String>, summary: Option<String>) -> Value {
    let mut out = Map::new();
    if let Some(s) = summary.filter(|s| !s.is_empty()) {
        out.insert("summary".into(), json!(s));
    }
    if let Some(p) = rest {
        out.insert("x-catch-all".into(), json!([p]));
    }
    if let Some(p) = params(op) {
        out.insert("parameters".into(), p.clone());
    }
    if let Some(schema) = body(op) {
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
    out: PathBuf,
    check: bool,
}

fn args() -> Args {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let mut registry = std::env::var("HANZO_REGISTRY").ok();
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
            "--registry" => registry = Some(val),
            "--out" => out = PathBuf::from(val),
            other => panic!(
                "usage: genspec --registry <url|path> [--out <path>] [--check]\nunknown: {other}"
            ),
        }
        i += 2;
    }
    // NO DEFAULT. A default would have to name a host, and a host cannot say
    // which deploy it was — every stale capture ever committed satisfied
    // "provenance names the wire". The document is passed BY VALUE at the ref
    // `.spec-lock` records; `make spec` fetches it and sets HANZO_REGISTRY.
    let registry = registry.unwrap_or_else(|| {
        panic!(
            "genspec: no document. Pass --registry <url|path> or set HANZO_REGISTRY to \
             hanzoai/cloud's openapi.yaml AT A RELEASE REF — `make spec` does both from .spec-lock. \
             There is deliberately no default: api.hanzo.ai names a host, and a host cannot say \
             which deploy a capture describes."
        )
    });
    Args { registry, out, check }
}

/// Read the document: a path is the normal case (a release's `openapi.yaml`,
/// fetched by value), a URL is for an experiment.
///
/// JSON or YAML — the SAME document is served as `/v1/openapi.json` and committed
/// as `openapi.yaml`, and which encoding a reading arrives in says nothing about
/// what it means.
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
    let doc: Value = serde_json::from_str(&body)
        .or_else(|_| serde_norway::from_str(&body))
        .unwrap_or_else(|e| panic!("{src} is not the route table as JSON or YAML: {e}"));
    // `api.hanzo.ai/openapi.json` — the same URL without `/v1/` — answers 200
    // with the console's HTML, and an HTML page parses as neither JSON nor YAML
    // often enough to be caught above, but not always. Assert the one key that
    // makes a document a document.
    assert!(
        doc.get("openapi").is_some() && doc.get("paths").is_some(),
        "{src} parsed, but carries no `openapi:` and no `paths:` — that is not the API document. \
         (`api.hanzo.ai/openapi.json`, without the `/v1/`, answers 200 with the console's HTML.)"
    );
    (doc, digest)
}

#[tokio::main]
async fn main() {
    let a = args();
    let (registry, digest) = read_registry(&a.registry).await;
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
    // THE REPO IS READ, NOT SPELLED. It was the literal `hanzoai/cloud` here,
    // which made this a fourth place that knows which cloud the document comes
    // from -- beside .spec-lock, the Makefile and hanzo.yml -- and the one place
    // nobody thought to change when the product line moved to hanzo-inc/cloud.
    // The lock then said hanzo-inc and the capture it describes said hanzoai, so
    // the drift test compared a repo against itself and passed while the two
    // disagreed.
    let spec_repo = std::env::var("HANZO_SPEC_REPO").unwrap_or_else(|_| "hanzo-inc/cloud".into());
    let provenance = format!("{spec_repo}@{spec_ref} sha256:{digest}");

    // THE WHOLE DERIVATION. The document enumerates; each of its operations
    // exists, carries its own prose and its own reflected shape, and is kept.
    // There is no second loop, because there is no second source.
    let mut paths = Map::new();
    for ((method, path), op) in &reg.ops {
        let s = segs(path);
        paths
            .entry(path.clone())
            .or_insert_with(|| Value::Object(Map::new()))
            .as_object_mut()
            .expect("path item")
            .insert(method.to_ascii_lowercase(), prune(op, catch_all(&s), prose(op)));
    }

    let keep = reachable(&paths, &reg.schemas);
    let components: Map<String, Value> =
        keep.iter().filter_map(|n| reg.schemas.get(n).map(|s| (n.clone(), s.clone()))).collect();

    // The OpenAPI-native place for per-product prose: a `tags` entry per product
    // that actually has operations in this spec AND a description in the
    // document. `genproduct` turns these into the product groups' help lines.
    let present: BTreeSet<&str> = paths.keys().filter_map(|p| segs(p).get(1).copied()).collect();
    let tags: Vec<Value> = reg
        .products
        .iter()
        .filter(|(n, _)| present.contains(n.as_str()))
        .map(|(n, d)| json!({"name": n, "description": d}))
        .collect();

    let mut doc = json!({
        "openapi": "3.1.0",
        "info": {
            "title": "Hanzo Cloud — the CLI's command surface",
            "version": registry.pointer("/info/version").and_then(Value::as_str).unwrap_or("unknown"),
            "description": format!(
                "Generated by `cargo run --features genspec --bin genspec`. The SOURCE is the Hanzo \
                 Cloud API document {provenance}, and it is the ONLY source: it enumerates the \
                 operations, and carries their prose and their reflected shapes. Nothing is joined \
                 in — an operation absent here is an operation no cloud router registers, which is \
                 what makes a phantom command impossible rather than rare. Never hand-edited: \
                 `genproduct` derives src/commands/product/generated.rs from this and nothing else."
            ),
        },
        "paths": paths,
        "components": {"schemas": components},
    });
    if !tags.is_empty() {
        doc.as_object_mut().expect("doc").insert("tags".into(), json!(tags));
    }

    let bytes = serde_json::to_vec(&doc).expect("encode");

    // FRESHNESS OF THE PROJECTION — the same generation, asked of the artifact
    // instead of written over it. `spec/cloud.json` is a projection of one
    // document; re-projecting that document and finding different bytes means the
    // committed capture is not what its input says it is, and every command
    // `genproduct` derives from it inherits the lie. Nothing is written: a check
    // that repairs what it measures reports success for a tree that was wrong
    // when the job started.
    // THE DOOR CENSUS. Printed every run — BOTH runs. It is a fact about the
    // DOCUMENT, so it belongs before the write-or-compare branch: sitting after
    // it, the ceiling was only ever reached by a regeneration, and the gate CI
    // actually runs on every push (`--check`) returned above it. The one thing a
    // person needs to know about a surface derived from one document is where
    // that document stops being able to enumerate. Each of these is a served
    // subtree the CLI cannot reach past, and the fix is upstream in every case:
    // type the relay's routes as zip ops, or have the mounted service publish its
    // own document for the weave to carry.
    eprintln!(
        "genspec: {} relay door(s) — /v1/<product>/* subtrees the document declares without \
         naming what is behind them, so no command can reach past them: {}",
        reg.doors.len(),
        reg.doors.iter().cloned().collect::<Vec<_>>().join(" ")
    );
    assert!(
        reg.doors.len() <= DOORS,
        "THE DOOR CEILING ROSE: the document now relays {} product subtrees through a catch-all and \
         DOORS in src/bin/genspec.rs says {DOORS}. New door(s): {}\n\n\
         A door is the one place this pipeline knowingly under-reaches: the request gets through and \
         nothing here can name the operation. Type the relay's routes in hanzoai/cloud, or — if the \
         subtree really must stay opaque — raise DOORS with the reason in the commit. Do NOT re-add \
         a hand-authored file enumerating what is behind it; that is the second authority this \
         generator was rebuilt to delete.",
        reg.doors.len(),
        reg.doors.iter().cloned().collect::<Vec<_>>().join(" "),
    );

    if a.check {
        let have = std::fs::read(&a.out).unwrap_or_else(|e| panic!("read {}: {e}", a.out.display()));
        if have == bytes {
            eprintln!(
                "genspec --check: {} is current with {} ({} bytes)",
                a.out.display(),
                a.registry,
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

    let products = present.len();
    eprintln!(
        "genspec: {} operations across {products} products, all of them the document's -> {} ({} bytes)",
        reg.ops.len(),
        a.out.display(),
        bytes.len()
    );

}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// SERVED and PUBLISHED are different questions, and `Registry::read` is the
    /// only place the answers may differ: a legacy address is real, and answers,
    /// but must not become a second spelling of one command in `--help`.
    #[test]
    fn a_compat_address_never_becomes_a_command() {
        let doc = json!({"openapi": "3.1.0", "paths": {
            "/v1/iam/users":     {"get": {"summary": "List users", "tags": ["iam"]}},
            "/v1/iam/get-users": {"get": {"summary": "List users (legacy verb)", "tags": ["iam", "compat"]}},
        }});
        let reg = Registry::read(&doc);
        let commands: Vec<&String> = reg.ops.keys().map(|(_, p)| p).collect();
        assert_eq!(commands, vec!["/v1/iam/users"], "the legacy spelling became a command");
    }

    /// The tag is read off the operation, not guessed from the path shape: a
    /// verb-noun address with no declaration is somebody's real surface, and
    /// dropping it on looks alone would silently delete a command.
    #[test]
    fn only_a_declaration_makes_an_address_compat() {
        assert!(is_compat(&json!({"tags": ["iam", "compat"]})));
        assert!(!is_compat(&json!({"tags": ["iam"]})));
        assert!(!is_compat(&json!({})));
    }

    /// A PHANTOM IS STRUCTURALLY IMPOSSIBLE — the property this binary exists for,
    /// stated as a test rather than as a paragraph. Whatever the document carries
    /// is what comes out; there is no other way in. A second source could only
    /// enter through `main`, and `main` reads one.
    #[test]
    fn every_operation_out_is_an_operation_in() {
        let doc = json!({"openapi": "3.1.0", "paths": {
            "/v1/kv/{key}": {"get": {"summary": "Read one key", "tags": ["kv"]}},
        }});
        let reg = Registry::read(&doc);
        assert_eq!(reg.ops.len(), 1);
        assert!(reg.ops.contains_key(&("GET".into(), "/v1/kv/{key}".into())));
    }

    /// One predicate, two readings, and the boundary between them is `w > 2`. A
    /// product-level catch-all is a DOOR — counted, never treated as a parameter,
    /// because reading it as one would send raw slashes into every id behind it.
    /// Deeper, it is a multi-segment PARAMETER and the runtime must not escape it.
    #[test]
    fn a_product_catch_all_is_a_door_and_a_deeper_one_is_a_parameter() {
        let doc = json!({"openapi": "3.1.0", "paths": {
            "/v1/dns/{wildcard1}":              {"get": {"summary": "Relay", "tags": ["dns"]}},
            "/v1/kms/secrets/{wildcard1}":      {"get": {"summary": "Read one secret", "tags": ["kms"]}},
        }});
        let reg = Registry::read(&doc);
        assert_eq!(reg.doors.iter().cloned().collect::<Vec<_>>(), vec!["dns".to_string()]);
        assert_eq!(catch_all(&segs("/v1/dns/{wildcard1}")), None);
        assert_eq!(catch_all(&segs("/v1/kms/secrets/{wildcard1}")), Some("wildcard1".into()));
    }

    /// The global fallthrough is not a product, and neither is it wearing a longer
    /// path. Counting either as evidence would make every conceivable address an
    /// operation of some product.
    #[test]
    fn the_global_fallthrough_is_not_a_product() {
        let doc = json!({"openapi": "3.1.0", "paths": {
            "/v1/{wildcard1}":          {"get": {"summary": "Fallthrough", "tags": ["ai"]}},
            "/v1/{proxy}/anything":     {"get": {"summary": "Also fallthrough", "tags": ["ai"]}},
            "/v1/kv":                   {"get": {"summary": "List keys", "tags": ["kv"]}},
        }});
        let reg = Registry::read(&doc);
        assert_eq!(reg.ops.len(), 1, "the fallthrough became a command: {:?}", reg.ops.keys());
        assert!(reg.doors.is_empty());
    }
}
