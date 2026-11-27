use std::collections::BTreeSet;
use serde_json::Value;

pub(super) const VERBS: [&str; 5] = ["get", "post", "put", "patch", "delete"];
pub(super) const METHOD_PRIORITY: [&str; 5] = ["PATCH", "PUT", "POST", "DELETE", "GET"];

pub(super) fn is_param(s: &str) -> bool {
    s.starts_with('{') && s.ends_with('}')
}
pub(super) fn is_wild(s: &str) -> bool {
    is_param(s) && (s.contains("wild") || s.contains('*'))
}
pub(super) fn pname(s: &str) -> &str {
    s.trim_start_matches('{').trim_end_matches('}')
}
pub(super) fn segs(p: &str) -> Vec<&str> {
    p.split('/').filter(|s| !s.is_empty()).collect()
}

/// camelCase / snake_case → kebab-case for a flag long name.
pub(super) fn kebab(s: &str) -> String {
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

pub(super) fn has_child(p: &str, all: &BTreeSet<String>) -> bool {
    let pre = format!("{}/", p.trim_end_matches('/'));
    all.iter()
        .any(|k| k != p && k.trim_end_matches('/').starts_with(&pre))
}
/// A collection is a path whose MEMBERS are addressed by appending a `{param}`.
/// The member path need not STOP there: `/v1/admin/plugins/{name}/reload` selects
/// one plugin exactly as `/v1/iam/users/{id}` selects one user, so the read at the
/// root is a `list` either way. Requiring the param to be the last segment would
/// call `plugins` a singular resource on the evidence that its members happen to
/// have sub-actions rather than a bare GET.
pub(super) fn is_collection(p: &str, all: &BTreeSet<String>) -> bool {
    let ps = segs(p);
    all.iter().any(|k| {
        let ks = segs(k);
        ks.len() > ps.len() && ks[..ps.len()] == ps[..] && is_param(ks[ps.len()])
    })
}
pub(super) fn cmd_tokens(sg: &[&str]) -> Vec<String> {
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
pub(super) fn root_verb(method: &str, coll: bool) -> &'static str {
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
pub(super) fn item_verb(method: &str) -> &'static str {
    match method {
        "GET" => "get",
        "DELETE" => "rm",
        "PUT" => "set",
        "PATCH" => "update",
        _ => "add",
    }
}

pub(super) struct Folded {
    pub(super) product: String,
    pub(super) nodes: Vec<String>,
    pub(super) verb: String,
    pub(super) params: Vec<String>,
}

/// `multi` is the set of paths carrying MORE THAN ONE verb. A terminal noun
/// normally becomes the command's verb (`GET /v1/websearch/search` →
/// `websearch search`), which is right for an address with one method and lossy
/// for an address with several: every method folds to the same coordinate and all
/// but one reach nobody. Those addresses take the `has_child` shape instead.
pub(super) fn fold(
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
    let params: Vec<String> = ct
        .iter()
        .filter(|s| is_param(s))
        .map(|s| pname(s).to_string())
        .collect();
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
    Some(Folded {
        product,
        nodes,
        verb,
        params,
    })
}

// ---- typed field extraction -------------------------------------------------

/// How deep a nested body object is expanded into dotted flags (`--a.b.c`).
/// Beyond it a property keeps its whole JSON value in one flag, which is where
/// every object property used to land. Measured against this document the cap
/// SATURATES at 3 — caps of 3, 4 and 8 all derive the identical field set,
/// because the `$ref` cycle guard stops the recursive schemas first — so it is a
/// termination guarantee rather than a policy about the surface.
pub(super) const MAX_NEST: usize = 3;

#[derive(Clone)]
pub(super) struct FieldDef {
    /// The body key, DOTTED for a nested property (`spec.replicas`). No schema
    /// property name in this document contains a `.` (measured: 0 of 3751), so
    /// the path is unambiguous and the runtime rebuilds the object from it.
    pub(super) key: String,
    pub(super) flag: String,
    pub(super) ty: &'static str, // Str|Int|Num|Bool|Json|Union
    pub(super) required: bool,
    pub(super) choices: Vec<String>,
    /// A query-string parameter (goes in the URL), vs a requestBody property.
    pub(super) query: bool,
    /// A SECRET body value: read from stdin, NEVER a flag — so it can never land
    /// in argv, `ps` or shell history. Decided by `is_secret`, which honours
    /// `format: password` first and falls back to the NAME, because the document
    /// carries that format on zero fields. The runtime reads it through
    /// `iam::secret::read_secret`.
    pub(super) secret: bool,
    /// An ARRAY: the flag may be given more than once and the values collect into
    /// a JSON array (`--tag a --tag b` → `["a","b"]`). `ty` is then the ELEMENT's
    /// type, so an array of strings is a repeatable `--flag STRING` rather than
    /// one opaque `--flag '["a","b"]'`.
    pub(super) repeat: bool,
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
pub(super) fn is_secret(path: &str, name: &str, pschema: &Value) -> bool {
    if pschema.get("format").and_then(Value::as_str) == Some("password") {
        return true;
    }
    // Only a string can be a secret value. A boolean `secret` is a switch, an
    // integer `budgetTokens` is a budget, an array `tokenFields` is a schema.
    if pschema.get("type").and_then(Value::as_str) != Some("string") {
        return false;
    }
    // The operation is evidence too. Three operations store a sealed value
    // under a field called `value` (`PUT /v1/environment/{repo}/secrets/{name}`,
    // `POST /v1/kms/secrets`, `PUT /v1/notify/credentials/{provider}/{key}`), a
    // name no name rule can catch: a `value` written to a `secrets` or
    // `credentials` route is the secret. Body fields only — a query field never
    // gets here.
    if name == "value"
        && path
            .split('/')
            .any(|seg| matches!(seg, "secrets" | "credentials"))
    {
        return true;
    }
    let n: String = name
        .to_ascii_lowercase()
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .collect();

    /// Names that DESCRIBE a credential rather than carrying one.
    const NOT_THE_VALUE: &[&str] = &[
        "file",
        "path",
        "hash",
        "salt",
        "ref",
        "id",
        "name",
        "type",
        "format",
        "method",
        "url",
        "uri",
        "days",
        "options",
        "ttl",
        "count",
        "enabled",
        "attributes",
        "fields",
        "prefix",
        "algorithm",
        "scheme",
        "kind",
        "mode",
        "expiry",
        "expires",
        "at",
        "by",
        "signingmethod",
    ];
    if NOT_THE_VALUE.iter().any(|suf| n.ends_with(suf)) {
        return false;
    }

    /// What a secret is called, as a whole name or the tail of one
    /// (`clientSecret`, `accessSecret`, `apiKey`, `refreshToken`).
    const IS_THE_VALUE: &[&str] = &[
        "password",
        "passwd",
        "passphrase",
        "secret",
        "apikey",
        "privatekey",
        "credential",
        "token",
        "clientsecret",
        "accesskey",
        "secretkey",
    ];
    IS_THE_VALUE.iter().any(|w| n == *w || n.ends_with(w))
}

pub(super) fn deref<'a>(spec: &'a Value, v: &'a Value) -> &'a Value {
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
pub(super) fn body_schema<'a>(spec: &'a Value, op: &'a Value) -> Option<&'a Value> {
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
pub(super) fn classify(spec: &Value, pschema: &Value) -> (&'static str, Vec<String>, bool) {
    let d = deref(spec, pschema);
    let enum_vals: Vec<String> = d
        .get("enum")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
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
        // A UNION of variants (`anyOf`/`oneOf`). One that admits a string takes
        // the value as TEXT unless it reads as a JSON object or array, which is
        // what `state: string | object | array` on /v1/decisions says; one with
        // no string variant is a JSON value. It used to fall through to `Str`,
        // so `--state '{"message":"…"}'` reached the server as a quoted string.
        _ if d.get("anyOf").or_else(|| d.get("oneOf")).is_some() => {
            let variants = d
                .get("anyOf")
                .or_else(|| d.get("oneOf"))
                .and_then(Value::as_array);
            let text = variants.is_some_and(|vs| {
                vs.iter()
                    .any(|v| deref(spec, v).get("type").and_then(Value::as_str) == Some("string"))
            });
            (if text { "Union" } else { "Json" }, vec![], false)
        }
        _ if d.get("properties").is_some() => ("Json", vec![], false),
        // A schema stating nothing at all (`{}`) is a freeform value, not a string.
        _ if d.as_object().is_none_or(serde_json::Map::is_empty) => ("Json", vec![], false),
        _ => ("Str", vec![], false),
    }
}

/// The properties an object schema declares, plus the names it marks required,
/// flattening `allOf`. ONE reading, used at every level of the walk.
pub(super) fn object_of(spec: &Value, schema: &Value) -> (Vec<(String, Value)>, BTreeSet<String>) {
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
pub(super) fn fields_of(spec: &Value, path: &str, schema: &Value) -> Vec<FieldDef> {
    let mut out = Vec::new();
    walk_object(spec, path, schema, "", true, 0, &mut Vec::new(), &mut out);
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
#[allow(clippy::too_many_arguments)]
pub(super) fn walk_object(
    spec: &Value,
    path: &str,
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
        let refname = pschema
            .get("$ref")
            .and_then(Value::as_str)
            .map(str::to_string);
        let d = deref(spec, &pschema);
        let expandable = d
            .get("properties")
            .and_then(Value::as_object)
            .is_some_and(|p| !p.is_empty())
            || d.get("allOf").is_some();
        let cycle = refname.as_ref().is_some_and(|r| seen.contains(r));
        if expandable && depth < MAX_NEST && !cycle {
            let before = out.len();
            if let Some(r) = refname.clone() {
                seen.push(r);
            }
            walk_object(spec, path, d, &format!("{key}."), req, depth + 1, seen, out);
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
            secret: is_secret(path, &name, d),
            repeat,
        });
    }
}

/// Typed flags from an operation's `parameters` array: the `in: query` params
/// become query `--flags` (`in: path` params are already positionals from the
/// path template, so they are skipped here). $ref params resolve to their shared
/// definition. Applies to reads AND writes.
pub(super) fn query_fields(spec: &Value, op: &Value) -> Vec<FieldDef> {
    let Some(params) = op.get("parameters").and_then(Value::as_array) else {
        return vec![];
    };
    let mut out = Vec::new();
    for p in params {
        let p = deref(spec, p);
        if p.get("in").and_then(Value::as_str) != Some("query") {
            continue;
        }
        let Some(name) = p.get("name").and_then(Value::as_str) else {
            continue;
        };
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

pub(super) struct Op {
    pub(super) product: String,
    pub(super) nodes: Vec<String>,
    pub(super) verb: String,
    pub(super) method: String,
    pub(super) path: String,
    pub(super) params: Vec<String>,
    /// The subset of `params` that are multi-segment (catch-all) — see REST_PARAMS.
    pub(super) rest: Vec<String>,
    pub(super) fields: Vec<FieldDef>,
    /// One line of prose from the spec (a typed op's doc comment, lifted by
    /// zipdoc and carried through genspec). Empty when the op is undescribed.
    pub(super) sum: String,
    /// WHY this write has no typed body, when it has none — read off the document
    /// at the moment the decision is made, so the census below can never be a
    /// second opinion about it. `""` for a read and for a typed write.
    pub(super) untyped: &'static str,
}

pub(super) fn method_rank(m: &str) -> usize {
    METHOD_PRIORITY
        .iter()
        .position(|x| *x == m)
        .unwrap_or(usize::MAX)
}

