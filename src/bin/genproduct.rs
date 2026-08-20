//! `genproduct` — regenerate `src/commands/product/generated.rs` from the
//! COMMITTED, hand-authored OpenAPI snapshot. Run by hand when the snapshot
//! changes; the output is checked in, so `hanzo` never fetches a spec at runtime.
//!
//! Source of truth: `spec/products.json` — the per-product OpenAPI 3.1 specs
//! (repo hanzoai/openapi) vendored as one JSON object keyed by product. It carries
//! real requestBody schemas AND typed `parameters`, so a write op becomes TYPED
//! body `--flags` and a query parameter becomes a TYPED query `--flag`, not
//! `--data`.
//!
//! The fold from path → (product, resource nodes, verb, params) is TOTAL; typed
//! fields resolve $ref → component schema → property names + types + required.
//! It emits pure DATA: no host, no URL, no auth. See `commands::product`.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::PathBuf;

use serde_json::Value;

const VERBS: [&str; 5] = ["get", "post", "put", "patch", "delete"];
/// Local commands own these bare names; the generated tree never claims them.
const EXCLUDE: [&str; 3] = ["billing", "agent", "deploy"];
/// Curation — products NOT emitted as top-level commands. Reviewed by hand.
const DENY: &[&str] = &[
    // Noise: sub-operations, UI/config surfaces, or enumeration artifacts — not
    // first-class products a person reaches for.
    "download", "upload", "files", "completions", "console", "settings",
    "search-docs", "index-docs", "chat-docs", "indexers", "embed-status",
    "csrf", "openapi.json", "account-bridge", "agent-bindings",
    // Singular/plural dedupe: the LOCAL hand-written command owns the singular
    // (`network` = network selection, `cluster` = talk-to-a-node), and `bot` is
    // the canonical cloud product — so the redundant cloud PLURALS are dropped.
    "networks", "clusters", "bots",
    // Internal control planes, not user commands: `provisioning` is the internal
    // provisioner (you provision via the concrete `hanzo vector|kv|s3 create`),
    // and `do` is the DigitalOcean PROVIDER backend.
    "provisioning", "do",
    // `gateway` is aspirational: the whole `/v1/gateway/*` subtree is unmounted
    // (404 live). The real gateway surface is TOP-LEVEL — `/v1/models`,
    // `/v1/chat/completions`, `/v1/embeddings` — already reached as `hanzo models`,
    // `hanzo chat completions`, `hanzo embeddings`. Shipping a command group the
    // server cannot answer is worse than no verb, so it is dropped until the
    // openapi authors the routes that are actually served.
    "gateway",
];
/// Curation — absorb a product's ops UNDER another command as a sub-namespace, so
/// the compute plane is ONE `hanzo compute` (machines + gpus + regions/sizes)
/// instead of three top-levels. `machines`/`gpus` live at their own path prefixes
/// with a colliding `get`, so a FLAT `compute list` is impossible without
/// ambiguity — sub-namespacing unifies them losslessly. A flat surface would need
/// the cloud specs reorganized under one `/v1/compute` tag.
const REMAP: &[(&str, &str)] = &[("machines", "compute"), ("gpus", "compute")];
/// Curation — path parameters that address a MULTI-SEGMENT path (a server
/// catch-all), keyed by `(product, param)`. Their value is a `/`-joined address
/// (a KMS secret is `sub/path/name`), so the runtime keeps the slashes raw
/// (encoding each segment) instead of `%2F`-escaping them into one opaque segment
/// the backend 404s. This is knowledge the OpenAPI does not carry, so it lives
/// here beside the other curation tables — not in the vendored spec. Everything
/// else is single-segment, the route-confusion-safe default.
const REST_PARAMS: &[(&str, &str)] = &[("kms", "secret")];
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
fn segs(p: &str) -> Vec<&str> {
    p.split('/').filter(|s| !s.is_empty()).collect()
}

/// camelCase / snake_case → kebab-case for a flag long name.
fn kebab(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 4);
    let mut prev_lower = false;
    for c in s.chars() {
        if c == '_' || c == ' ' {
            if !out.is_empty() && !out.ends_with('-') {
                out.push('-');
            }
            prev_lower = false;
        } else if c.is_ascii_uppercase() {
            if prev_lower {
                out.push('-');
            }
            out.push(c.to_ascii_lowercase());
            prev_lower = false;
        } else {
            out.push(c);
            prev_lower = c.is_ascii_lowercase() || c.is_ascii_digit();
        }
    }
    out
}

// ---- the total fold ---------------------------------------------------------

fn has_child(p: &str, all: &BTreeSet<String>) -> bool {
    let pre = format!("{}/", p.trim_end_matches('/'));
    all.iter().any(|k| k != p && k.trim_end_matches('/').starts_with(&pre))
}
fn is_collection(p: &str, all: &BTreeSet<String>) -> bool {
    let ps = segs(p);
    all.iter().any(|k| {
        let ks = segs(k);
        ks.len() == ps.len() + 1 && ks[..ps.len()] == ps[..] && is_param(ks[ks.len() - 1])
    })
}
fn cmd_tokens(sg: &[&str]) -> Vec<String> {
    let mut out = Vec::new();
    let mut j = 1usize;
    if sg[0] == "orgs" && sg.len() > 1 && is_param(sg[1]) {
        j = 2;
    }
    while j < sg.len() {
        if sg[j] == "orgs" && j + 1 < sg.len() && is_param(sg[j + 1]) {
            j += 2;
            continue;
        }
        out.push(sg[j].to_string());
        j += 1;
    }
    out
}
/// The collection-root verb: distinct writes (`clear`/`replace`) so a collection
/// op never clashes with the item op's `rm`/`update`.
fn root_verb(method: &str, coll: bool) -> &'static str {
    match method {
        "GET" => {
            if coll {
                "list"
            } else {
                "get"
            }
        }
        "POST" => "create",
        "PUT" | "PATCH" => "replace",
        _ => "clear",
    }
}

struct Folded {
    product: String,
    nodes: Vec<String>,
    verb: String,
    params: Vec<String>,
}

fn fold(method: &str, path: &str, all: &BTreeSet<String>) -> Option<Folded> {
    let sg = segs(path);
    let sg = &sg[1..]; // drop v1
    if sg.is_empty() || is_wild(sg[0]) {
        return None;
    }
    let product = sg[0].to_string();
    let ct = cmd_tokens(sg);
    let params: Vec<String> = ct.iter().filter(|s| is_param(s)).map(|s| pname(s).to_string()).collect();
    let p = path.trim_end_matches('/').to_string();
    if ct.is_empty() {
        return Some(Folded {
            product,
            nodes: vec![],
            verb: root_verb(method, is_collection(&p, all)).into(),
            params,
        });
    }
    let mut nodes = Vec::new();
    for i in 0..ct.len() {
        let tok = &ct[i];
        let terminal = i == ct.len() - 1;
        if is_param(tok) {
            if !terminal && ct.get(i + 1).map(|n| is_param(n)).unwrap_or(false) {
                nodes.push(pname(tok).to_string());
            }
            continue;
        }
        if terminal {
            break;
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
    } else if has_child(&p, all) {
        let v = root_verb(method, is_collection(&p, all));
        nodes.push(last.clone());
        v.into()
    } else {
        last.clone()
    };
    Some(Folded { product, nodes, verb, params })
}

// ---- typed field extraction -------------------------------------------------

#[derive(Clone)]
struct FieldDef {
    key: String,
    flag: String,
    ty: &'static str, // Str|Int|Num|Bool|Json
    required: bool,
    choices: Vec<String>,
    /// A query-string parameter (goes in the URL), vs a requestBody property.
    query: bool,
    /// A SECRET body value (`format: password`): read from stdin, NEVER a flag —
    /// so it can never land in argv, `ps` or shell history. The ONE stdin-secret
    /// marker; the runtime reads it through `iam::secret::read_secret`.
    secret: bool,
}

/// A body property that is a SECRET VALUE. The marker is the standard OpenAPI
/// `format: password` — the one signal "this input is a secret", honored
/// uniformly across the whole product surface (today: `kms secrets create`).
fn is_secret(pschema: &Value) -> bool {
    pschema.get("format").and_then(Value::as_str) == Some("password")
}

fn deref<'a>(spec: &'a Value, v: &'a Value) -> &'a Value {
    if let Some(r) = v.get("$ref").and_then(Value::as_str) {
        let mut node = spec;
        for part in r.trim_start_matches("#/").split('/') {
            match node.get(part) {
                Some(n) => node = n,
                None => return v,
            }
        }
        return node;
    }
    v
}

/// The JSON body schema for a write op, or None (no requestBody).
fn body_schema<'a>(spec: &'a Value, op: &'a Value) -> Option<&'a Value> {
    let rb = deref(spec, op.get("requestBody")?);
    rb.get("content")?.get("application/json")?.get("schema")
}

/// Map a property/parameter schema to a clap type + enum choices. Shared by the
/// requestBody-property and query-parameter paths — one classification rule.
fn classify(spec: &Value, pschema: &Value) -> (&'static str, Vec<String>) {
    let is_ref = pschema.get("$ref").is_some();
    let d = deref(spec, pschema);
    let enum_vals: Vec<String> = d
        .get("enum")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect())
        .unwrap_or_default();
    let t = d.get("type").and_then(Value::as_str).unwrap_or("");
    if is_ref {
        ("Json", vec![])
    } else if t == "string" && !enum_vals.is_empty() {
        ("Str", enum_vals)
    } else {
        match t {
            "string" => ("Str", vec![]),
            "integer" => ("Int", vec![]),
            "number" => ("Num", vec![]),
            "boolean" => ("Bool", vec![]),
            "array" | "object" => ("Json", vec![]),
            _ if d.get("properties").is_some() => ("Json", vec![]),
            _ => ("Str", vec![]),
        }
    }
}

/// Resolve a body schema into typed fields, or an empty vec for a freeform /
/// non-object body (→ `--data` fallback). Faithful to the schema — no invention.
fn fields_of(spec: &Value, schema: &Value) -> Vec<FieldDef> {
    let s = deref(spec, schema);
    let mut props: Vec<(String, Value)> = Vec::new();
    let mut required: BTreeSet<String> = BTreeSet::new();
    let mut collect = |obj: &Value| {
        if let Some(r) = obj.get("required").and_then(Value::as_array) {
            for v in r {
                if let Some(n) = v.as_str() {
                    required.insert(n.to_string());
                }
            }
        }
        if let Some(p) = obj.get("properties").and_then(Value::as_object) {
            for (k, v) in p {
                props.push((k.clone(), v.clone()));
            }
        }
    };
    if let Some(all) = s.get("allOf").and_then(Value::as_array) {
        for sub in all {
            collect(deref(spec, sub));
        }
    } else {
        collect(s);
    }
    props
        .into_iter()
        .map(|(name, pschema)| {
            let (ty, choices) = classify(spec, &pschema);
            let required = required.contains(&name);
            let secret = is_secret(deref(spec, &pschema));
            FieldDef { flag: kebab(&name), key: name, ty, required, choices, query: false, secret }
        })
        .collect()
}

/// Typed flags from an operation's `parameters` array: the `in: query` params
/// become query `--flags` (`in: path` params are already positionals from the
/// path template, so they are skipped here). $ref params resolve to their shared
/// definition. Applies to reads AND writes.
fn query_fields(spec: &Value, op: &Value) -> Vec<FieldDef> {
    let Some(params) = op.get("parameters").and_then(Value::as_array) else {
        return vec![];
    };
    let mut out = Vec::new();
    for p in params {
        let p = deref(spec, p);
        if p.get("in").and_then(Value::as_str) != Some("query") {
            continue;
        }
        let Some(name) = p.get("name").and_then(Value::as_str) else { continue };
        let required = p.get("required").and_then(Value::as_bool).unwrap_or(false);
        let (ty, choices) = match p.get("schema") {
            Some(schema) => classify(spec, schema),
            None => ("Str", vec![]),
        };
        out.push(FieldDef {
            flag: kebab(name),
            key: name.to_string(),
            ty,
            required,
            choices,
            query: true,
            // A query parameter rides the URL; a secret must never do that, so a
            // query field is never a stdin-secret.
            secret: false,
        });
    }
    out
}

// ---- collect + resolve collisions + emit ------------------------------------

struct Op {
    product: String,
    nodes: Vec<String>,
    verb: String,
    method: String,
    path: String,
    params: Vec<String>,
    /// The subset of `params` that are multi-segment (catch-all) — see REST_PARAMS.
    rest: Vec<String>,
    fields: Vec<FieldDef>,
}

fn method_rank(m: &str) -> usize {
    METHOD_PRIORITY.iter().position(|x| *x == m).unwrap_or(usize::MAX)
}

fn main() {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let products: Value =
        serde_json::from_str(&fs::read_to_string(manifest.join("spec/products.json")).unwrap()).unwrap();

    // Global authored path universe (product = first path segment, not the dir).
    let mut all: BTreeSet<String> = BTreeSet::new();
    for (_dir, spec) in products.as_object().unwrap() {
        for (p, _item) in spec.get("paths").and_then(Value::as_object).into_iter().flatten() {
            if p.starts_with("/v1/") {
                all.insert(p.clone());
            }
        }
    }

    // Fold every authored op; carry the (dir spec, op) so $ref resolves locally.
    let mut raw: BTreeMap<(String, Vec<String>, String), Vec<Op>> = BTreeMap::new();
    let mut seen_ops: BTreeSet<(String, String)> = BTreeSet::new();
    for (_dir, spec) in products.as_object().unwrap() {
        for (path, item) in spec.get("paths").and_then(Value::as_object).into_iter().flatten() {
            // A path key with a `?query` (AWS-S3-style sub-resource selectors) is
            // not a distinct RESOURCE — the query, not the path, distinguishes it.
            // Fold only clean paths; those niche variants stay reachable via `hanzo
            // api … --query`.
            if !path.starts_with("/v1/") || path.contains('?') || path.contains('#') {
                continue;
            }
            let product0 = segs(path)[1];
            if EXCLUDE.contains(&product0) || DENY.contains(&product0) || is_wild(product0) {
                continue;
            }
            for (m, op) in item.as_object().into_iter().flatten() {
                if !VERBS.contains(&m.as_str()) {
                    continue;
                }
                let method = m.to_uppercase();
                if !seen_ops.insert((method.clone(), path.clone())) {
                    continue; // a path defined in two dirs — first wins
                }
                let Some(f) = fold(&method, path, &all) else { continue };
                // Curation remap: absorb a product UNDER another as a sub-namespace
                // (e.g. `machines list` → `compute machines list`). The PATH is
                // unchanged — only the command coordinate moves.
                let (mut product, mut nodes) = (f.product, f.nodes);
                if let Some((from, target)) = REMAP.iter().find(|(from, _)| *from == product) {
                    product = target.to_string();
                    nodes.insert(0, (*from).to_string());
                }
                // Typed flags: body properties (writes) + query parameters (all ops).
                let mut fields = if matches!(method.as_str(), "POST" | "PUT" | "PATCH") {
                    body_schema(spec, op).map(|s| fields_of(spec, s)).unwrap_or_default()
                } else {
                    vec![]
                };
                fields.extend(query_fields(spec, op));
                // One name may appear as BOTH a body property and a query param
                // (or twice after kebab-casing); a clap long must be unique, so
                // keep the FIRST (body wins over query).
                let mut seen_flag: BTreeSet<String> = BTreeSet::new();
                fields.retain(|f| seen_flag.insert(f.flag.clone()));
                // Mark any multi-segment (catch-all) path param for this product, so
                // the runtime keeps its slashes raw (see REST_PARAMS / fill_path).
                let rest: Vec<String> = f
                    .params
                    .iter()
                    .filter(|p| REST_PARAMS.contains(&(product.as_str(), p.as_str())))
                    .cloned()
                    .collect();
                let coord = (product.clone(), nodes.clone(), f.verb.clone());
                raw.entry(coord).or_default().push(Op {
                    product,
                    nodes,
                    verb: f.verb,
                    method,
                    path: path.clone(),
                    params: f.params,
                    rest,
                    fields,
                });
            }
        }
    }

    // Collision resolution — ARITY only. When two ops fold to the same coordinate
    // with different positional counts (`GET /v1/mq/objects` vs
    // `GET /v1/mq/objects/{store}/list`), the MAX-arity op keeps the verb and the
    // shallower one becomes `<verb>-all`.
    //
    // A group/leaf coincidence (a collection-root verb that also names a child
    // group, e.g. `GET /v1/kv` = `list` while `/v1/kv/list/{key}` nests a `list`
    // group) is NOT renamed: the op lands as a leaf on the SAME node as the group,
    // and the runtime makes that node a RUNNABLE GROUP (`hanzo kv list` runs the
    // collection GET; `hanzo kv list push <key>` runs the datatype). Keeping the
    // collection GET a runnable leaf is the whole point.
    let mut resolved: BTreeMap<(String, Vec<String>, String), Vec<Op>> = BTreeMap::new();
    for ((p, nodes, verb), ops) in raw {
        let arities: BTreeSet<usize> = ops.iter().map(|o| o.params.len()).collect();
        if arities.len() <= 1 {
            resolved.entry((p, nodes, verb)).or_default().extend(ops);
            continue;
        }
        let maxar = *arities.iter().max().unwrap();
        for mut o in ops {
            // Rename the op's OWN verb, not just the map key — the emitted data
            // must carry the disambiguated verb.
            if o.params.len() != maxar {
                o.verb = format!("{verb}-all");
            }
            let coord = (p.clone(), nodes.clone(), o.verb.clone());
            resolved.entry(coord).or_default().push(o);
        }
    }

    // Collapse multi-method coordinates by priority (one op per command).
    let mut coords: Vec<Op> = Vec::new();
    for (_c, mut ops) in resolved {
        ops.sort_by_key(|o| method_rank(&o.method));
        let arities: BTreeSet<usize> = ops.iter().map(|o| o.params.len()).collect();
        assert!(
            arities.len() == 1,
            "unresolved arity collision: {:?}",
            ops.iter().map(|o| &o.path).collect::<Vec<_>>()
        );
        coords.push(ops.into_iter().next().unwrap());
    }
    coords.sort_by(|a, b| (&a.product, &a.nodes, &a.verb).cmp(&(&b.product, &b.nodes, &b.verb)));

    // Guard: the runtime tree needs unique (product, nodes, verb) — fail loudly.
    let mut seen_coord: BTreeSet<(String, Vec<String>, String)> = BTreeSet::new();
    for o in &coords {
        let c = (o.product.clone(), o.nodes.clone(), o.verb.clone());
        assert!(seen_coord.insert(c), "DUP COORD: {} {:?} {} <- {}", o.product, o.nodes, o.verb, o.path);
    }

    // ---- emit ----
    // The authored specs are the ONLY source: every cloud capability is a real
    // `hanzo <product> <resource> <verb>`. A product with no authored spec, or an
    // unenumerable one, is simply absent — there is no passthrough and no `hanzo
    // api` fallback to paper over it. That gap closes by authoring the spec.
    let ntyped = coords.iter().filter(|o| !o.fields.is_empty()).count();
    let ndata = coords
        .iter()
        .filter(|o| o.fields.is_empty() && matches!(o.method.as_str(), "POST" | "PUT" | "PATCH"))
        .count();
    let nprod = coords.iter().map(|o| &o.product).collect::<BTreeSet<_>>().len();

    let mut s = String::new();
    s.push_str("//! @generated by `cargo run --bin genproduct` from the committed spec\n");
    s.push_str("//! snapshot at `spec/products.json` (the hand-authored OpenAPI specs).\n");
    s.push_str("//! DO NOT EDIT BY HAND.\n//!\n");
    s.push_str("//! Pure DATA: (product, resource nodes, verb, method, /v1 path, params, typed\n");
    s.push_str("//! body fields). No host, no absolute URL, no auth — pinned by a test.\n\n");
    s.push_str("use super::{Field, Op, Ty};\n\n");
    s.push_str(&format!(
        "/// {} coordinates across {} products ({} typed-flag, {} --data-fallback writes).\n",
        coords.len(),
        nprod,
        ntyped,
        ndata
    ));
    s.push_str("pub(crate) static OPS: &[Op] = &[\n");
    for o in &coords {
        s.push_str(&emit_op(o));
    }
    s.push_str("];\n");

    let out = manifest.join("src/commands/product/generated.rs");
    fs::write(&out, s).unwrap();
    eprintln!(
        "genproduct: {} ops ({} typed, {} data-fallback) across {} products -> {}",
        coords.len(),
        ntyped,
        ndata,
        nprod,
        out.display()
    );
}

fn emit_slice(items: &[String]) -> String {
    format!("&[{}]", items.iter().map(|s| format!("{s:?}")).collect::<Vec<_>>().join(", "))
}

fn emit_op(o: &Op) -> String {
    let fields = if o.fields.is_empty() {
        "&[]".to_string()
    } else {
        let items: Vec<String> = o
            .fields
            .iter()
            .map(|f| {
                // The clap id is namespaced by LOCATION so a body property and a
                // query param of the same name never collide.
                let id = format!("{}.{}", if f.query { "query" } else { "field" }, f.key);
                format!(
                    "Field {{ key: {:?}, id: {:?}, flag: {:?}, ty: Ty::{}, required: {}, choices: {}, query: {}, secret: {} }}",
                    f.key,
                    id,
                    f.flag,
                    f.ty,
                    f.required,
                    emit_slice(&f.choices),
                    f.query,
                    f.secret
                )
            })
            .collect();
        format!("&[{}]", items.join(", "))
    };
    format!(
        "    Op {{ product: {:?}, nodes: {}, verb: {:?}, method: {:?}, path: {:?}, params: {}, rest: {}, fields: {} }},\n",
        o.product,
        emit_slice(&o.nodes),
        o.verb,
        o.method,
        o.path,
        emit_slice(&o.params),
        emit_slice(&o.rest),
        fields
    )
}
