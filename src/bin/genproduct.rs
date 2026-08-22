//! `genproduct` — derive `src/commands/product/generated.rs` from `spec/cloud.json`
//! and nothing else. Offline and deterministic: the same spec always yields the
//! same tree, which is what lets `--check` be a build gate and what keeps `hanzo`
//! free of any runtime spec fetch.
//!
//! Source of truth: `spec/cloud.json`, ONE OpenAPI 3.1 document written by
//! `genspec` from hanzoai/cloud's own emitted `openapi.yaml` and nothing else.
//! Existence, prose and shape all come from that one reading, and none of the
//! three is restated here. Refresh the surface with `cargo run --features
//! genspec --bin genspec`, then run this.
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

/// The ONE curation table — which products the tree does not surface at their own
/// bare name, which it absorbs under another command, and WHY, as data rather than
/// as a comment. `driftgate` reads the same file to excuse the same gaps and RUNS
/// every spelling an entry claims, so a product dropped here and a product excused
/// there can never be two different lists, and a reason cannot quietly stop being
/// true. It was three tables — `DENY`, `REMAP`, and an `EXCLUDE` that was a strict
/// subset of `DENY` — with the reasons in comments; two of `EXCLUDE`'s three names
/// reserved local commands that had been DELETED, and 21 served `/v1/deploy`
/// operations reached nobody while the list still called it a decision.
#[path = "../curation.rs"]
mod curation;

const VERBS: [&str; 5] = ["get", "post", "put", "patch", "delete"];
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
/// The collection-root verb. Held DISJOINT from `item_verb` below, because
/// `/v1/x` and `/v1/x/{id}` fold to the same `nodes`, so an overlapping name
/// makes them one coordinate and one of them reaches nobody.
///
/// `PUT` and `PATCH` are TWO commands, and cloud says so in its own prose:
/// `PUT /v1/store/{storeid}` is "Replace a storefront outright" and `PATCH` is
/// "Change part of a storefront". They both read `replace` before, so every
/// `PATCH` beside a `PUT` was silently unreachable — 62 of them. `patch` is the
/// method's own name rather than an invented synonym: this generator refuses to
/// coin vocabulary, and the one word already in the contract is not a coinage.
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
        "PUT" => "replace",
        "PATCH" => "patch",
        _ => "clear",
    }
}

/// The ITEM verb — the address ends in a `{param}`, so there is no noun to name
/// the command after. Disjoint from `root_verb` by construction (`get` is the
/// one shared word, and it is reachable at a root only when that root is NOT a
/// collection, which is exactly when no item path exists).
///
/// `set` is `PUT` here for the same reason `patch` is `PATCH` above: `replace`
/// belongs to the collection table, and the alternative was leaving `PUT` and
/// `PATCH` folded into one `update` that only one of them ever reached.
fn item_verb(method: &str) -> &'static str {
    match method {
        "GET" => "get",
        "DELETE" => "rm",
        "PUT" => "set",
        "PATCH" => "update",
        _ => "add",
    }
}

struct Folded {
    product: String,
    nodes: Vec<String>,
    verb: String,
    params: Vec<String>,
}

/// `multi` is the set of paths carrying MORE THAN ONE verb. A terminal noun
/// normally becomes the command's verb (`GET /v1/websearch/search` →
/// `websearch search`), which is right for an address with one method and lossy
/// for an address with several: every method folds to the same coordinate and all
/// but one reach nobody. Those addresses take the `has_child` shape instead.
fn fold(
    method: &str,
    path: &str,
    all: &BTreeSet<String>,
    multi: &BTreeSet<String>,
) -> Option<Folded> {
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
        item_verb(method).into()
    } else if has_child(&p, all) || multi.contains(&p) {
        // Two reasons for the SAME shape. A noun with children is a group, so it
        // cannot also be a verb. A noun answering several methods is not one
        // command either — `GET` lists and `POST` creates — and letting the noun
        // be the verb makes them one coordinate, of which only the first survives.
        //
        // `root_verb` is the decision, and it is the existing one: no new naming
        // judgement is made here. It does NOT separate `PUT` from `PATCH` (both
        // read `replace`) — whether those are one command or two is a question
        // about the API, not the fold, and it stays counted rather than guessed.
        let v = root_verb(method, is_collection(&p, all));
        nodes.push(last.clone());
        v.into()
    } else {
        last.clone()
    };
    Some(Folded { product, nodes, verb, params })
}

// ---- typed field extraction -------------------------------------------------

/// How deep a nested body object is expanded into dotted flags (`--a.b.c`).
/// Beyond it a property keeps its whole JSON value in one flag, which is where
/// every object property used to land. Measured against this document the cap
/// SATURATES at 3 — caps of 3, 4 and 8 all derive the identical field set,
/// because the `$ref` cycle guard stops the recursive schemas first — so it is a
/// termination guarantee rather than a policy about the surface.
const MAX_NEST: usize = 3;

#[derive(Clone)]
struct FieldDef {
    /// The body key, DOTTED for a nested property (`spec.replicas`). No schema
    /// property name in this document contains a `.` (measured: 0 of 3751), so
    /// the path is unambiguous and the runtime rebuilds the object from it.
    key: String,
    flag: String,
    ty: &'static str, // Str|Int|Num|Bool|Json
    required: bool,
    choices: Vec<String>,
    /// A query-string parameter (goes in the URL), vs a requestBody property.
    query: bool,
    /// A SECRET body value: read from stdin, NEVER a flag — so it can never land
    /// in argv, `ps` or shell history. Decided by `is_secret`, which honours
    /// `format: password` first and falls back to the NAME, because the document
    /// carries that format on zero fields. The runtime reads it through
    /// `iam::secret::read_secret`.
    secret: bool,
    /// An ARRAY: the flag may be given more than once and the values collect into
    /// a JSON array (`--tag a --tag b` → `["a","b"]`). `ty` is then the ELEMENT's
    /// type, so an array of strings is a repeatable `--flag STRING` rather than
    /// one opaque `--flag '["a","b"]'`.
    repeat: bool,
}

/// A body property that is a SECRET VALUE — read from stdin, never a flag, so it
/// cannot land in argv, `ps` output or shell history.
///
/// `format: password` is the standard OpenAPI marker and is honoured first. It is
/// NOT trusted alone: cloud's document carries that format on ZERO fields while
/// serving credential-shaped inputs — sign-in passwords, client secrets, API keys,
/// bearer tokens.
///
/// ONE AUTHORITY MADE THIS LOAD-BEARING, not optional. The marker used to reach
/// exactly one command on the strength of the hand-authored master, which carried
/// `format: password` twice. That file is deleted, and cloud's document — now the
/// only input — carries it zero times. So the choice here is not "marker or
/// heuristic"; it is "heuristic or nothing", and nothing means the generator emits
/// `--password <VALUE>` and the credential goes to `~/.zsh_history`.
///
/// So the NAME is also evidence. That is a heuristic, and it is the right kind of
/// heuristic because the two ways of being wrong are not symmetric: a false
/// positive asks someone to type a value on stdin that did not need it, while a
/// false negative writes a live credential into `~/.zsh_history`. When in doubt,
/// stdin.
///
/// The disqualifiers matter as much as the matches — `passwordSalt`,
/// `passwordHash`, `tokenFormat` and `password_file` all NAME a credential
/// without being one, and forcing those onto stdin would be noise that teaches
/// people to distrust the mechanism.
fn is_secret(name: &str, pschema: &Value) -> bool {
    if pschema.get("format").and_then(Value::as_str) == Some("password") {
        return true;
    }
    // Only a string can be a secret value. A boolean `secret` is a switch, an
    // integer `budgetTokens` is a budget, an array `tokenFields` is a schema.
    if pschema.get("type").and_then(Value::as_str) != Some("string") {
        return false;
    }
    let n: String = name.to_ascii_lowercase().chars().filter(char::is_ascii_alphanumeric).collect();

    /// Names that DESCRIBE a credential rather than carrying one.
    const NOT_THE_VALUE: &[&str] = &[
        "file", "path", "hash", "salt", "ref", "id", "name", "type", "format",
        "method", "url", "uri", "days", "options", "ttl", "count", "enabled",
        "attributes", "fields", "prefix", "algorithm", "scheme", "kind", "mode",
        "expiry", "expires", "at", "by", "signingmethod",
    ];
    if NOT_THE_VALUE.iter().any(|suf| n.ends_with(suf)) {
        return false;
    }

    /// What a secret is called, as a whole name or the tail of one
    /// (`clientSecret`, `accessSecret`, `apiKey`, `refreshToken`).
    const IS_THE_VALUE: &[&str] = &[
        "password", "passwd", "passphrase", "secret", "apikey", "privatekey",
        "credential", "token", "clientsecret", "accesskey", "secretkey",
    ];
    IS_THE_VALUE.iter().any(|w| n == *w || n.ends_with(w))
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

/// The ONE leaf classification: a resolved schema that is not an EXPANDABLE
/// object → (clap type, enum choices, repeatable). Shared by the body-property
/// walk and the query-parameter path, so a `string` means the same thing in the
/// URL and in the body.
///
/// An ARRAY is read through to its ELEMENT and marked repeatable: `--tag a --tag
/// b` instead of one opaque `--tag '["a","b"]'`. An array OF arrays has no
/// scalar element to repeat, so it stays one JSON value.
///
/// It derefs FIRST. The previous rule answered `Json` for any property written
/// as a `$ref`, whatever it referred to — so a shared enum was as opaque as a
/// nested object. What a schema IS does not depend on whether it was spelled
/// inline or by name.
fn classify(spec: &Value, pschema: &Value) -> (&'static str, Vec<String>, bool) {
    let d = deref(spec, pschema);
    let enum_vals: Vec<String> = d
        .get("enum")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect())
        .unwrap_or_default();
    match d.get("type").and_then(Value::as_str).unwrap_or("") {
        "string" => ("Str", enum_vals, false),
        "integer" => ("Int", vec![], false),
        "number" => ("Num", vec![], false),
        "boolean" => ("Bool", vec![], false),
        "array" => match d.get("items") {
            // The element decides the flag's type; the array only decides that
            // the flag repeats. A nested array cannot repeat into a flat list.
            Some(items) => match classify(spec, items) {
                (_, _, true) => ("Json", vec![], false),
                (t, c, false) => (t, c, true),
            },
            None => ("Json", vec![], false),
        },
        "object" => ("Json", vec![], false),
        _ if d.get("properties").is_some() => ("Json", vec![], false),
        // A schema stating nothing at all (`{}`) is a freeform value, not a string.
        _ if d.as_object().is_none_or(serde_json::Map::is_empty) => ("Json", vec![], false),
        _ => ("Str", vec![], false),
    }
}

/// The properties an object schema declares, plus the names it marks required,
/// flattening `allOf`. ONE reading, used at every level of the walk.
fn object_of(spec: &Value, schema: &Value) -> (Vec<(String, Value)>, BTreeSet<String>) {
    let s = deref(spec, schema);
    let mut props: Vec<(String, Value)> = Vec::new();
    let mut required: BTreeSet<String> = BTreeSet::new();
    let mut collect = |obj: &Value| {
        if let Some(r) = obj.get("required").and_then(Value::as_array) {
            required.extend(r.iter().filter_map(Value::as_str).map(str::to_string));
        }
        if let Some(p) = obj.get("properties").and_then(Value::as_object) {
            props.extend(p.iter().map(|(k, v)| (k.clone(), v.clone())));
        }
    };
    match s.get("allOf").and_then(Value::as_array) {
        Some(all) => all.iter().for_each(|sub| collect(deref(spec, sub))),
        None => collect(s),
    }
    (props, required)
}

/// Resolve a body schema into typed fields, or an empty vec for a freeform /
/// non-object body (→ `--data` fallback). Faithful to the schema — no invention.
fn fields_of(spec: &Value, schema: &Value) -> Vec<FieldDef> {
    let mut out = Vec::new();
    walk_object(spec, schema, "", true, 0, &mut Vec::new(), &mut out);
    out
}

/// Expand one object schema into flags, DESCENDING into a property that is
/// itself an object with declared properties: `spec.replicas` becomes
/// `--spec.replicas INT` instead of `--spec '<json>'`.
///
/// A nested leaf is required only when EVERY step to it is required — a flag
/// clap demands inside an object the caller never mentions would refuse a call
/// the server accepts.
///
/// `seen` is the `$ref` chain on the way down: a schema that refers to itself
/// (a tree, a linked node) would otherwise expand forever, so it stops there and
/// keeps its whole JSON value. `MAX_NEST` is the same guarantee for a schema
/// spelled inline. A branch that expands to NOTHING (an object whose properties
/// are all empty) falls back to one JSON flag rather than losing the property.
fn walk_object(
    spec: &Value,
    schema: &Value,
    prefix: &str,
    req_path: bool,
    depth: usize,
    seen: &mut Vec<String>,
    out: &mut Vec<FieldDef>,
) {
    let (props, required) = object_of(spec, schema);
    for (name, pschema) in props {
        let key = format!("{prefix}{name}");
        let req = req_path && required.contains(&name);
        let refname = pschema.get("$ref").and_then(Value::as_str).map(str::to_string);
        let d = deref(spec, &pschema);
        let expandable = d.get("properties").and_then(Value::as_object).is_some_and(|p| !p.is_empty())
            || d.get("allOf").is_some();
        let cycle = refname.as_ref().is_some_and(|r| seen.contains(r));
        if expandable && depth < MAX_NEST && !cycle {
            let before = out.len();
            if let Some(r) = refname.clone() {
                seen.push(r);
            }
            walk_object(spec, d, &format!("{key}."), req, depth + 1, seen, out);
            if refname.is_some() {
                seen.pop();
            }
            if out.len() > before {
                continue;
            }
        }
        let (ty, choices, repeat) = classify(spec, &pschema);
        out.push(FieldDef {
            flag: kebab(&key),
            key,
            ty,
            required: req,
            choices,
            query: false,
            secret: is_secret(&name, d),
            repeat,
        });
    }
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
        let (ty, choices, repeat) = match p.get("schema") {
            Some(schema) => classify(spec, schema),
            None => ("Str", vec![], false),
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
            repeat,
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
    /// WHY this write has no typed body, when it has none — read off the document
    /// at the moment the decision is made, so the census below can never be a
    /// second opinion about it. `""` for a read and for a typed write.
    untyped: &'static str,
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
                // The cap counts CHARACTERS, and `char_indices` is what makes it
                // one: a bare byte index can land inside a multi-byte char, and
                // slicing there aborts the generator instead of shortening a
                // sentence. clap lays this column out in characters anyway, so it
                // is also the number the cap meant.
                if let Some((cap, _)) = s.char_indices().nth(100) {
                    let cut = s[..cap].rfind(' ').unwrap_or(cap);
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

    // Addresses answering more than one verb. A terminal noun at one of these
    // cannot be the command's verb without collapsing every method onto one
    // coordinate — see `fold`.
    let multi: BTreeSet<String> = paths
        .iter()
        .filter(|(p, _)| p.starts_with("/v1/"))
        .filter(|(_, item)| {
            item.as_object()
                .map(|o| o.keys().filter(|m| VERBS.contains(&m.as_str())).count() > 1)
                .unwrap_or(false)
        })
        .map(|(p, _)| p.trim_end_matches('/').to_string())
        .collect();

    // THE CURATION LAW: the tables may speak ONLY of products the document carries.
    // A name the spec does not mention states one thing and one thing only — that
    // the server does not serve it — and that is `genspec`'s answer against cloud's
    // own API document, arrived at by construction, not a list kept here. Without
    // this, every entry decays into the "this one 404s" knowledge deleting it cost
    // a release.
    let carried: BTreeSet<&str> = all.iter().filter_map(|p| segs(p).get(1).copied()).collect();
    let stale: Vec<&str> =
        curation::CURATED.iter().map(|c| c.product).filter(|n| !carried.contains(n)).collect();
    assert!(
        stale.is_empty(),
        "curation names {} product(s) spec/cloud.json does not carry: {}\n\n\
         A curation entry is a choice about what a COMMAND is, and must still hold if the route were \
         served perfectly. A name no document mentions asserts only that the server does not serve it \
         — which `genspec` decides against cloud's own API document. Delete the entry.",
        stale.len(),
        stale.join(" ")
    );

    let mut raw: BTreeMap<(String, Vec<String>, String), Vec<Op>> = BTreeMap::new();
    let mut tunnels = 0usize;
    for (path, item) in paths {
        // A path key with a `?query` (AWS-S3-style sub-resource selectors) is
        // not a distinct RESOURCE — the query, not the path, distinguishes it.
        if !path.starts_with("/v1/") || path.contains('?') || path.contains('#') {
            continue;
        }
        let product0 = segs(path)[1];
        if curation::dropped(product0) || is_wild(product0) {
            continue;
        }
        for (m, op) in item.as_object().into_iter().flatten() {
            if !VERBS.contains(&m.as_str()) {
                continue;
            }
            let method = m.to_uppercase();
            let Some(f) = fold(&method, path, &all, &multi) else { continue };
            // Curation remap: absorb a product UNDER another as a sub-namespace
            // (e.g. `machines list` → `compute machines list`). The PATH is
            // unchanged — only the command coordinate moves.
            let (mut product, mut nodes) = (f.product, f.nodes);
            if let Some(parent) = curation::under(&product) {
                nodes.insert(0, std::mem::replace(&mut product, parent.to_string()));
            }
            // Typed flags: body properties (writes) + query parameters (all ops).
            let write = matches!(method.as_str(), "POST" | "PUT" | "PATCH");
            let schema = write.then(|| body_schema(&spec, op)).flatten();
            let mut fields = schema.map(|s| fields_of(&spec, s)).unwrap_or_default();
            // WHY, decided here and nowhere else. `no-schema` is a TYPING GAP IN
            // CLOUD — the handler declares no JSON requestBody, so the document
            // states no shape and there is no shape for this generator to read.
            // `freeform` is a body the schema deliberately leaves open (`{}`,
            // `additionalProperties`, `oneOf`, a bare array), where `--data` is
            // the honest answer rather than a fallback.
            let untyped = match (write, schema, fields.is_empty()) {
                (true, None, _) => "no-schema",
                (true, Some(_), true) => "freeform",
                _ => "",
            };
            fields.extend(query_fields(&spec, op));
            // A body property that repeats a PATH parameter is the SAME value,
            // already supplied as a positional. It appears twice because zip binds
            // the path segment into the same Go struct as the body, so the emitted
            // schema honestly carries both. A flag for it would be a second way to
            // say one thing — and one that cannot work alone, the positional being
            // required.
            //
            // UNLESS it is the body's ONLY property. Then the two cannot be one
            // value: dropping it leaves a declared schema sending `{}`, and a
            // command whose whole body is typed away falls back to `--data`, which
            // is strictly less than the schema stated. `POST
            // /v1/o11y/service_accounts/{id}/roles` is the case — its `{id}` is the
            // service account and its body `id` is the ROLE to assign — and it was
            // the one `--data` write in the tree that no cloud typing gap explains.
            let echo = |fd: &FieldDef| f.params.contains(&fd.key);
            let body_survives = fields.iter().any(|fd| !fd.query && !echo(fd));
            fields.retain(|fd| !echo(fd) || (!body_survives && !fd.query));
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
            // A METHOD-OVERRIDE TUNNEL IS NOT A COMMAND. cloud registers a POST
            // beside the real verb at some addresses so a browser form — which can
            // only send GET and POST — can still reach a PUT/PATCH/DELETE handler.
            // Its own summary says exactly that. A CLI sends the real verb, so the
            // tunnel reaches nothing this tree does not already reach at the verb
            // it tunnels for, and generating it would put two spellings of one
            // command in the surface.
            //
            // This is a DROP, not an elision: nothing is lost, so it must not be
            // counted as loss. It is counted separately and written into the
            // generated header, because a number that changes without anyone
            // noticing is how the elision census went unread in the first place.
            if sum.starts_with("Method-override tunnel") {
                tunnels += 1;
                continue;
            }
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
                untyped,
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

    // Collapse multi-method coordinates by priority (one op per command) — and
    // COUNT what that costs. The fold names a leaf after its own noun, so two
    // methods at one address want one name: `PUT` and `PATCH` on an item both
    // read as `update`, `GET` and `POST` on `/v1/billing/payment-methods` both
    // read as `payment-methods`. One of them wins and the other reaches nobody.
    //
    // "Is a `PUT` beside a `PATCH` a different command, or the same one?" used to
    // stand here as a question this generator could not answer. It was never the
    // generator's to answer, and it was never open: cloud states it, per address,
    // in the prose it publishes. `PUT /v1/store/{storeid}` is "Replace a storefront
    // outright"; `PATCH` is "Change part of a storefront". Two commands, said in
    // the contract, folded into one name by a table here — so `root_verb` and
    // `item_verb` now separate them, and the same reading retired the 18
    // method-override tunnels, which cloud's own summary calls a shim for clients
    // that cannot send the real verb.
    //
    // What survives the census is genuinely undecided, and the rule is unchanged:
    // an operation that vanishes without a number is exactly the shape of defect
    // this pipeline exists to end. So the loss is reported, and pinned as a
    // CEILING — free to fall, and it cannot grow without somebody deciding.
    let mut coords: Vec<Op> = Vec::new();
    let mut elided: Vec<Op> = Vec::new();
    for (_c, mut ops) in resolved {
        ops.sort_by_key(|o| method_rank(&o.method));
        let arities: BTreeSet<usize> = ops.iter().map(|o| o.params.len()).collect();
        assert!(
            arities.len() == 1,
            "unresolved arity collision: {:?}",
            ops.iter().map(|o| &o.path).collect::<Vec<_>>()
        );
        let mut it = ops.into_iter();
        coords.push(it.next().expect("a coordinate holds at least one op"));
        elided.extend(it);
    }
    elided.sort_by(|a, b| (&a.path, &a.method).cmp(&(&b.path, &b.method)));
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

    // The elision census, read beside the ops it stands for. It is printed every
    // run and written into the generated header, because a gap nobody prints is a
    // gap nobody closes.
    let by_product = elided.iter().fold(BTreeMap::<&str, usize>::new(), |mut m, o| {
        *m.entry(o.product.as_str()).or_default() += 1;
        m
    });
    let worst = {
        let mut v: Vec<_> = by_product.iter().collect();
        v.sort_by_key(|(p, n)| (std::cmp::Reverse(**n), **p));
        v.into_iter()
            .take(8)
            .map(|(p, n)| format!("{p} {n}"))
            .collect::<Vec<_>>()
            .join(", ")
    };
    eprintln!(
        "genproduct: {} served operation(s) share a coordinate with a higher-priority method and \
         reach no command ({worst}{})",
        elided.len(),
        if by_product.len() > 8 { ", …" } else { "" }
    );
    eprintln!(
        "genproduct: {tunnels} method-override tunnel(s) dropped — a POST cloud registers so a \
         browser form can reach a PUT/PATCH/DELETE handler. Nothing is lost: a CLI sends the real \
         verb, and that command is in the tree."
    );

    // THE UNTYPED CENSUS. A `--data` write is the CLI saying it does not know the
    // shape, and until now the only number anywhere was one aggregate in the
    // generated header, which cannot say WHOSE gap it is. It is two different
    // facts and they have two different owners:
    //
    //   no-schema  the handler in hanzoai/cloud declares no JSON requestBody, so
    //              the document states no shape. NOTHING IN THIS REPO CAN CLOSE
    //              IT — the fix is a typed op where the handler lives (#67).
    //   freeform   the schema is open BY CONSTRUCTION (`{}`, additionalProperties,
    //              oneOf, a bare array). `--data` is the honest answer, not a
    //              fallback, and typing it would be inventing a shape.
    //
    // Printed every run, and the per-product split is the hand-off list, so the
    // number a cloud owner needs is the generator's own output rather than an
    // analysis somebody has to redo.
    let untyped: Vec<&Op> = coords.iter().filter(|o| !o.untyped.is_empty()).collect();
    let no_schema = untyped.iter().filter(|o| o.untyped == "no-schema").count();
    let gap_by_product = untyped.iter().filter(|o| o.untyped == "no-schema").fold(
        BTreeMap::<&str, usize>::new(),
        |mut m, o| {
            *m.entry(o.product.as_str()).or_default() += 1;
            m
        },
    );
    let gap_worst = {
        let mut v: Vec<_> = gap_by_product.iter().collect();
        v.sort_by_key(|(p, n)| (std::cmp::Reverse(**n), **p));
        v.into_iter().take(8).map(|(p, n)| format!("{p} {n}")).collect::<Vec<_>>().join(", ")
    };
    eprintln!(
        "genproduct: {} write command(s) take --data — {no_schema} because the handler declares no \
         requestBody in hanzoai/cloud ({gap_worst}{}), {} because the schema is freeform by \
         construction",
        untyped.len(),
        if gap_by_product.len() > 8 { ", …" } else { "" },
        untyped.len() - no_schema,
    );

    // ---- emit ----
    // `spec/cloud.json` is the ONLY source: every cloud capability is a real
    // `hanzo <product> <resource> <verb>`. A product the spec does not carry is a
    // product no cloud router registers, because the spec is a projection of
    // cloud's own emitted document and of nothing else. There is no passthrough
    // and no `hanzo api` fallback to paper over it: that gap closes by SERVING
    // the route, which is the only place it can close.
    let ntyped = coords.iter().filter(|o| !o.fields.is_empty()).count();
    let ndata = coords
        .iter()
        .filter(|o| o.fields.is_empty() && matches!(o.method.as_str(), "POST" | "PUT" | "PATCH"))
        .count();
    let nprod = coords.iter().map(|o| &o.product).collect::<BTreeSet<_>>().len();

    let mut s = String::new();
    s.push_str("//! @generated by `cargo run --bin genproduct` from `spec/cloud.json`\n");
    s.push_str("//! (itself a projection of hanzoai/cloud's emitted openapi.yaml, and of nothing else).\n");
    s.push_str("//! DO NOT EDIT BY HAND — `cargo test` regenerates and diffs this.\n//!\n");
    s.push_str("//! Pure DATA: (product, resource nodes, verb, method, /v1 path, params, typed\n");
    s.push_str("//! body fields). No host, no absolute URL, no auth — pinned by a test.\n//!\n");

    // THE CENSUS RIDES IN THE FILE IT DESCRIBES. Every number here was once a
    // `const` a person edited by hand on each re-pin, so the pipeline that re-pins
    // nightly could not land one: the constants move whenever cloud moves, and a
    // machine has no hand. They are DERIVED facts about the pinned document, and a
    // derived fact belongs in the projection — where `genproduct --check` already
    // diffs this file byte for byte, so a census that moves turns CI red and shows
    // the delta in the diff a person reads. One mechanism, and it needs no editing
    // to stay true.
    s.push_str("//! Census of this derivation, against the document `.spec-lock` names:\n");
    s.push_str(&format!(
        "//!   {} write command(s) take --data — {no_schema} because the handler in hanzoai/cloud\n\
         //!   declares no requestBody ({gap_worst}{}), {} because the schema is freeform by\n\
         //!   construction. Only the first is a gap, and it closes where the handler lives.\n",
        untyped.len(),
        if gap_by_product.len() > 8 { ", …" } else { "" },
        untyped.len() - no_schema,
    ));
    s.push_str(&format!(
        "//!   {tunnels} method-override tunnel(s) dropped — a POST cloud registers so a browser\n\
         //!   form can reach a PUT/PATCH/DELETE handler. Nothing is lost: a CLI sends the real verb.\n",
    ));
    s.push_str(&format!(
        "//!   {} served operation(s) share a coordinate with a higher-priority method and reach\n\
         //!   no command. Two methods at one address want one command name, so one of them reaches\n\
         //!   nobody; the fold's `has_child` branch is how a second one gets a name of its own.\n",
        elided.len(),
    ));
    for o in &elided {
        s.push_str(&format!("//!     {} {}\n", o.method, o.path));
    }
    s.push_str("\n");
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
                    "Field {{ key: {:?}, id: {:?}, flag: {:?}, ty: Ty::{}, required: {}, choices: {}, query: {}, secret: {}, repeat: {} }}",
                    f.key,
                    id,
                    f.flag,
                    f.ty,
                    f.required,
                    emit_slice(&f.choices),
                    f.query,
                    f.secret,
                    f.repeat
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

#[cfg(test)]
mod tests {
    use super::is_secret;
    use serde_json::json;

    fn s(name: &str, ty: &str) -> bool {
        is_secret(name, &json!({ "type": ty }))
    }

    /// The standard marker still decides, on its own, whatever the field is called.
    #[test]
    fn the_openapi_marker_is_honoured_first() {
        assert!(is_secret("anything", &json!({ "type": "string", "format": "password" })));
    }

    /// …and is not TRUSTED on its own. Cloud's document carries `format: password`
    /// on zero fields while serving sign-in passwords, client secrets, API keys and
    /// bearer tokens. When the single upstream marker went away, so did every
    /// protection that depended on it — silently, which is the failure this exists
    /// to make impossible.
    #[test]
    fn a_credential_is_recognised_by_name_too() {
        for name in [
            "password", "newPassword", "oldPassword", "defaultPassword", "masterPassword",
            "secret", "clientSecret", "accessSecret", "token", "accessToken",
            "refreshToken", "apiKey", "privateKey", "passphrase", "credential",
        ] {
            assert!(s(name, "string"), "`{name}` carries a credential and must go to stdin");
        }
    }

    /// A name that DESCRIBES a credential is not one. Forcing these onto stdin
    /// would be noise, and noise is what teaches people to distrust the mechanism.
    #[test]
    fn naming_a_credential_is_not_carrying_one() {
        for name in [
            "passwordSalt", "passwordHash", "password_file", "tokenFormat",
            "tokenSigningMethod", "secretRef", "credentialId", "tokenPrefix",
            "passwordExpireDays", "secretName", "tokenUrl",
        ] {
            assert!(!s(name, "string"), "`{name}` names a credential without being one");
        }
    }

    /// Only a string can BE the value: a boolean `secret` is a switch, an integer
    /// `budgetTokens` is a budget, an array `tokenFields` is a schema.
    #[test]
    fn only_a_string_can_be_the_value() {
        for (name, ty) in [("secret", "boolean"), ("budgetTokens", "integer"), ("tokenFields", "array")] {
            assert!(!s(name, ty), "`{name}: {ty}` is not a secret value");
        }
    }
}
