//! `genproduct` — regenerate `src/commands/product/generated.rs` from the
//! COMMITTED, hand-authored OpenAPI snapshot. Run by hand when the snapshot
//! changes; the output is checked in, so `hanzo` never fetches a spec at runtime.
//!
//! Source of truth: `spec/products.json` — the per-product OpenAPI 3.1 specs
//! (repo hanzoai/openapi) vendored as one JSON object keyed by product. Unlike a
//! router dump this carries real requestBody schemas, so a write op becomes TYPED
//! `--flags`, not `--data '<json>'`. `spec/openapi.json` (the live router shape)
//! is used only for the product UNIVERSE: router products with no authored spec
//! fall through to a passthrough command, and `/v1/code`'s verbs nest under the
//! `hanzo code` wrapper.
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
const EXCLUDE: [&str; 4] = ["kms", "billing", "agent", "deploy"];
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
    let mut out = Vec::new();
    for (name, pschema) in props {
        let is_ref = pschema.get("$ref").is_some();
        let d = deref(spec, &pschema);
        let enum_vals: Vec<String> = d
            .get("enum")
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect())
            .unwrap_or_default();
        let t = d.get("type").and_then(Value::as_str).unwrap_or("");
        let (ty, choices): (&'static str, Vec<String>) = if is_ref {
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
        };
        let required = required.contains(&name);
        out.push(FieldDef { flag: kebab(&name), key: name, ty, required, choices });
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
    fields: Vec<FieldDef>,
}

fn method_rank(m: &str) -> usize {
    METHOD_PRIORITY.iter().position(|x| *x == m).unwrap_or(usize::MAX)
}

fn main() {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let products: Value =
        serde_json::from_str(&fs::read_to_string(manifest.join("spec/products.json")).unwrap()).unwrap();
    let router: Value =
        serde_json::from_str(&fs::read_to_string(manifest.join("spec/openapi.json")).unwrap()).unwrap();

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
            if EXCLUDE.contains(&product0) || is_wild(product0) {
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
                let fields = if matches!(method.as_str(), "POST" | "PUT" | "PATCH") {
                    body_schema(spec, op).map(|s| fields_of(spec, s)).unwrap_or_default()
                } else {
                    vec![]
                };
                let coord = (f.product.clone(), f.nodes.clone(), f.verb.clone());
                raw.entry(coord).or_default().push(Op {
                    product: f.product,
                    nodes: f.nodes,
                    verb: f.verb,
                    method,
                    path: path.clone(),
                    params: f.params,
                    fields,
                });
            }
        }
    }

    // Collision resolution: a coordinate with >1 arity, or whose verb is also a
    // child GROUP node here, is ambiguous. The MAX-arity op keeps the verb; the
    // rest (and the whole set on a group/leaf clash) move to `<verb>-all`. Proven
    // to leave 0 residual (asserted at collapse).
    let mut child_map: BTreeMap<(String, Vec<String>), BTreeSet<String>> = BTreeMap::new();
    for (p, nodes, _verb) in raw.keys() {
        for i in 0..nodes.len() {
            child_map.entry((p.clone(), nodes[..i].to_vec())).or_default().insert(nodes[i].clone());
        }
    }
    let is_group = |p: &str, nodes: &[String], verb: &str| -> bool {
        child_map.get(&(p.to_string(), nodes.to_vec())).is_some_and(|s| s.contains(verb))
    };
    let mut resolved: BTreeMap<(String, Vec<String>, String), Vec<Op>> = BTreeMap::new();
    for ((p, nodes, verb), ops) in raw {
        let arities: BTreeSet<usize> = ops.iter().map(|o| o.params.len()).collect();
        let gl = is_group(&p, &nodes, &verb);
        if arities.len() <= 1 && !gl {
            resolved.entry((p, nodes, verb)).or_default().extend(ops);
            continue;
        }
        let maxar = *arities.iter().max().unwrap();
        for mut o in ops {
            // Rename the op's OWN verb, not just the map key — the emitted data
            // must carry the disambiguated verb.
            if !(o.params.len() == maxar && !gl) {
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

    let authored: BTreeSet<String> = coords.iter().map(|o| o.product.clone()).collect();

    // /v1/code verbs (router-derived, shape-only) — nest under the wrapper.
    let mut code_all: BTreeSet<String> = BTreeSet::new();
    for (p, _item) in router.get("paths").and_then(Value::as_object).into_iter().flatten() {
        if p.starts_with("/v1/code/") || p == "/v1/code" {
            code_all.insert(p.clone());
        }
    }
    let mut code_by: BTreeMap<(Vec<String>, String), Vec<Op>> = BTreeMap::new();
    for (p, item) in router.get("paths").and_then(Value::as_object).into_iter().flatten() {
        if !(p.starts_with("/v1/code/") || p == "/v1/code") {
            continue;
        }
        for (m, _op) in item.as_object().into_iter().flatten() {
            if !VERBS.contains(&m.as_str()) {
                continue;
            }
            let method = m.to_uppercase();
            let Some(f) = fold(&method, p, &code_all) else { continue };
            code_by.entry((f.nodes.clone(), f.verb.clone())).or_default().push(Op {
                product: "code".into(),
                nodes: f.nodes,
                verb: f.verb,
                method,
                path: p.clone(),
                params: f.params,
                fields: vec![],
            });
        }
    }
    let mut code_final: Vec<Op> = Vec::new();
    for (_k, mut ops) in code_by {
        ops.sort_by_key(|o| method_rank(&o.method));
        code_final.push(ops.into_iter().next().unwrap());
    }
    code_final.sort_by(|a, b| (&a.nodes, &a.verb).cmp(&(&b.nodes, &b.verb)));

    // Passthrough: router products with no authored spec (and not a local/code).
    let router_products: BTreeSet<String> = router
        .get("paths")
        .and_then(Value::as_object)
        .into_iter()
        .flatten()
        .filter_map(|(k, _)| {
            let s = segs(k);
            (s.len() >= 2 && s[0] == "v1" && !is_wild(s[1])).then(|| s[1].to_string())
        })
        .collect();
    let local: BTreeSet<&str> = ["kms", "billing", "agent", "deploy", "code"].into_iter().collect();
    let passthrough: BTreeSet<String> = router_products
        .into_iter()
        .filter(|p| !authored.contains(p) && !local.contains(p.as_str()))
        .collect();

    // ---- emit ----
    let ntyped = coords.iter().filter(|o| !o.fields.is_empty()).count();
    let ndata = coords
        .iter()
        .filter(|o| o.fields.is_empty() && matches!(o.method.as_str(), "POST" | "PUT" | "PATCH"))
        .count();
    let nprod = authored.len();

    let mut s = String::new();
    s.push_str("//! @generated by `cargo run --bin genproduct` from the committed spec\n");
    s.push_str("//! snapshots at `spec/products.json` (authored, typed) + `spec/openapi.json`\n");
    s.push_str("//! (router shape, for code + passthrough). DO NOT EDIT BY HAND.\n//!\n");
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
    for o in coords.iter().chain(code_final.iter()) {
        s.push_str(&emit_op(o));
    }
    s.push_str("];\n\n");
    s.push_str("pub(crate) static PASSTHROUGH: &[&str] = &[\n");
    for p in &passthrough {
        s.push_str(&format!("    {:?},\n", p));
    }
    s.push_str("];\n");

    let out = manifest.join("src/commands/product/generated.rs");
    fs::write(&out, s).unwrap();
    eprintln!(
        "genproduct: {} ops ({} typed, {} data-fallback), {} code verbs, {} passthrough -> {}",
        coords.len(),
        ntyped,
        ndata,
        code_final.len(),
        passthrough.len(),
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
                format!(
                    "Field {{ key: {:?}, id: {:?}, flag: {:?}, ty: Ty::{}, required: {}, choices: {} }}",
                    f.key,
                    format!("field.{}", f.key),
                    f.flag,
                    f.ty,
                    f.required,
                    emit_slice(&f.choices)
                )
            })
            .collect();
        format!("&[{}]", items.join(", "))
    };
    format!(
        "    Op {{ product: {:?}, nodes: {}, verb: {:?}, method: {:?}, path: {:?}, params: {}, fields: {} }},\n",
        o.product,
        emit_slice(&o.nodes),
        o.verb,
        o.method,
        o.path,
        emit_slice(&o.params),
        fields
    )
}
