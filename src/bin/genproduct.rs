//! `genproduct` — derive `src/commands/product/generated.rs` from `spec/cloud.json`
//! and nothing else. Offline and deterministic: the same spec always yields the
//! same tree, which is what lets `--check` be a build gate and what keeps `hanzo`
//! free of any runtime spec fetch.
//!
//! Source of truth: `spec/cloud.json`, ONE OpenAPI 3.1 document written by
//! `genspec` — the authored shapes from hanzoai/openapi, minus every operation
//! cloud's live route table refutes. Existence is therefore the REGISTRY's answer
//! and shape is the authored spec's; neither is restated here. Refresh the surface
//! with `cargo run --features genspec --bin genspec`, then run this.
//!
//! The fold from path → (product, resource nodes, verb, params) is TOTAL; typed
//! fields resolve $ref → component schema → property names + types + required.
//! It emits pure DATA: no host, no URL, no auth. See `commands::product`.
//!
//! Usage: `genproduct` writes the tree; `genproduct --check` regenerates it in
//! memory and fails if the committed file differs.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::PathBuf;

use serde_json::Value;

const VERBS: [&str; 5] = ["get", "post", "put", "patch", "delete"];
/// Curation — products NOT emitted as top-level commands. POLICY ONLY: whether a
/// route EXISTS is decided upstream by `genspec` against cloud's live route table,
/// so nothing here may encode "the server 404s this". Every entry states a choice
/// that would still hold if the route were served perfectly.
///
/// `EXCLUDE` used to sit beside this list holding `billing`/`agent`/`deploy` with
/// the note "local commands own these bare names". It was a THIRD statement of one
/// fact — every entry was already in DENY, and `product::augment` reads the real
/// answer off the parser (`cmd.get_subcommands()`) rather than off any list. Two of
/// its three names had ALSO stopped being true: `agent` and `deploy` were deleted as
/// top-level commands (`main.rs::old_top_level_verbs_are_removed` asserts it), so the
/// list reserved names for commands that no longer existed and 21 served routes —
/// the whole `/v1/deploy` control plane behind the Hanzo CD console — reached no one.
const DENY: &[&str] = &[
    // Noise: sub-operations, UI/config surfaces, or enumeration artifacts — not
    // first-class products a person reaches for.
    "download", "upload", "files", "completions", "console", "settings",
    "search-docs", "index-docs", "chat-docs", "indexers", "embed-status",
    "csrf", "openapi.json", "account-bridge", "agent-bindings",
    // Singular/plural dedupe: the LOCAL hand-written command owns `network`
    // (network selection), and `bot` is the canonical cloud product — so the
    // redundant cloud PLURALS are dropped. `clusters` is served again: the hand
    // `cluster` proxy is deleted (it discarded its own name argument).
    "networks", "bots",
    // Internal control planes, not user commands: `provisioning` is the internal
    // provisioner (you provision via the concrete `hanzo vector|kv|s3 create`),
    // and `do` is the DigitalOcean PROVIDER backend.
    "provisioning", "do",
    // A LOCAL hand-written command owns these names, whatever the server serves:
    // `code` is the AI coding session, `billing` is the prepaid-wallet wrapper with
    // its own UX, and `help` is clap's own builtin — a generated `help` product
    // panics the parser with a duplicate command.
    //
    // Each of these SHADOWS served operations, and the shadow is a stated gap, not
    // a claim that the routes are absent: `hanzo billing` reaches 2 of the 22 live
    // `/v1/billing/*` routes, and `hanzo code` reaches none of the 6 live
    // `/v1/code/*` ones. Closing a shadow means the local command ABSORBS the
    // product's operations — a UX decision per command, not a list edit here.
    "code", "help", "billing",
    // `engine` is the LOCAL `hanzo engine serve <model>` (launches hanzo-engine on
    // this machine). The cloud engine product manages engine CLUSTERS; when its
    // revival lands it needs its own noun or a nested home, not this name. It has
    // revived — `/v1/engine/{model,models,status,system}` are served — so this is a
    // shadow of 4 operations on the same terms as `billing` above.
    "engine",
    // Singular/plural: `/v1/agent` (one tool-calling round + its conversation log,
    // registered by github.com/hanzoai/agent) and `/v1/agents` (the agent registry,
    // sessions and targets) are two products sharing one noun. Mounting both would
    // ship `hanzo agent` beside `hanzo agents`, which is the collision the house
    // rule forbids, so the plural — the larger, cloud-owned surface — keeps the
    // name. The fix is a route move in hanzoai/agent (`/v1/agent` →
    // `/v1/agents/run`, `/v1/agent/{presets,conversations}` → `/v1/agents/…`), and
    // this entry goes when that lands. It is NOT here because a local command owns
    // the name: no local `agent` command exists.
    "agent",
    // `gateway` USED TO BE HERE, with a comment noting its whole `/v1/gateway/*`
    // subtree was unmounted. That was a fact about the server kept in a list in the
    // client — exactly the drift this file no longer owns. `genspec` refutes those
    // 27 operations against the live route table, so the entry is gone and the
    // curation test still passes. The real inference surface is TOP-LEVEL and
    // unaffected: `hanzo models`, `hanzo chat completions`, `hanzo embeddings`.
];
/// Curation — absorb a product's ops UNDER another command as a sub-namespace, so
/// the compute plane is ONE `hanzo compute` (machines + gpus + regions/sizes)
/// instead of three top-levels. `machines`/`gpus` live at their own path prefixes
/// with a colliding `get`, so a FLAT `compute list` is impossible without
/// ambiguity — sub-namespacing unifies them losslessly. A flat surface would need
/// the cloud specs reorganized under one `/v1/compute` tag.
const REMAP: &[(&str, &str)] = &[("machines", "compute"), ("gpus", "compute")];
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
/// A collection is a path whose MEMBERS are addressed by appending a `{param}`.
/// The member path need not STOP there: `/v1/admin/plugins/{name}/reload` selects
/// one plugin exactly as `/v1/iam/users/{id}` selects one user, so the read at the
/// root is a `list` either way. Requiring the param to be the last segment would
/// call `plugins` a singular resource on the evidence that its members happen to
/// have sub-actions rather than a bare GET.
fn is_collection(p: &str, all: &BTreeSet<String>) -> bool {
    let ps = segs(p);
    all.iter().any(|k| {
        let ks = segs(k);
        ks.len() > ps.len() && ks[..ps.len()] == ps[..] && is_param(ks[ps.len()])
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
    /// One line of prose from the spec (a typed op's doc comment, lifted by
    /// zipdoc and carried through genspec). Empty when the op is undescribed.
    sum: String,
}

fn method_rank(m: &str) -> usize {
    METHOD_PRIORITY.iter().position(|x| *x == m).unwrap_or(usize::MAX)
}

fn main() {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let spec_path = manifest.join("spec/cloud.json");
    let spec: Value = serde_json::from_str(
        &fs::read_to_string(&spec_path)
            .unwrap_or_else(|e| panic!("read {}: {e} — run `cargo run --features genspec --bin genspec`", spec_path.display())),
    )
    .unwrap();
    let paths = spec.get("paths").and_then(Value::as_object).expect("spec has no paths");

    // Per-product prose: the spec's `tags`, each description the owning Go
    // package's doc synopsis (lifted by cloud's weave, carried by genspec).
    // Whitespace is normalized to one line. Prose is display-only — it may
    // truthfully name a host or URL shape; the no-host invariant guards call
    // data, and the test scrubs prose before enforcing it.
    let mut tags: Vec<(String, String)> = spec
        .get("tags")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|t| {
            let n = t.get("name")?.as_str()?.to_string();
            let d = t.get("description")?.as_str()?.split_whitespace().collect::<Vec<_>>().join(" ");
            // Go doc convention opens with "Package <name> "; that identifier
            // belongs to Go's namespace, not the help line. Same rule as the
            // handler-name strip: drop the exact prefix, recapitalize, and a
            // connective "is/are" goes with it ("Package books is double-entry
            // accounting" -> "Double-entry accounting").
            // The strip is generic over the package WORD, not the tag name —
            // `audit` is package auditlog, `evals` package eval — any leading
            // "Package <word>" is the convention, whatever the word.
            let d = {
                let mut d = d;
                if let Some(rest) = d.strip_prefix("Package ") {
                    if let Some(sp) = rest.find(' ') {
                        let rest = &rest[sp + 1..];
                        let rest = rest.strip_prefix("is ").or_else(|| rest.strip_prefix("are ")).unwrap_or(rest);
                        let mut c = rest.chars();
                        d = match c.next() {
                            Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
                            None => rest.to_string(),
                        };
                    }
                }
                d
            };
            // The list line is ONE sentence, capped — clap renders it beside a
            // hundred siblings. A sentence ending in ":" was leading into a Go
            // doc's list; the colon dangles here, so it goes. The FULL prose
            // stays in the OpenAPI document and the MCP tool; the CLI's help
            // line is a label, not a manual.
            let d = {
                let mut s = match d.find(". ") {
                    Some(i) => d[..i + 1].to_string(),
                    None => d.clone(),
                };
                if s.len() > 100 {
                    let cut = s[..100].rfind(' ').unwrap_or(100);
                    s = format!("{}…", s[..cut].trim_end_matches([',', ';', ':', '.']));
                }
                s.trim_end_matches(['.', ':']).trim().to_string()
            };
            (!d.is_empty()).then_some((n, d))
        })
        .collect();
    tags.sort();

    // The path universe the fold reads to tell a collection from an item and a
    // group from a leaf. It is the SERVED surface, so a sibling route that cloud
    // does not answer can no longer shape a command that it does.
    let all: BTreeSet<String> = paths.keys().filter(|p| p.starts_with("/v1/")).cloned().collect();

    let mut raw: BTreeMap<(String, Vec<String>, String), Vec<Op>> = BTreeMap::new();
    for (path, item) in paths {
        // A path key with a `?query` (AWS-S3-style sub-resource selectors) is
        // not a distinct RESOURCE — the query, not the path, distinguishes it.
        if !path.starts_with("/v1/") || path.contains('?') || path.contains('#') {
            continue;
        }
        let product0 = segs(path)[1];
        if DENY.contains(&product0) || is_wild(product0) {
            continue;
        }
        for (m, op) in item.as_object().into_iter().flatten() {
            if !VERBS.contains(&m.as_str()) {
                continue;
            }
            let method = m.to_uppercase();
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
                body_schema(&spec, op).map(|s| fields_of(&spec, s)).unwrap_or_default()
            } else {
                vec![]
            };
            fields.extend(query_fields(&spec, op));
            // A body property that repeats a PATH parameter is the SAME value,
            // already supplied as a positional. It appears twice because zip binds
            // the path segment into the same Go struct as the body, so the emitted
            // schema honestly carries both. A flag for it would be a second way to
            // say one thing — and one that cannot work alone, the positional being
            // required.
            fields.retain(|fd| !f.params.contains(&fd.key));
            // One name may appear as BOTH a body property and a query param
            // (or twice after kebab-casing); a clap long must be unique, so
            // keep the FIRST (body wins over query).
            let mut seen_flag: BTreeSet<String> = BTreeSet::new();
            fields.retain(|f| seen_flag.insert(f.flag.clone()));
            // The multi-segment (catch-all) path params, as the ROUTER reports
            // them: genspec marks a param the server addresses with a fiber `*`,
            // so the runtime keeps its slashes raw instead of `%2F`-escaping them
            // into one opaque segment the backend 404s (see fill_path).
            let rest: Vec<String> = op
                .get("x-catch-all")
                .and_then(Value::as_array)
                .map(|a| a.iter().filter_map(Value::as_str).map(str::to_string).collect())
                .unwrap_or_default();
            let sum = op.get("summary").and_then(Value::as_str).unwrap_or("").to_string();
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
                sum,
            });
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

    // Guard: prose is not optional, and there is no substitute for it. A command
    // whose help line would be blank is a MISSING GO DOC COMMENT on the handler in
    // hanzoai/cloud — zipdoc lifts that comment into the published route table,
    // genspec joins it in, and this generator only carries it. Printing something
    // else (the route, the verb, a manufactured phrase) would convert a fixable
    // gap at the source into a shipped help line that reads deliberate, and nobody
    // files a bug against a design choice. So the generator REFUSES to emit the
    // command at all: the gap stays visible at the one place it can be closed.
    let bare: Vec<String> = coords
        .iter()
        .filter(|o| o.sum.trim().is_empty())
        .map(|o| {
            let mut c = vec![o.product.clone()];
            c.extend(o.nodes.iter().cloned());
            c.push(o.verb.clone());
            format!("  hanzo {}\n      <- {} {}", c.join(" "), o.method, o.path)
        })
        .collect();
    assert!(
        bare.is_empty(),
        "{} operation(s) in spec/cloud.json carry no summary, so these commands would have \
         nothing to say:\n{}\n\n\
         Every command states what it does for the person running it. That sentence is the Go \
         doc comment on the handler in hanzoai/cloud: zipdoc lifts it into the published route \
         table, genspec joins it into spec/cloud.json, and genproduct carries it here. NOTHING \
         IN THIS REPO CAN SUPPLY IT — write the doc comment where the handler lives, run `make \
         openapi` in hanzoai/cloud, then re-run `cargo run --features genspec --bin genspec`.",
        bare.len(),
        bare.join("\n"),
    );

    // ---- emit ----
    // `spec/cloud.json` is the ONLY source: every cloud capability is a real
    // `hanzo <product> <resource> <verb>`. A product the spec does not carry —
    // never authored, or authored and refuted by the live route table — is simply
    // absent. There is no passthrough and no `hanzo api` fallback to paper over
    // it: that gap closes by authoring the spec, or by serving the route.
    let ntyped = coords.iter().filter(|o| !o.fields.is_empty()).count();
    let ndata = coords
        .iter()
        .filter(|o| o.fields.is_empty() && matches!(o.method.as_str(), "POST" | "PUT" | "PATCH"))
        .count();
    let nprod = coords.iter().map(|o| &o.product).collect::<BTreeSet<_>>().len();

    let mut s = String::new();
    s.push_str("//! @generated by `cargo run --bin genproduct` from `spec/cloud.json`\n");
    s.push_str("//! (the authored shapes, minus everything cloud's live route table refutes).\n");
    s.push_str("//! DO NOT EDIT BY HAND — `cargo test` regenerates and diffs this.\n//!\n");
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

    // One line per product that documents itself — the group's help line. Only
    // products that actually field commands; a tag for an excluded product is
    // dropped with it.
    let live: BTreeSet<&String> = coords.iter().map(|o| &o.product).collect();
    s.push_str("\n/// Each product's own prose (its package doc synopsis), for the group help.\n");
    s.push_str("pub(crate) static PRODUCTS: &[(&str, &str)] = &[\n");
    for (n, d) in tags.iter().filter(|(n, _)| live.contains(n)) {
        s.push_str(&format!("    ({n:?}, {d:?}),\n"));
    }
    s.push_str("];\n");

    let out = manifest.join("src/commands/product/generated.rs");
    // --check is the drift gate (see tests/spec_drift.rs). The derivation runs
    // either way; the flag only decides whether the answer is written or compared,
    // so the gate can never test a different derivation than the one that writes.
    if std::env::args().any(|a| a == "--check") {
        let got = fs::read_to_string(&out).unwrap_or_default();
        if got != s {
            eprintln!(
                "genproduct --check: {} is not what spec/cloud.json derives ({} vs {} bytes). \
                 Run `cargo run --bin genproduct` and commit the result.",
                out.display(),
                got.len(),
                s.len()
            );
            std::process::exit(1);
        }
        eprintln!("genproduct --check: {} ops match spec/cloud.json", coords.len());
        return;
    }
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
        "    Op {{ product: {:?}, nodes: {}, verb: {:?}, method: {:?}, path: {:?}, params: {}, rest: {}, fields: {}, sum: {:?} }},\n",
        o.product,
        emit_slice(&o.nodes),
        o.verb,
        o.method,
        o.path,
        emit_slice(&o.params),
        emit_slice(&o.rest),
        fields,
        o.sum
    )
}
