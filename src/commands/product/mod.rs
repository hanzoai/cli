//! `hanzo <product> <resource…> <verb>` — cloud's Go surface as FIRST-CLASS
//! product commands, DERIVED from `spec/cloud.json`. This is the ONLY interface to
//! cloud: every capability is a real subcommand — there is no `hanzo api` verb and
//! no raw-path escape.
//!
//! Nothing here links cloud. `genspec` reads ONE document — hanzoai/cloud's own
//! emitted `openapi.yaml` at a pinned release — and nothing else, so existence,
//! prose and shape all come from the code that serves the route. (This paragraph
//! used to describe a second reading, a hand-authored master that said "what each
//! one takes"; it was deleted in 1.9.34 with the drift it caused.) `genproduct`
//! folds that one spec into a (product, resource nodes, verb, method, path
//! template, params, typed body fields) coordinate and commits it as pure DATA
//! (`generated.rs`). At runtime we build a clap tree from
//! that data and dispatch it through the one authenticated seam below: the ORIGIN
//! comes from `network`, the BEARER from `store`, and the data contributes only
//! the path template + argument SHAPE. The data contains no host, no URL and no
//! auth (a test fails the build otherwise), so a hostile snapshot can at worst
//! shape a call to YOUR OWN cloud with YOUR OWN token — never redirect it.
//!
//! A write whose requestBody the document TYPES gets typed `--flags` and the JSON
//! body is assembled from them — never `--data`. The reading goes all the way
//! down, because a shape the document states and the CLI does not is the same
//! defect as a shape neither states:
//!
//! - a scalar property is a flag at its own type (`--limit INT`, `--kind ENUM`);
//! - an ARRAY is a REPEATABLE flag over its ELEMENT (`--tag a --tag b`), not one
//!   `'["a","b"]'` blob the caller has to quote past a shell;
//! - a NESTED OBJECT is DOTTED flags (`--spec.replicas 3`), rebuilt into the
//!   object the schema declared — flat to type, nested on the wire.
//!
//! What stays `--data` is exactly what the document does not describe: a handler
//! declaring no requestBody (a typing gap in hanzoai/cloud, counted and pinned by
//! `genproduct`'s `NO_SCHEMA` ceiling), or a body that is freeform BY
//! CONSTRUCTION (`{}`, `additionalProperties`, `oneOf`, a bare array), where a
//! flag would be an invented shape. A product the document does not carry is
//! simply ABSENT — no passthrough, no `hanzo api` to paper over it; that gap
//! closes by serving the route. Nothing is invented — the fields are exactly the
//! schema's properties.

use anyhow::{anyhow, bail, Context, Result};
use clap::{Arg, ArgAction, ArgMatches, Command};
use reqwest::{Client, Method, StatusCode};
use serde_json::{json, Map, Value};
use std::io::Read;

use crate::commands::host::{self, Origin};
use crate::config::Config;
use crate::http;
#[cfg(unix)]
use crate::zap;
use crate::iam::{paths, store};

mod generated;
pub(crate) use generated::{OPS, PRODUCTS};

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
    /// The subset of `params` that address a MULTI-SEGMENT path: a server
    /// catch-all whose value is a `/`-joined address (e.g. a KMS secret
    /// `sub/path/name`). Their `/` is STRUCTURAL — each segment is percent-encoded
    /// but the slashes ride raw, so the route resolves; `.`/`..`/empty segments are
    /// refused. Empty for the single-segment default, where a `/` in a value is
    /// `%2F`-escaped so it can never re-address a different route.
    pub rest: &'static [&'static str],
    /// Typed request-body fields from the requestBody schema. Empty ⇒ no typed
    /// body (a write with no schema uses `--data`; a read has no body).
    pub fields: &'static [Field],
    /// One line of prose from the spec — a typed op's doc comment, lifted by
    /// zipdoc, carried by genspec. NEVER empty: `genproduct` refuses to emit an
    /// undescribed op, so a command with nothing to say never becomes one.
    pub sum: &'static str,
}

/// One typed request-body field, from a schema property.
pub struct Field {
    /// The JSON property name — the body key sent to cloud. DOTTED for a nested
    /// property (`spec.replicas`), which [`insert_path`] rebuilds into the object
    /// the schema declared. No property name in cloud's document contains a `.`,
    /// so the path is unambiguous.
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
    /// `true` ⇒ a URL query-string parameter (`?key=…`); `false` ⇒ a requestBody
    /// property. The flag looks identical; only the destination differs.
    pub query: bool,
    /// A SECRET body value (`format: password` in the spec). It gets NO flag and
    /// NO positional — the value is read from STDIN at dispatch through the one
    /// secret law (`iam::secret::read_secret`), so it can never land in argv,
    /// `ps` or shell history. The grammar refuses a value-bearing argument.
    pub secret: bool,
    /// The schema said ARRAY: the flag may be given more than once and the values
    /// collect into a JSON array (`--tag a --tag b` → `["a","b"]`). `ty` is then
    /// the ELEMENT's type, so an array of strings is a repeatable `--tag STRING`
    /// rather than one opaque `--tag '["a","b"]'` the shell has to quote.
    pub repeat: bool,
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

/// The generated products this parser mounts AT THEIR OWN NAME: every product no
/// local command has already claimed. A claimed one is not dropped — it is
/// absorbed into the command that owns the name (see [`augment`]) — so this is a
/// question about the man page's top-level listing, not about reachability.
///
/// The answer to "which names are local" is read off the parser itself rather
/// than restated in a list. [`catalog`] advertises exactly this set, so the man
/// page cannot name a top-level group the parser does not accept.
pub(crate) fn mounted(cmd: &Command) -> Vec<&'static str> {
    let taken: std::collections::HashSet<&str> =
        cmd.get_subcommands().map(clap::Command::get_name).collect();
    product_names().into_iter().filter(|p| !taken.contains(p)).collect()
}

/// THE COLLISION LAW, and it is one law applied at every level: a LOCAL command
/// owns its name, and the generated operations of that name mount INSIDE it.
///
/// A product no local command claims becomes a first-class top-level command. A
/// product whose name is taken is ABSORBED — `hanzo billing invoices` is the
/// document's operation, `hanzo billing balance` is the hand-written one, and the
/// same rule settles both: whoever was there first keeps the name.
///
/// It used to DROP the collision, and the cost was measured before this was
/// written: 7 `/v1/code`, 25 `/v1/billing` and 4 `/v1/engine` operations were
/// served, described, and reachable by nothing, excused by three curation entries
/// whose own reasons said so. A name clash is not a decision about the surface,
/// it is an arrangement of it — so it is settled here, by arranging, and
/// `src/curation.rs` no longer carries an entry that says "taken".
pub fn augment(mut cmd: Command) -> Command {
    for product in product_names() {
        cmd = match cmd.find_subcommand(product).is_some() {
            false => cmd.subcommand(build_product(product)),
            true => cmd.mut_subcommand(product, |local| absorb(local, product)),
        };
    }
    cmd
}

/// Merge a generated product into the local command that owns its bare name: each
/// of the product's own top-level nodes becomes a subcommand of it, EXCEPT one the
/// local command already declares — that one is the local command's, all the way
/// down, and [`resolve`] reads the same tree to route it back to the derive
/// dispatch. One predicate, both halves; they cannot disagree.
fn absorb(local: Command, product: &'static str) -> Command {
    let owned: std::collections::HashSet<String> =
        local.get_subcommands().map(|c| c.get_name().to_string()).collect();
    trie(product)
        .children
        .iter()
        .filter(|(name, _)| !owned.contains(**name))
        .fold(local, |c, (name, node)| c.subcommand(to_command(name, node)))
}

/// Distinct product names in `OPS`, stable order.
fn product_names() -> Vec<&'static str> {
    let mut seen = std::collections::HashSet::new();
    OPS.iter().map(|o| o.product).filter(|p| seen.insert(*p)).collect()
}

/// Every generated product with its one-line prose, for the root man page —
/// the SAME names the parser accepts and the SAME prose the group help shows.
/// `cmd` is the hand-written derive tree, before augmentation: a product whose
/// name a local command owns is not mounted, so it is not advertised either.
pub(crate) fn catalog(cmd: &Command) -> Vec<(&'static str, String)> {
    mounted(cmd)
        .into_iter()
        .map(|n| {
            let about = product_about(n)
                .map(str::to_string)
                .unwrap_or_else(|| format!("`{n}` cloud operations"));
            (n, about)
        })
        .collect()
}

/// A trie node: `children` are subcommands and `leaf` is a terminal op. A node can
/// be BOTH — a collection whose name also heads a nested group (`GET /v1/kv` is the
/// `list` leaf, and `/v1/kv/list/{key}` nests under the same `list` node). That is
/// a RUNNABLE GROUP: bare it runs the leaf, with a subcommand it descends.
#[derive(Default)]
struct Node {
    children: std::collections::BTreeMap<&'static str, Node>,
    leaf: Option<&'static Op>,
}

/// The product's own prose — its package doc synopsis, when it has one.
fn product_about(name: &str) -> Option<&'static str> {
    PRODUCTS.iter().find(|(n, _)| *n == name).map(|(_, d)| *d)
}

/// One product's whole operation set as a trie — the shape both a first-class
/// product command and an absorbed one are built from.
fn trie(product: &'static str) -> Node {
    let mut root = Node::default();
    for op in OPS.iter().filter(|o| o.product == product) {
        let mut n = &mut root;
        for &g in op.nodes {
            n = n.children.entry(g).or_default();
        }
        n.children.entry(op.verb).or_default().leaf = Some(op);
    }
    root
}

fn build_product(product: &'static str) -> Command {
    let root = trie(product);
    let c = to_command(product, &root);
    // The product describes ITSELF: its package doc synopsis is the about line
    // in `hanzo --help` and `hanzo <product> --help`. A runnable root keeps its
    // op's prose in long help; a product with no doc keeps the plain fallback —
    // the cure for which is a package doc in cloud, never a string here.
    match product_about(product) {
        Some(d) => c.about(d),
        None => c,
    }
}

fn to_command(name: &'static str, node: &Node) -> Command {
    match (node.leaf, node.children.is_empty()) {
        // Pure leaf — a runnable verb.
        (Some(op), true) => leaf(op),
        // Runnable group — its own op runs when NO subcommand is given, and its
        // args are mutually exclusive with the subcommands (so `kv list push k`
        // does not demand the collection GET's flags).
        (Some(op), false) => {
            let mut c = leaf(op).subcommand_required(false).args_conflicts_with_subcommands(true);
            for (child, sub) in &node.children {
                c = c.subcommand(to_command(child, sub));
            }
            c
        }
        // Pure group — a namespace that requires a subcommand.
        (None, _) => {
            let mut c = Command::new(name)
                .about(format!("`{name}` cloud operations"))
                .subcommand_required(true)
                .arg_required_else_help(true);
            for (child, sub) in &node.children {
                c = c.subcommand(to_command(child, sub));
            }
            c
        }
    }
}

/// A leaf command under its own verb name.
fn leaf(op: &'static Op) -> Command {
    leaf_named(op.verb, op)
}

/// A leaf command with an explicit NAME (so a friendly alias can reuse a
/// generated op verbatim): positionals for the path params, one typed flag per
/// body property + query parameter, and — only for a write with NO body schema —
/// a raw `--data` escape. A body property named `data`/`raw` cannot collide with
/// a control, because the clap id is namespaced and `--data` is added only when
/// no field already claims that long.
fn leaf_named(name: &'static str, op: &'static Op) -> Command {
    // The prose IS the help — the same sentence the OpenAPI description, the MCP
    // tool and the SDK docstring carry, because all four are projections of one Go
    // doc comment. There is no second rendering: `genproduct` refuses to emit an op
    // with no summary, so an undescribed op never reaches a `Command`. A projection
    // cannot repair its source, and every branch that handles "the source didn't
    // say" hides a defect at the one place it could be fixed. The route stays in
    // LONG help — reference detail, and what a user quotes when filing a bug —
    // never in prose's place.
    let mut c = Command::new(name)
        .about(op.sum)
        .long_about(format!("{}\n\n{} {}", op.sum, op.method, op.path));
    for &p in op.params {
        c = c.arg(Arg::new(p).required(true).help(format!("path parameter {{{p}}}")));
    }
    for f in op.fields {
        // A secret field is DELIBERATELY not an argument: it has no flag and no
        // positional, so a value-bearing argv is a parse error, not a matter of
        // discipline. The value is read from stdin at dispatch.
        if f.secret {
            continue;
        }
        c = c.arg(field_arg(f));
    }
    let has_body = op.fields.iter().any(|f| !f.query);
    if op.is_write() && !has_body && !op.fields.iter().any(|f| f.flag == "data") {
        c = c.arg(data_arg());
    }
    c
}

/// A typed body flag from a schema property. The clap parser matches the JSON
/// type, so the assembled body carries the right type — a number is a number, a
/// bool a bool — not a stringly-typed `--data` blob.
fn field_arg(f: &'static Field) -> Arg {
    // The clap id is namespaced; the value placeholder shows the TYPE, so the id
    // (`field.act`) never leaks into `--help`.
    let mut a = Arg::new(f.id).long(field_flag(f)).required(f.required).help(field_help(f));
    match f.ty {
        Ty::Int => a = a.value_parser(clap::value_parser!(i64)).value_name("INT"),
        Ty::Num => a = a.value_parser(clap::value_parser!(f64)).value_name("NUMBER"),
        // An array of booleans still takes a VALUE — `--f --f` could not say
        // `[true,false]` — so only a scalar bool is a bare presence flag.
        Ty::Bool if !f.repeat => a = a.action(ArgAction::SetTrue),
        Ty::Bool => a = a.value_parser(clap::value_parser!(bool)).value_name("BOOL"),
        Ty::Json => a = a.value_parser(parse_json).value_name("JSON"),
        Ty::Str => a = a.value_name("STRING"),
    }
    if !f.choices.is_empty() {
        a = a.value_parser(clap::builder::PossibleValuesParser::new(f.choices)).value_name("ENUM");
    }
    // An ARRAY property is a repeatable flag, one element per occurrence. The
    // element keeps its own parser, so `--port 1 --port two` is a named type
    // error at parse time rather than a malformed array the server rejects.
    if f.repeat {
        a = a.action(ArgAction::Append);
    }
    a
}

/// The GLOBAL controls (`--config`, `--verbose`) plus clap's own (`--help`,
/// `--version`) propagate into every generated subcommand, so a schema field
/// with one of those names cannot own the bare long flag. The collision rename
/// mirrors the id namespacing that already exists (`field.<key>` / `query.<key>`):
/// a body field becomes `--body-<key>`, a query param `--query-<key>`. Every
/// other field keeps the schema's own name — this touches collisions ONLY.
fn field_flag(f: &'static Field) -> &'static str {
    match (f.flag, f.query) {
        ("config", false) => "body-config",
        ("config", true) => "query-config",
        ("verbose", false) => "body-verbose",
        ("verbose", true) => "query-verbose",
        ("help", false) => "body-help",
        ("help", true) => "query-help",
        ("version", false) => "body-version",
        ("version", true) => "query-version",
        _ => f.flag,
    }
}

/// Type-derived help — DATA, never the spec's prose (which could carry a URL).
fn field_help(f: &Field) -> String {
    let t = if f.choices.is_empty() {
        match f.ty {
            Ty::Str => "string",
            Ty::Int => "integer",
            Ty::Num => "number",
            Ty::Bool if f.repeat => "boolean",
            Ty::Bool => "flag",
            Ty::Json => "JSON value",
        }
        .to_string()
    } else {
        format!("one of: {}", f.choices.join(" | "))
    };
    // The two facts a caller needs beyond the type, and they compose: an array of
    // enums is "one of: … (repeatable)".
    let t = if f.repeat { format!("{t} (repeatable)") } else { t };
    if f.required {
        format!("{t} (required)")
    } else {
        t
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

// ---- resolve a parse into a call, then dispatch through the ONE seam ---------

/// What a matched generated command resolves to. Pure over the clap matches —
/// no config, no keychain — so it is unit-testable without a network.
pub enum Resolved {
    Leaf { op: &'static Op, values: Vec<String>, body: LeafBody, query: Vec<String> },
}

/// A leaf's request body: assembled from typed flags, read raw from `--data`, or
/// absent (a read). The three tiers of the fallback ladder, resolved once.
pub enum LeafBody {
    Typed(Value),
    Data(Option<String>),
    None,
}

/// If `matches` selected a GENERATED operation, resolve it; otherwise `None` so
/// the derive tree handles it (a local command, or bare).
///
/// `hand` is the derive tree BEFORE augmentation — the same reading [`absorb`]
/// takes, asked the same way. Under an absorbed name both kinds of command live
/// side by side, and which one a token is cannot be answered from the data alone:
/// `hanzo billing balance` names a local command and `hanzo billing invoices`
/// names an operation, and only the hand-written tree knows which is which.
pub fn resolve(hand: &Command, matches: &ArgMatches) -> Option<Resolved> {
    let (top, sub) = matches.subcommand()?;
    if !is_product(top) {
        return None;
    }
    // Walk to the deepest matched subcommand. A RUNNABLE GROUP invoked bare stops
    // here (no sub-subcommand), so `find_op` resolves its own collection op.
    let mut chain: Vec<&str> = vec![top];
    let mut m = sub;
    while let Some((n, mm)) = m.subcommand() {
        chain.push(n);
        m = mm;
    }
    // Absorbed: the local command owns the name, so a node IT declares is its own
    // — including the bare command itself, which `absorb` never gives an op.
    if let Some(local) = hand.find_subcommand(top) {
        if local.find_subcommand(*chain.get(1)?).is_some() {
            return None;
        }
    }
    let op = find_op(&chain)?;
    Some(resolve_leaf(op, m))
}

/// Read a leaf op's arguments off its own matches: positionals, then the body
/// (typed properties, or `--data`, or none) and the typed query string.
fn resolve_leaf(op: &'static Op, m: &ArgMatches) -> Resolved {
    let values = op
        .params
        .iter()
        .map(|p| m.get_one::<String>(p).cloned().unwrap_or_default())
        .collect();
    let has_body = op.fields.iter().any(|f| !f.query);
    let body = if has_body {
        LeafBody::Typed(typed_body(op, m))
    } else if op.is_write() {
        // `--data` exists only when no field already claims the `data` long.
        let data = (!op.fields.iter().any(|f| f.flag == "data"))
            .then(|| m.get_one::<String>("data").cloned())
            .flatten();
        LeafBody::Data(data)
    } else {
        LeafBody::None
    };
    Resolved::Leaf { op, values, body, query: typed_query(op, m) }
}

/// Assemble the JSON body from the BODY flags actually provided — nothing else.
/// An unset optional field is OMITTED (so the server's own default stands), never
/// sent as null. Each value is encoded at its schema type, and a DOTTED key is
/// rebuilt into the nested object the schema declared.
fn typed_body(op: &Op, m: &ArgMatches) -> Value {
    let mut map = Map::new();
    // A secret field has no matches entry (no flag/positional) — it is injected
    // from stdin at dispatch, so it is skipped here.
    for f in op.fields.iter().filter(|f| !f.query && !f.secret) {
        if let Some(v) = field_value(f, m) {
            insert_path(&mut map, f.key, v);
        }
    }
    Value::Object(map)
}

/// ONE reading of a flag's value at its schema type, for the body and the query
/// alike. `None` when the flag was not given, so an unset optional stays out of
/// the request entirely. A REPEATABLE flag collects every occurrence into a JSON
/// array, in the order they were typed.
fn field_value(f: &Field, m: &ArgMatches) -> Option<Value> {
    if f.repeat {
        let vs: Vec<Value> = match f.ty {
            Ty::Str => m.get_many::<String>(f.id)?.map(|v| json!(v)).collect(),
            Ty::Int => m.get_many::<i64>(f.id)?.map(|v| json!(v)).collect(),
            Ty::Num => m.get_many::<f64>(f.id)?.map(|v| json!(v)).collect(),
            Ty::Bool => m.get_many::<bool>(f.id)?.map(|v| json!(v)).collect(),
            Ty::Json => m.get_many::<Value>(f.id)?.cloned().collect(),
        };
        return Some(Value::Array(vs));
    }
    match f.ty {
        Ty::Str => m.get_one::<String>(f.id).map(|v| json!(v)),
        Ty::Int => m.get_one::<i64>(f.id).map(|v| json!(v)),
        Ty::Num => m.get_one::<f64>(f.id).map(|v| json!(v)),
        // A scalar bool is a presence flag: absent means "unset", so it is
        // omitted rather than sent as `false` over the server's own default.
        Ty::Bool => m.get_flag(f.id).then_some(json!(true)),
        Ty::Json => m.get_one::<Value>(f.id).cloned(),
    }
}

/// Place a value at a DOTTED body key, building the objects the schema declared:
/// `--spec.replicas 3` becomes `{"spec":{"replicas":3}}`. The paths come from one
/// schema walk, so two fields can never disagree about whether a step is an
/// object; a non-object already sitting at a step is REPLACED rather than
/// panicked over, because a half-built body is worse than a whole one.
fn insert_path(map: &mut Map<String, Value>, key: &str, value: Value) {
    let mut parts = key.split('.').peekable();
    let mut cur = map;
    while let Some(p) = parts.next() {
        if parts.peek().is_none() {
            cur.insert(p.to_string(), value);
            return;
        }
        let slot = cur.entry(p.to_string()).or_insert_with(|| Value::Object(Map::new()));
        if !slot.is_object() {
            *slot = Value::Object(Map::new());
        }
        cur = slot.as_object_mut().expect("just made it an object");
    }
}

/// Assemble `key=value` query pairs from the QUERY flags actually provided; each
/// value is stringified at its type and percent-encoded by `target`. A repeatable
/// parameter emits ONE PAIR PER VALUE (`?tag=a&tag=b`), which is how a query
/// array is spelled — never one comma-joined string.
fn typed_query(op: &Op, m: &ArgMatches) -> Vec<String> {
    let mut out = Vec::new();
    for f in op.fields.iter().filter(|f| f.query) {
        match field_value(f, m) {
            Some(Value::Array(vs)) => out.extend(vs.iter().map(|v| format!("{}={}", f.key, scalar(v)))),
            Some(v) => out.push(format!("{}={}", f.key, scalar(&v))),
            None => {}
        }
    }
    out
}

/// A query value's wire spelling: a string rides BARE (`?q=hi`, not `?q="hi"`),
/// everything else is its JSON text.
fn scalar(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        v => v.to_string(),
    }
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
    let Resolved::Leaf { op, values, body, query } = resolved;
    let owner = store::active(cfg, paths::DEFAULT_BRAND).map(|i| i.owner);
    let path = fill_path(op.path, op.rest, owner.as_deref(), &values)?;
    let method = parse_method(op.method)?;
    let body = match body {
        // A typed body may carry a stdin-secret field, read here (the IO layer)
        // so `resolve` stays pure and the value never passes through argv.
        LeafBody::Typed(v) => Some(inject_secret(op, v)?),
        LeafBody::Data(d) => read_body(d, &method)?,
        LeafBody::None => None,
    };
    // The `/v1` envelope's `data` is always what we surface (there is no `--raw`).
    call(cfg, method, path, body, query, false).await
}

/// Fill a stdin-secret body field (`format: password`, e.g. `kms secrets
/// create`'s `value`) from STDIN through the ONE secret law
/// (`iam::secret::read_secret`). Because the field has no flag and no positional,
/// this is the ONLY way a secret enters the body — it can never come from argv.
/// A single op reads stdin exactly once; more than one secret field is an
/// authoring error we refuse rather than read stdin twice.
fn inject_secret(op: &Op, mut body: Value) -> Result<Value> {
    let mut secrets = op.fields.iter().filter(|f| f.secret);
    let Some(f) = secrets.next() else { return Ok(body) };
    if secrets.next().is_some() {
        anyhow::bail!("{} declares more than one stdin-secret field — an op reads stdin once", op.path);
    }
    let value = crate::iam::secret::read_secret(std::io::stdin().lock())?;
    let obj = body.as_object_mut().ok_or_else(|| anyhow!("a typed body must be a JSON object"))?;
    insert_path(obj, f.key, Value::String(value));
    Ok(body)
}

/// Fill a `/v1` template: a param preceded by `orgs` is the tenant scope, bound
/// to `owner` (refused when signed out); every other param takes the next
/// positional. Values are percent-encoded so a value cannot re-address a
/// different route — the same rule `kms` uses. This mirrors the generator's
/// scope predicate; their agreement is pinned by `every_op_fills_to_a_path`.
fn fill_path(
    template: &str,
    rest: &[&str],
    owner: Option<&str>,
    values: &[String],
) -> Result<String> {
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
                    let o = owner.ok_or_else(|| anyhow!("not signed in — run `hanzo auth login`"))?;
                    out.push_str(&enc(o));
                } else {
                    let v = it.next().ok_or_else(|| anyhow!("missing value for {{{name}}}"))?;
                    // A MULTI-SEGMENT (catch-all) param keeps its slashes raw so the
                    // server route resolves; a single-segment one `%2F`-escapes them.
                    if rest.contains(&name) {
                        out.push_str(&enc_path(v)?);
                    } else {
                        out.push_str(&enc(v));
                    }
                }
            }
            None => out.push_str(seg),
        }
    }
    Ok(out)
}

/// Encode a MULTI-SEGMENT path value whose `/` are STRUCTURAL — a server catch-all
/// (e.g. a KMS secret `sub/path/name`). Each segment is percent-encoded by [`enc`]
/// and the slashes stay raw, so the value addresses the secret the user named
/// rather than a `%2F`-mangled one the backend 404s. `.`/`..`/empty segments are
/// refused BEFORE a URL exists, so a value can never `../../` its way onto another
/// org's route — the same protection the flat case gets for free from `%2F`.
fn enc_path(s: &str) -> Result<String> {
    let mut out = String::with_capacity(s.len());
    for (i, seg) in s.split('/').enumerate() {
        if seg.is_empty() || seg == "." || seg == ".." {
            bail!("invalid path segment {seg:?} in {s:?}");
        }
        if i > 0 {
            out.push('/');
        }
        out.push_str(&enc(seg));
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
    let seam = Seam::open(cfg).await?;
    let (status, resp) = seam.send(method, &path, &query, body).await?;

    if status.is_success() {
        // A 2xx is NOT proof of success. Some planes (IAM) answer an error
        // with HTTP 200 and an `{"status":"error","msg":…}` envelope; rendering
        // only `data` then prints nothing and exits 0, silently swallowing the
        // refusal. Surface the server's own message to stderr with a non-zero exit.
        if let Some(msg) = envelope_error(&resp) {
            bail!("{path}: {msg}");
        }
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
        if let Some(hint) = seam.hint {
            anyhow::bail!("{path} -> {status}: {shown}{hint}");
        }
    }
    anyhow::bail!("{path} -> {status}: {shown}");
}

/// WHERE and WHO, resolved ONCE — the one authenticated route into cloud, held
/// as a value so a command that reads SEVERAL surfaces (`hanzo status`) resolves
/// the origin and the credential once and then fans out concurrently, instead of
/// re-deriving them per call and drifting.
pub(crate) struct Seam {
    origin: Origin,
    /// The bearer AND NOTHING ELSE goes on the wire — cloud derives the tenant
    /// from the `owner` claim it verifies, so nothing here can name a tenant.
    token: String,
    /// The identity-switch hint to offer if — and only if — the SERVER returns a
    /// 403. Resolved from the very identity we authenticate as, so it can never
    /// name someone else.
    pub(crate) hint: Option<String>,
    http: Client,
}

impl Seam {
    pub(crate) async fn open(cfg: &mut Config) -> Result<Self> {
        // WHERE, and over WHICH WIRE. A LOCAL origin also gets a host started (or
        // reused) behind it, so the same command tree runs against a checkout with
        // no server up — over ZAP, the native transport, because the CLI is first
        // party.
        let origin = host::origin(cfg).await?;

        // WHO. A local host may have no IAM to sign in to, so a missing credential
        // is NOT refused here — the call goes out unauthenticated and the server
        // decides. That is the same rule this module already follows for a 403:
        // the refusal is always the server's, never a client-side guess.
        let identity = store::active_token(cfg, paths::DEFAULT_BRAND).await?;
        if identity.is_none() && matches!(origin, Origin::Http(_)) {
            bail!("not signed in — run `hanzo auth login`");
        }
        let held = store::list(cfg, paths::DEFAULT_BRAND);
        let hint = identity.as_ref().and_then(|(id, _)| store::refusal_hint(id, &held));
        let token = identity.map(|(_, t)| t.access_token).unwrap_or_default();
        Ok(Self { origin, token, hint, http: Client::new() })
    }

    /// Send one request over whichever wire the origin named. The STATUS is
    /// handed back, never flattened: a refusal is a value the caller must read,
    /// and only a transport fault is an `Err`.
    pub(crate) async fn send(
        &self,
        method: Method,
        path: &str,
        query: &[String],
        body: Option<Value>,
    ) -> Result<(StatusCode, Value)> {
        let target = target(path, query)?;
        match &self.origin {
            #[cfg(unix)]
            Origin::Local(sock) => {
                zap::send(sock, &method, &target, &self.token, body.as_ref()).await
            }
            #[cfg(not(unix))]
            Origin::Local(base) => {
                let url = format!("{base}{target}");
                http::send(&self.http, method, &url, &self.token, body.as_ref()).await
            }
            Origin::Http(base) => {
                let url = format!("{base}{target}");
                http::send(&self.http, method, &url, &self.token, body.as_ref()).await
            }
        }
    }
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

/// The origin-form request target: `/path` plus any `--query k=v`, percent-
/// encoded. ZAP puts this in the target slot and HTTP appends it to its origin,
/// so a query is encoded exactly ONCE, here, for both wires. Values go through
/// `reqwest::Url`, so a `k=a b&c` cannot forge extra parameters.
fn target(path: &str, query: &[String]) -> Result<String> {
    // A placeholder authority, only so the URL crate will parse an origin-form
    // path and lend its query encoder. Nothing reads it back — the host on the
    // wire comes from the transport, never from here.
    let mut url = reqwest::Url::parse(&format!("http://local{path}"))
        .with_context(|| format!("building request target {path}"))?;
    if !query.is_empty() {
        let mut pairs = url.query_pairs_mut();
        for q in query {
            let (k, v) = q
                .split_once('=')
                .ok_or_else(|| anyhow!("--query must be k=v (got {q:?})"))?;
            pairs.append_pair(k, v);
        }
    }
    Ok(match url.query() {
        Some(q) => format!("{}?{}", url.path(), q),
        None => url.path().to_string(),
    })
}

/// Read the cloud `/v1` envelope (`{status,msg,data}`) for an error the HTTP status
/// did not carry. Some planes (IAM) answer a refusal with HTTP 200 and
/// `{"status":"error","msg":"…"}`, so a 2xx is not proof of success. Returns the
/// server's own message when the body is an ERROR — an explicit `status:"error"`
/// (any case), or a bare `{"error":"…"}` with no `data` — and `None` otherwise, so
/// a genuine success (a success envelope, or a raw non-enveloped body) renders
/// unchanged. It never invents a message and never fires on a success that merely
/// has a `msg` with null `data`: an explicit non-error `status` wins.
pub(crate) fn envelope_error(body: &Value) -> Option<String> {
    let obj = body.as_object()?;
    let message = || {
        obj.get("msg")
            .or_else(|| obj.get("error"))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or("request failed")
            .to_string()
    };
    match obj.get("status").and_then(Value::as_str) {
        // An explicit error status is a failure regardless of the HTTP code.
        Some(s) if s.eq_ignore_ascii_case("error") || s.eq_ignore_ascii_case("failed") => {
            Some(message())
        }
        // Any other explicit status (`ok`/`success`/…) is the server saying success.
        Some(_) => None,
        // No envelope status: a bare `{"error":"…"}` carrying no payload is still an
        // error; a raw body (an array, or an object that IS the data) is not.
        None => {
            let has_data = obj.get("data").is_some_and(|d| !d.is_null());
            let bare_error =
                obj.get("error").and_then(Value::as_str).is_some_and(|s| !s.trim().is_empty());
            (bare_error && !has_data).then(message)
        }
    }
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
