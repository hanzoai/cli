//! `hanzo <product> <resource…> <verb>` — cloud's Go surface as FIRST-CLASS
//! product commands, generated from the hand-authored OpenAPI specs. This is the
//! ONLY interface to cloud: every capability is a real subcommand — there is no
//! `hanzo api` verb and no raw-path escape.
//!
//! A build-time generator folds each authored path into a (product, resource
//! nodes, verb, method, path template, params, typed body fields) coordinate and
//! commits it as pure DATA (`generated.rs`). At runtime we build a clap tree from
//! that data and dispatch it through the one authenticated seam below: the ORIGIN
//! comes from `network`, the BEARER from `store`, and the data contributes only
//! the path template + argument SHAPE. The data contains no host, no URL and no
//! auth (a test fails the build otherwise), so a hostile snapshot can at worst
//! shape a call to YOUR OWN cloud with YOUR OWN token — never redirect it.
//!
//! Because the source specs carry real requestBody schemas, a write op with a
//! schema gets TYPED `--flags` (one per property, with the property's type and
//! required-ness) and the JSON body is assembled from them — not `--data`. A write
//! with NO schema (or a freeform body) falls back to `--data '<json>'`. A product
//! with no authored spec is simply ABSENT (no passthrough, no `hanzo api` to paper
//! over it — that gap closes by authoring the spec). Nothing is invented — the
//! fields are exactly the schema's properties.

use anyhow::{anyhow, Context, Result};
use clap::{Arg, ArgAction, ArgMatches, Command};
use reqwest::{Client, Method, StatusCode};
use serde_json::{json, Map, Value};
use std::io::Read;

use crate::commands::network;
use crate::config::Config;
use crate::http;
use crate::iam::{paths, store};

mod generated;
pub(crate) use generated::OPS;

/// One generated operation — pure DATA derived from the authored spec SHAPE.
pub struct Op {
    /// First path segment after `/v1/` — the top-level command.
    pub product: &'static str,
    /// Resource group chain (nouns), verb excluded — the subcommand path.
    pub nodes: &'static [&'static str],
    /// The leaf verb (`list`/`get`/`create`/`update`/`rm`, or an action name).
    pub verb: &'static str,
    /// The one HTTP method this coordinate dispatches with.
    pub method: &'static str,
    /// The `/v1` path template, e.g. `/v1/agents/sessions/{id}/events`.
    pub path: &'static str,
    /// User positionals, in path order — the tenant-org scope is NOT among them.
    pub params: &'static [&'static str],
    /// Typed request-body fields from the requestBody schema. Empty ⇒ no typed
    /// body (a write with no schema uses `--data`; a read has no body).
    pub fields: &'static [Field],
}

/// One typed request-body field, from a schema property.
pub struct Field {
    /// The JSON property name — the body key sent to cloud.
    pub key: &'static str,
    /// The clap arg id — namespaced (`field.<key>`) so it can never collide with a
    /// path positional or a fixed control, even when a body key is `data`/`id`.
    pub id: &'static str,
    /// The kebab-case long flag the user types (`amountCents` → `--amount-cents`).
    pub flag: &'static str,
    /// The property's type, which picks the clap parser + the JSON encoding.
    pub ty: Ty,
    /// A `required` property is a required flag; else it is omitted when unset.
    pub required: bool,
    /// A string enum's allowed values (clap validates); empty otherwise.
    pub choices: &'static [&'static str],
}

/// The JSON type of a body field — schema `type` mapped to a clap parser.
pub enum Ty {
    Str,
    Int,
    Num,
    Bool,
    Json,
}

impl Op {
    fn is_write(&self) -> bool {
        matches!(self.method, "POST" | "PUT" | "PATCH")
    }
}

// ---- build the clap tree (BUILDER api — 130 products can't be a derive enum) --

/// Add every generated product to `cmd` as a first-class top-level command.
///
/// Collisions resolve ONE way: a LOCAL command owns its bare name. The generator
/// already omits the hand-written products (`kms`/`billing`/`agent`/`deploy`), and
/// this also skips any name the derive tree already took — so a future spec
/// addition that collides is auto-excluded rather than clobbering an invariant.
pub fn augment(mut cmd: Command) -> Command {
    let taken: std::collections::HashSet<String> =
        cmd.get_subcommands().map(|s| s.get_name().to_string()).collect();

    for product in product_names() {
        if taken.contains(product) {
            continue;
        }
        cmd = cmd.subcommand(build_product(product));
    }
    cmd
}

/// Distinct product names in `OPS`, stable order.
fn product_names() -> Vec<&'static str> {
    let mut seen = std::collections::HashSet::new();
    OPS.iter().map(|o| o.product).filter(|p| seen.insert(*p)).collect()
}

/// A trie node: children are subcommands; `leaf` marks a terminal verb. No node
/// is ever both — the fold is proven free of group/leaf conflicts.
#[derive(Default)]
struct Node {
    children: std::collections::BTreeMap<&'static str, Node>,
    leaf: Option<&'static Op>,
}

fn build_product(product: &'static str) -> Command {
    let mut root = Node::default();
    for op in OPS.iter().filter(|o| o.product == product) {
        let mut n = &mut root;
        for &g in op.nodes {
            n = n.children.entry(g).or_default();
        }
        n.children.entry(op.verb).or_default().leaf = Some(op);
    }
    to_command(product, &root)
}

fn to_command(name: &'static str, node: &Node) -> Command {
    if let Some(op) = node.leaf {
        return leaf(op);
    }
    let mut c = Command::new(name)
        .about(format!("`{name}` cloud operations"))
        .subcommand_required(true)
        .arg_required_else_help(true);
    for (child, sub) in &node.children {
        c = c.subcommand(to_command(child, sub));
    }
    c
}

/// A leaf command: positionals for the path params, then EITHER typed body flags
/// (when the schema is known) OR the raw `--data` escape (when it is not). Typed
/// leaves carry ONLY their body flags — never `--data`/`--query`/`--raw`, so a
/// body field named `data`/`query`/`raw` can never collide with a fixed control.
fn leaf(op: &'static Op) -> Command {
    let mut c = Command::new(op.verb).about(format!("{} {}", op.method, op.path));
    for &p in op.params {
        c = c.arg(Arg::new(p).required(true).help(format!("path parameter {{{p}}}")));
    }
    if !op.fields.is_empty() {
        for f in op.fields {
            c = c.arg(field_arg(f));
        }
    } else if op.is_write() {
        c = c.arg(data_arg()).arg(query_arg()).arg(raw_arg());
    } else {
        c = c.arg(query_arg()).arg(raw_arg());
    }
    c
}

/// A typed body flag from a schema property. The clap parser matches the JSON
/// type, so the assembled body carries the right type — a number is a number, a
/// bool a bool — not a stringly-typed `--data` blob.
fn field_arg(f: &'static Field) -> Arg {
    // The clap id is namespaced; the value placeholder shows the TYPE, so the id
    // (`field.act`) never leaks into `--help`.
    let mut a = Arg::new(f.id).long(f.flag).required(f.required).help(field_help(f));
    match f.ty {
        Ty::Int => a = a.value_parser(clap::value_parser!(i64)).value_name("INT"),
        Ty::Num => a = a.value_parser(clap::value_parser!(f64)).value_name("NUMBER"),
        Ty::Bool => a = a.action(ArgAction::SetTrue),
        Ty::Json => a = a.value_parser(parse_json).value_name("JSON"),
        Ty::Str => a = a.value_name("STRING"),
    }
    if !f.choices.is_empty() {
        a = a.value_parser(clap::builder::PossibleValuesParser::new(f.choices)).value_name("ENUM");
    }
    a
}

/// Type-derived help — DATA, never the spec's prose (which could carry a URL).
fn field_help(f: &Field) -> String {
    if !f.choices.is_empty() {
        return format!("one of: {}", f.choices.join(" | "));
    }
    let t = match f.ty {
        Ty::Str => "string",
        Ty::Int => "integer",
        Ty::Num => "number",
        Ty::Bool => "flag",
        Ty::Json => "JSON value",
    };
    if f.required {
        format!("{t} (required)")
    } else {
        t.to_string()
    }
}

/// Parse a `Json`-typed flag's value at the clap layer, so an invalid JSON body
/// field is a named parse error, not a silent malformed request.
fn parse_json(s: &str) -> std::result::Result<Value, String> {
    serde_json::from_str(s).map_err(|e| format!("not valid JSON: {e}"))
}

fn data_arg() -> Arg {
    Arg::new("data")
        .long("data")
        .value_name("JSON")
        .help("JSON request body; `-` reads it from stdin so a secret never lands in argv")
}
fn query_arg() -> Arg {
    Arg::new("query")
        .long("query")
        .value_name("K=V")
        .action(ArgAction::Append)
        .help("Append a query parameter, `k=v` (repeatable). Values are encoded")
}
fn raw_arg() -> Arg {
    Arg::new("raw")
        .long("raw")
        .action(ArgAction::SetTrue)
        .help("Print the whole {status,msg,data} envelope instead of just data")
}

// ---- resolve a parse into a call, then dispatch through the ONE seam ---------

/// What a matched generated command resolves to. Pure over the clap matches —
/// no config, no keychain — so it is unit-testable without a network.
pub enum Resolved {
    Leaf {
        op: &'static Op,
        values: Vec<String>,
        body: LeafBody,
        query: Vec<String>,
        raw: bool,
    },
}

/// A leaf's request body: assembled from typed flags, read raw from `--data`, or
/// absent (a read). The three tiers of the fallback ladder, resolved once.
pub enum LeafBody {
    Typed(Value),
    Data(Option<String>),
    None,
}

/// If `matches` selected a GENERATED product, resolve it; otherwise `None` so the
/// derive tree handles it (a local command, or a truly-bare `hanzo`).
pub fn resolve(matches: &ArgMatches) -> Option<Resolved> {
    let (top, sub) = matches.subcommand()?;
    if !is_product(top) {
        return None;
    }

    let mut chain: Vec<&str> = vec![top];
    let mut m = sub;
    while let Some((n, mm)) = m.subcommand() {
        chain.push(n);
        m = mm;
    }
    let op = find_op(&chain)?;
    let values = op
        .params
        .iter()
        .map(|p| m.get_one::<String>(p).cloned().unwrap_or_default())
        .collect();
    // The three body tiers, resolved once. A typed leaf carries ONLY its body
    // flags (no `--query`/`--raw`), so those are read only off the other tiers.
    let (body, query, raw) = if !op.fields.is_empty() {
        (LeafBody::Typed(typed_body(op, m)), Vec::new(), false)
    } else {
        let data = op.is_write().then(|| m.get_one::<String>("data").cloned()).flatten();
        let query = m.get_many::<String>("query").map(|v| v.cloned().collect()).unwrap_or_default();
        let body = if op.is_write() { LeafBody::Data(data) } else { LeafBody::None };
        (body, query, m.get_flag("raw"))
    };
    Some(Resolved::Leaf { op, values, body, query, raw })
}

/// Assemble the JSON body from the typed flags actually provided — nothing else.
/// An unset optional field is OMITTED (so the server's own default stands), never
/// sent as null. Each value is encoded at its schema type.
fn typed_body(op: &Op, m: &ArgMatches) -> Value {
    let mut map = Map::new();
    for f in op.fields {
        match f.ty {
            Ty::Str => {
                if let Some(v) = m.get_one::<String>(f.id) {
                    map.insert(f.key.to_string(), json!(v));
                }
            }
            Ty::Int => {
                if let Some(v) = m.get_one::<i64>(f.id) {
                    map.insert(f.key.to_string(), json!(v));
                }
            }
            Ty::Num => {
                if let Some(v) = m.get_one::<f64>(f.id) {
                    map.insert(f.key.to_string(), json!(v));
                }
            }
            Ty::Bool => {
                if m.get_flag(f.id) {
                    map.insert(f.key.to_string(), json!(true));
                }
            }
            Ty::Json => {
                if let Some(v) = m.get_one::<Value>(f.id) {
                    map.insert(f.key.to_string(), v.clone());
                }
            }
        }
    }
    Value::Object(map)
}

fn is_product(name: &str) -> bool {
    OPS.iter().any(|o| o.product == name)
}

fn find_op(chain: &[&str]) -> Option<&'static Op> {
    let product = *chain.first()?;
    let verb = *chain.last()?;
    if chain.len() < 2 {
        return None;
    }
    let nodes = &chain[1..chain.len() - 1];
    OPS.iter().find(|o| o.product == product && o.verb == verb && o.nodes == nodes)
}

/// Bind the resolved call to a concrete request and send it through the one seam.
/// The org scope is filled from the active identity's `owner` (via the seam),
/// never asked; all other params are the user's positionals. The bearer + origin
/// are `call`'s to resolve.
pub async fn dispatch(cfg: &mut Config, resolved: Resolved) -> Result<()> {
    let Resolved::Leaf { op, values, body, query, raw } = resolved;
    let owner = store::active(cfg, paths::DEFAULT_BRAND).map(|i| i.owner);
    let path = fill_path(op.path, owner.as_deref(), &values)?;
    let method = parse_method(op.method)?;
    let body = match body {
        LeafBody::Typed(v) => Some(v),
        LeafBody::Data(d) => read_body(d, &method)?,
        LeafBody::None => None,
    };
    call(cfg, method, path, body, query, raw).await
}

/// Fill a `/v1` template: a param preceded by `orgs` is the tenant scope, bound
/// to `owner` (refused when signed out); every other param takes the next
/// positional. Values are percent-encoded so a value cannot re-address a
/// different route — the same rule `kms` uses. This mirrors the generator's
/// scope predicate; their agreement is pinned by `every_op_fills_to_a_path`.
fn fill_path(template: &str, owner: Option<&str>, values: &[String]) -> Result<String> {
    let raw: Vec<&str> = template.split('/').collect();
    let mut out = String::new();
    let mut it = values.iter();
    for (i, seg) in raw.iter().enumerate() {
        if i > 0 {
            out.push('/');
        }
        match seg.strip_prefix('{').and_then(|s| s.strip_suffix('}')) {
            Some(name) => {
                let prev = if i > 0 { raw[i - 1] } else { "" };
                if prev == "orgs" {
                    let o = owner.ok_or_else(|| anyhow!("not signed in — run `hanzo login`"))?;
                    out.push_str(&enc(o));
                } else {
                    let v = it.next().ok_or_else(|| anyhow!("missing value for {{{name}}}"))?;
                    out.push_str(&enc(v));
                }
            }
            None => out.push_str(seg),
        }
    }
    Ok(out)
}

/// Percent-encode one URL path segment: everything outside the RFC 3986
/// unreserved set becomes `%XX`, so a value with `/`, `?` or `#` addresses the
/// segment the user meant, never a different route.
fn enc(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => out.push(b as char),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

// ---- the ONE authenticated call into cloud (the seam the tree dispatches on) --

/// Resolve WHERE (the active network's origin) and WHO (the active identity's
/// bearer) through the one identity seam, send the request, print the `data`, and
/// explain a 403 in identity terms. The org is NEVER a header: where a route names
/// it in the PATH, the caller has already addressed it (its own `owner`), and the
/// server re-checks it against the JWT it verifies. `path` is the FINAL
/// template-filled `/v1/…` — never a fetched spec, never a user-supplied host.
async fn call(
    cfg: &mut Config,
    method: Method,
    path: String,
    body: Option<Value>,
    query: Vec<String>,
    raw: bool,
) -> Result<()> {
    let origin = network::active(cfg).api;
    let origin = origin.trim_end_matches('/');
    let (id, tok) = store::active_token(cfg, paths::DEFAULT_BRAND)?
        .ok_or_else(|| anyhow!("not signed in — run `hanzo login`"))?;
    // The identity we would suggest switching to on a 403 (SuperAdmin gate) — the
    // very identity we authenticate as, so the hint can never name someone else.
    let held = store::list(cfg, paths::DEFAULT_BRAND);
    let hint = store::refusal_hint(&id, &held);

    let url = build_url(origin, &path, &query)?;
    let http_client = Client::new();
    let (status, resp) =
        http::send(&http_client, method, &url, &tok.access_token, body.as_ref()).await?;

    if status.is_success() {
        print_body(&resp, raw);
        return Ok(());
    }

    // Non-2xx: surface the SERVER's own body, and — only on a 403 the server
    // itself returned — the identity-switch hint. The refusal is always the
    // server's, never a client-side guess; we read our identity only to explain
    // it, after the fact.
    let shown = match &resp {
        Value::Null => String::new(),
        Value::String(s) => s.trim().to_string(),
        v => v.to_string(),
    };
    if status == StatusCode::FORBIDDEN {
        if let Some(hint) = hint {
            anyhow::bail!("{path} -> {status}: {shown}{hint}");
        }
    }
    anyhow::bail!("{path} -> {status}: {shown}");
}

/// Map an op's method string to a `reqwest::Method`.
fn parse_method(m: &str) -> Result<Method> {
    match m {
        "GET" => Ok(Method::GET),
        "POST" => Ok(Method::POST),
        "PUT" => Ok(Method::PUT),
        "PATCH" => Ok(Method::PATCH),
        "DELETE" => Ok(Method::DELETE),
        "HEAD" => Ok(Method::HEAD),
        other => anyhow::bail!("unsupported method {other:?}"),
    }
}

/// `--data` is JSON; `-` reads stdin so a secret in a body never lands in argv,
/// `ps` or shell history — the same rule as `kms set`. A body on a GET/HEAD is a
/// named error, not silently sent.
fn read_body(data: Option<String>, method: &Method) -> Result<Option<Value>> {
    let Some(d) = data else { return Ok(None) };
    if matches!(*method, Method::GET | Method::HEAD) {
        anyhow::bail!("--data is not sent on a {method} — this verb takes no body");
    }
    let raw = if d == "-" {
        let mut s = String::new();
        std::io::stdin().read_to_string(&mut s).context("reading --data from stdin")?;
        s
    } else {
        d
    };
    let value: Value = serde_json::from_str(raw.trim())
        .context("--data must be valid JSON (use `-` to read a JSON body from stdin)")?;
    Ok(Some(value))
}

/// Build the absolute URL, appending any `--query k=v` pairs. Split out so the
/// join is unit-testable without a network. Values are percent-encoded by
/// `reqwest::Url`, so a `k=a b&c` cannot forge extra parameters.
fn build_url(origin: &str, path: &str, query: &[String]) -> Result<String> {
    let mut url = reqwest::Url::parse(&format!("{origin}{path}"))
        .with_context(|| format!("building URL {origin}{path}"))?;
    {
        let mut pairs = url.query_pairs_mut();
        for q in query {
            let (k, v) = q
                .split_once('=')
                .ok_or_else(|| anyhow!("--query must be k=v (got {q:?})"))?;
            pairs.append_pair(k, v);
        }
    }
    Ok(url.to_string())
}

/// Print the response. The cloud `/v1` envelope is `{status,msg,data}`; by default
/// we surface `data` (what a caller pipes), and `--raw` prints the whole envelope.
fn print_body(resp: &Value, raw: bool) {
    let shown = if raw { resp } else { resp.get("data").unwrap_or(resp) };
    match shown {
        Value::Null => {}
        Value::String(s) => println!("{s}"),
        v => println!("{}", serde_json::to_string_pretty(v).unwrap_or_else(|_| v.to_string())),
    }
}

#[cfg(test)]
mod tests;
