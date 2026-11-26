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
use hanzo_client::{Http, Method, Reply, Request, Transport};
use serde_json::{json, Map, Value};
use std::io::{IsTerminal, Read};

use crate::commands::host::{self, Origin};
use crate::config::Config;
use crate::iam::{paths, store};
#[cfg(unix)]
use crate::zap;

mod generated;
pub(crate) use generated::{OPS, PRODUCTS};

// The table's TYPES are their own module because `src/bin/driftgate.rs` reads the
// same generated table — see `op.rs`.
mod op;
pub use op::{Field, Op, Ty};

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
    product_names()
        .into_iter()
        .filter(|p| !taken.contains(p))
        .collect()
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
    // The parser MINTS a `help` subcommand of its own, during `build`, after this
    // has run — so a generated product of that name arrives second and clap
    // refuses two commands with one name. The collision law above settles it the
    // same way it settles every other clash, and here it is the parser's own
    // affordance that stands down: `/v1/help` is the customer help center
    // (articles, categories, support tickets), four served operations that
    // reached nobody while clap held the word.
    //
    // clap carries this setting to every subcommand, so `hanzo iam help` goes with
    // it and not only the root one — which is the right size of change rather than
    // collateral. `--help` sits on every command and is the spelling the man page
    // teaches; bare `hanzo` prints that page. `help` as a positional was a second
    // spelling of one act, and one act has one spelling.
    if product_names().contains(&"help") {
        cmd = cmd.disable_help_subcommand(true);
    }
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
    absorb_node(local, &trie(product))
}

/// Recursively merge a generated trie node into a hand-written local command.
/// Local terminal subcommands keep their names (including aliases); local groups
/// descend so generated siblings under the same group remain reachable.
fn absorb_node(mut local: Command, node: &Node) -> Command {
    let owned: std::collections::HashSet<String> = local
        .get_subcommands()
        .flat_map(|c| std::iter::once(c.get_name()).chain(c.get_all_aliases()))
        .map(str::to_string)
        .collect();
    let runnable = local.get_arguments().next().is_some();
    for (&name, child) in &node.children {
        if !owned.contains(name) {
            local = local.subcommand(to_command(name, child));
        } else if !child.children.is_empty() {
            let group_name = local
                .get_subcommands()
                .find(|c| {
                    (c.get_name() == name || c.get_all_aliases().any(|a| a == name))
                        && c.get_subcommands().next().is_some()
                })
                .map(|c| c.get_name().to_string());
            if let Some(g) = group_name {
                local = local.mut_subcommand(g, |sub| absorb_node(sub, child));
            }
        }
    }
    match runnable {
        true => local
            .args_conflicts_with_subcommands(true)
            .subcommand_negates_reqs(true),
        false => local,
    }
}

/// Distinct product names in `OPS`, stable order.
fn product_names() -> Vec<&'static str> {
    let mut seen = std::collections::HashSet::new();
    OPS.iter()
        .map(|o| o.product)
        .filter(|p| seen.insert(*p))
        .collect()
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
            let mut c = leaf(op)
                .subcommand_required(false)
                .args_conflicts_with_subcommands(true);
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
        c = c.arg(
            Arg::new(p)
                .required(true)
                .help(format!("path parameter {{{p}}}")),
        );
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
    if op.is_write() && !op.fields.iter().any(|f| f.flag == "data") {
        // An op with no typed body takes `--data`; one with a typed body takes flags, and `--data` is
        // parsed (hidden) only so that using it is refused by name (see [`flags_not_data`]).
        c = c.arg(if has_body {
            data_arg().hide(true)
        } else {
            data_arg()
        });
    }
    c.arg(output_arg())
}

/// How an answer is printed (`-o`): a table, JSON or YAML. One formatter serves every generated command.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Output {
    Table,
    Json,
    Yaml,
}

fn output_arg() -> Arg {
    Arg::new("output")
        .short('o')
        .long("output")
        .value_name("FORMAT")
        .value_parser(["table", "json", "yaml"])
        .help("how to print the answer: table (the default for a list on a terminal), json or yaml")
}

/// The `-o` a command line gave, if it gave one.
fn output_of(m: &ArgMatches) -> Option<Output> {
    m.try_get_one::<String>("output")
        .ok()
        .flatten()
        .map(|name| match name.as_str() {
            "table" => Output::Table,
            "yaml" => Output::Yaml,
            _ => Output::Json,
        })
}

/// The refusal for `--data` on an operation that types its body: the flags it takes, by name.
fn flags_not_data(op: &'static Op) -> String {
    let mut name = vec![op.product];
    name.extend(op.nodes);
    name.push(op.verb);
    let flags: Vec<String> = op
        .fields
        .iter()
        .filter(|f| !f.query && !f.secret)
        .map(|f| format!("--{}", field_flag(f)))
        .collect();
    format!(
        "`hanzo {}` takes its body as flags, not --data: {}",
        name.join(" "),
        flags.join(" ")
    )
}

/// A typed body flag from a schema property. The clap parser matches the JSON
/// type, so the assembled body carries the right type — a number is a number, a
/// bool a bool — not a stringly-typed `--data` blob.
fn field_arg(f: &'static Field) -> Arg {
    // The clap id is namespaced; the value placeholder shows the TYPE, so the id
    // (`field.act`) never leaks into `--help`.
    let mut a = Arg::new(f.id)
        .long(field_flag(f))
        .required(f.required)
        .help(field_help(f));
    match f.ty {
        Ty::Int => a = a.value_parser(clap::value_parser!(i64)).value_name("INT"),
        Ty::Num => {
            a = a
                .value_parser(clap::value_parser!(f64))
                .value_name("NUMBER")
        }
        // A scalar boolean accepts `--enabled` (true) or `--enabled false`.
        // Presence-only flags cannot express false on update endpoints.
        Ty::Bool if !f.repeat => {
            a = a
                .action(ArgAction::Set)
                .num_args(0..=1)
                .default_missing_value("true")
                .value_parser(clap::value_parser!(bool))
                .value_name("BOOL")
        }
        Ty::Bool => a = a.value_parser(clap::value_parser!(bool)).value_name("BOOL"),
        Ty::Json => a = a.value_parser(parse_json).value_name("JSON"),
        Ty::Union => a = a.value_parser(parse_union).value_name("TEXT|JSON"),
        Ty::Str => a = a.value_name("STRING"),
    }
    if !f.choices.is_empty() {
        a = a
            .value_parser(clap::builder::PossibleValuesParser::new(f.choices))
            .value_name("ENUM");
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
        ("output", false) => "body-output",
        ("output", true) => "query-output",
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
            Ty::Union => "text, or a JSON object or array",
        }
        .to_string()
    } else {
        format!("one of: {}", f.choices.join(" | "))
    };
    // The two facts a caller needs beyond the type, and they compose: an array of
    // enums is "one of: … (repeatable)".
    let t = if f.repeat {
        format!("{t} (repeatable)")
    } else {
        t
    };
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

/// Parse a `Union`-typed flag: a JSON object or array is sent as that value, and
/// anything else as the text typed — so `--state 'charged twice'` and `--state
/// '{"message":"charged twice"}'` both say what they look like.
fn parse_union(s: &str) -> std::result::Result<Value, String> {
    match serde_json::from_str::<Value>(s) {
        Ok(v @ (Value::Object(_) | Value::Array(_))) => Ok(v),
        _ => Ok(Value::String(s.to_string())),
    }
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
    Leaf {
        op: &'static Op,
        values: Vec<String>,
        body: LeafBody,
        query: Vec<String>,
        output: Option<Output>,
    },
}

/// A leaf's request body: assembled from typed flags, read raw from `--data`, or
/// absent (a read). The three tiers of the fallback ladder, resolved once.
pub enum LeafBody {
    Typed(Value),
    Data(Option<String>),
    /// `--data` given to an operation that types its body: refused at dispatch, naming the flags.
    Misused,
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
    // Absorbed: the local command owns every path its hand-written tree declares
    // — including the bare command itself, which `absorb` never gives an op.
    // When `absorb_node` descended into a local group and mounted generated
    // siblings beneath it, stepping off the hand-written tree falls through to
    // `find_op`.
    if let Some(mut cursor) = hand.find_subcommand(top) {
        if chain.len() == 1 {
            return None;
        }
        let mut owned_by_local = true;
        for seg in &chain[1..] {
            match cursor.find_subcommand(seg) {
                Some(next) => cursor = next,
                None => {
                    owned_by_local = false;
                    break;
                }
            }
        }
        if owned_by_local {
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
        match m.try_get_one::<String>("data").ok().flatten() {
            Some(_) => LeafBody::Misused,
            None => LeafBody::Typed(typed_body(op, m)),
        }
    } else if op.is_write() {
        // `--data` exists only when no field already claims the `data` long.
        let data = (!op.fields.iter().any(|f| f.flag == "data"))
            .then(|| m.get_one::<String>("data").cloned())
            .flatten();
        LeafBody::Data(data)
    } else {
        LeafBody::None
    };
    Resolved::Leaf {
        op,
        values,
        body,
        query: typed_query(op, m),
        output: output_of(m),
    }
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
            Ty::Json | Ty::Union => m.get_many::<Value>(f.id)?.cloned().collect(),
        };
        return Some(Value::Array(vs));
    }
    match f.ty {
        Ty::Str => m.get_one::<String>(f.id).map(|v| json!(v)),
        Ty::Int => m.get_one::<i64>(f.id).map(|v| json!(v)),
        Ty::Num => m.get_one::<f64>(f.id).map(|v| json!(v)),
        // Optional booleans remain absent unless given: explicit false must
        // reach the API instead of being mistaken for an unset flag.
        Ty::Bool => m.get_one::<bool>(f.id).copied().map(Value::Bool),
        Ty::Json | Ty::Union => m.get_one::<Value>(f.id).cloned(),
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
        let slot = cur
            .entry(p.to_string())
            .or_insert_with(|| Value::Object(Map::new()));
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
            Some(Value::Array(vs)) => {
                out.extend(vs.iter().map(|v| format!("{}={}", f.key, scalar(v))))
            }
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
    OPS.iter()
        .find(|o| o.product == product && o.verb == verb && o.nodes == nodes)
}

/// Bind the resolved call to a concrete request and send it through the one seam.
/// The org scope is filled from the active identity's `owner` (via the seam),
/// never asked; all other params are the user's positionals. The bearer + origin
/// are `call`'s to resolve.
/// The membership read answers ONE question: which orgs a user is in, or who is
/// in an org. The authored spec leaves both query fields optional, so a bare call
/// reached the server and got a 400. Refuse it here with the one actionable fix.
fn memberships_query_ok(query: &[String]) -> bool {
    let has = |key: &str| query.iter().any(|q| q.starts_with(&format!("{key}=")));
    has("user") != has("org")
}

pub async fn dispatch(cfg: &mut Config, resolved: Resolved) -> Result<()> {
    let Resolved::Leaf {
        op,
        values,
        body,
        query,
        output,
    } = resolved;
    if op.product == "iam" && op.nodes == ["memberships"] && op.verb == "get"
        && !memberships_query_ok(&query)
    {
        anyhow::bail!("iam memberships get needs exactly one of --user or --org");
    }
    let owner = store::acting(cfg, paths::DEFAULT_BRAND).map(|i| i.owner);
    let path = fill_path(op.path, op.rest, owner.as_deref(), &values)?;
    let method = parse_method(op.method)?;
    let body = match body {
        // A typed body may carry a stdin-secret field, read here (the IO layer)
        // so `resolve` stays pure and the value never passes through argv.
        LeafBody::Typed(v) => Some(named(op, inject_secret(op, v)?)),
        LeafBody::Data(d) => read_body(d, &method)?,
        LeafBody::Misused => bail!("{}", flags_not_data(op)),
        LeafBody::None => None,
    };
    call(cfg, method, path, body, query, output).await
}

/// A fresh id for one invocation: the platform answers a request that carries it once, so a request made
/// again after a failure cannot do its work twice.
pub(crate) fn request_id() -> String {
    format!("cli-{:032x}", rand::random::<u128>())
}

/// The body of an operation that declares a `requestId`, with one given when the caller gave none.
fn named(op: &Op, mut body: Value) -> Value {
    if op.fields.iter().any(|f| f.key == "requestId" && !f.query) {
        if let Some(map) = body.as_object_mut() {
            map.entry("requestId")
                .or_insert_with(|| json!(request_id()));
        }
    }
    body
}

/// Fill a secret body field (`kms secrets create`'s `value`, `integrations
/// connect`'s `token`) through the ONE secret law (`iam::secret`). Because the
/// field has no flag and no positional, this is the ONLY way a secret enters the
/// body — it can never come from argv, and a value-bearing argv is a parse error
/// rather than a matter of discipline. A single op reads stdin exactly once; more
/// than one secret field is an authoring error we refuse rather than read twice.
///
/// A PIPE is read; a TERMINAL is PROMPTED, hidden. It used to read stdin either
/// way, so a person running this at a terminal typed their credential in the
/// clear — the reason a hand-written `hanzo connector add` survived beside the
/// generated command for the same four routes.
fn inject_secret(op: &Op, mut body: Value) -> Result<Value> {
    use crate::iam::secret;
    let mut secrets = op.fields.iter().filter(|f| f.secret);
    let Some(f) = secrets.next() else {
        return Ok(body);
    };
    if secrets.next().is_some() {
        anyhow::bail!(
            "{} declares more than one secret field — an op reads stdin once",
            op.path
        );
    }
    // A secret field has no flag, so `None` is the only question that can be
    // asked: nothing could have carried a literal here for the law to refuse.
    let value = match secret::secret_source(None, secret::stdin_is_tty()) {
        secret::SecretSource::Prompt => secret::prompt(&format!(
            "{} for `hanzo {} {}`",
            f.flag, op.product, op.verb
        ))?,
        _ => secret::read_secret(std::io::stdin().lock())?,
    };
    let obj = body
        .as_object_mut()
        .ok_or_else(|| anyhow!("a typed body must be a JSON object"))?;
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
                    let o =
                        owner.ok_or_else(|| anyhow!("not signed in — run `hanzo auth login`"))?;
                    out.push_str(&enc(o));
                } else {
                    let v = it
                        .next()
                        .ok_or_else(|| anyhow!("missing value for {{{name}}}"))?;
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
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(b as char)
            }
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
pub(crate) async fn call(
    cfg: &mut Config,
    method: Method,
    path: String,
    body: Option<Value>,
    query: Vec<String>,
    output: Option<Output>,
) -> Result<()> {
    let seam = Seam::open(cfg, paths::DEFAULT_BRAND).await?;
    let Reply { status, body: resp } = seam.send(method, &path, &query, body).await?;

    if (200..300).contains(&status) {
        // A 2xx is NOT proof of success. Some planes (IAM) answer an error
        // with HTTP 200 and an `{"status":"error","msg":…}` envelope; rendering
        // only `data` then prints nothing and exits 0, silently swallowing the
        // refusal. Surface the server's own message to stderr with a non-zero exit.
        if let Some(msg) = envelope_error(&resp) {
            bail!("{path}: {msg}");
        }
        print_body(&resp, output);
        return Ok(());
    }

    // Non-2xx: surface the SERVER's own body. The refusal is the server's,
    // never a client-side guess; a 403 already names who was refused and why.
    let shown = match &resp {
        Value::Null => String::new(),
        Value::String(s) => s.trim().to_string(),
        v => v.to_string(),
    };
    if status == 401 && seam.token.is_empty() {
        anyhow::bail!("{path} -> {status}: {shown} — not signed in; run `hanzo auth login`");
    }
    anyhow::bail!("{path} -> {status}: {shown}");
}

/// WHERE and WHO, resolved ONCE — the one authenticated route into cloud, held
/// as a value so a command that reads SEVERAL surfaces (`hanzo status`) resolves
/// the origin and the credential once and then fans out concurrently, instead of
/// re-deriving them per call and drifting.
pub(crate) struct Seam {
    origin: Origin,
    /// The bearer. Cloud derives the tenant from the `owner` claim it verifies,
    /// so a request that names no org acts in the caller's own — which is every
    /// request unless [`Seam::org`] is set.
    pub(crate) token: String,
    /// The org to act in, when the caller named one (`--org`).
    ///
    /// It rides as `X-Org-Id` and is a SELECTION, not an assertion: the gateway
    /// checks it against the IAM-signed `orgs` membership claim and discards a
    /// value outside that set, so naming an org one does not belong to is a
    /// no-op rather than an escalation. Without it an operator who belongs to
    /// several orgs could only ever see one of them.
    pub(crate) org: Option<String>,
    wire: Http,
}

impl Seam {
    /// The seam as `brand`'s active identity: the CLI's own commands speak as
    /// the default brand, and a session's meter as the brand the session runs as.
    pub(crate) async fn open(cfg: &mut Config, brand: &str) -> Result<Self> {
        // WHERE, and over WHICH WIRE. A LOCAL origin also gets a host started (or
        // reused) behind it, so the same command tree runs against a checkout with
        // no server up — over ZAP, the native transport, because the CLI is first
        // party.
        let origin = host::origin(cfg).await?;

        // A MACHINE CALLER'S KEY, read at the point of use. An API key names no
        // principal, so it is never filed as an identity (Identity::from_access_token
        // refuses one); a caller that holds only a key — a bot in a sandbox, a CI
        // step — sets HANZO_API_KEY and every cloud command speaks as that key. It
        // wins over a stored login for the reason `hanzo dev`'s provider tries it
        // first: the variable is chosen for this process, the login is the machine's
        // default. Without it a box answered `not signed in`, and a box signed in as a
        // person read none of the sessions its key had opened (404).
        if let Some(token) = machine_key(std::env::var_os("HANZO_API_KEY")) {
            return Ok(Self {
                origin,
                token,
                org: cfg.org.clone(),
                wire: Http::default(),
            });
        }

        // WHO. A local host may have no IAM to sign in to, so a missing credential
        // is NOT refused here — the call goes out unauthenticated and the server
        // decides. That is the same rule this module already follows for a 403:
        // the refusal is always the server's, never a client-side guess.
        let identity = store::active_token(cfg, brand).await?;
        // Public routes (`models list`, health) answer without a credential, so a
        // signed-out call goes out anonymously; a protected route answers 401 and
        // the error says how to sign in.
        let token = identity.map(|(_, t)| t.access_token).unwrap_or_default();
        Ok(Self {
            origin,
            token,
            org: cfg.org.clone(),
            wire: Http::default(),
        })
    }

    pub(crate) fn is_http_origin(&self) -> bool {
        matches!(self.origin, Origin::Http(_))
    }

    /// Send one request to an explicit HTTP origin (e.g. `https://post.hanzo.ai`
    /// fallback while `/v1/channel` & `/v1/post` finish rolling onto `api.hanzo.ai`)
    /// carrying the exact same IAM bearer and `X-Org-Id` selection.
    pub(crate) async fn send_at(
        &self,
        base_url: &str,
        method: Method,
        path: &str,
        query: &[String],
        body: Option<Value>,
    ) -> Result<Reply> {
        let target = target(path, query)?;
        let base = base_url.trim_end_matches('/');
        self.over_http(&format!("{base}{target}"), method, body)
            .await
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
    ) -> Result<Reply> {
        let target = target(path, query)?;
        match &self.origin {
            #[cfg(unix)]
            Origin::Local(sock) => {
                zap::send(sock, &method, &target, &self.token, body.as_ref()).await
            }
            #[cfg(not(unix))]
            Origin::Local(base) => {
                self.over_http(&format!("{base}{target}"), method, body)
                    .await
            }
            Origin::Http(base) => {
                self.over_http(&format!("{base}{target}"), method, body)
                    .await
            }
        }
    }

    async fn over_http(&self, url: &str, method: Method, body: Option<Value>) -> Result<Reply> {
        let mut request = Request::new(method, url).token(&self.token);
        if let Some(org) = &self.org {
            request = request.org(org);
        }
        if let Some(body) = body {
            request = request.body(body);
        }
        Ok(self.wire.send(request).await?)
    }
}

/// The bearer a machine caller set in `HANZO_API_KEY`, or `None` when it is unset,
/// blank or not text. Surrounding whitespace is a paste, not part of the key.
fn machine_key(var: Option<std::ffi::OsString>) -> Option<String> {
    let key = var?.into_string().ok()?;
    let key = key.trim();
    (!key.is_empty()).then(|| key.to_string())
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
        std::io::stdin()
            .read_to_string(&mut s)
            .context("reading --data from stdin")?;
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

/// The keys a `/v1` envelope carries: `{status,msg,data}`, IAM's `sub`, `name` and
/// `data2`, and the bare `error`/`code` form.
const ENVELOPE: &[&str] = &[
    "status", "msg", "data", "data2", "sub", "name", "error", "code",
];

/// Read the cloud `/v1` envelope (`{status,msg,data}`) for an error the HTTP status
/// did not carry. Some planes (IAM) answer a refusal with HTTP 200 and
/// `{"status":"error","msg":"…"}`, so a 2xx is not proof of success. Returns the
/// server's own message when the body is an ERROR — an explicit `status:"error"`
/// (any case), or a bare `{"error":"…"}` with no `data` — and `None` otherwise, so
/// a genuine success (a success envelope, or a raw non-enveloped body) renders
/// unchanged. It never invents a message and never fires on a success that merely
/// has a `msg` with null `data`: an explicit non-error `status` wins.
///
/// Only a body made of envelope keys is an envelope. A body with any other key is a
/// resource, and its `status`/`error` are its own state: a failed coding run is
/// `{"id":…,"status":"error","error":…}`, a success to render, not a refusal.
pub(crate) fn envelope_error(body: &Value) -> Option<String> {
    let obj = body.as_object()?;
    if !obj.keys().all(|k| ENVELOPE.contains(&k.as_str())) {
        return None;
    }
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
            let bare_error = obj
                .get("error")
                .and_then(Value::as_str)
                .is_some_and(|s| !s.trim().is_empty());
            (bare_error && !has_data).then(message)
        }
    }
}

/// Print the response. The cloud `/v1` envelope is `{status,msg,data}` and what is shown is `data`. A list
/// prints as a table on a terminal and as JSON anywhere else, so a pipe still gets what it can parse; `-o`
/// names the format outright.
fn print_body(resp: &Value, output: Option<Output>) {
    let shown = resp.get("data").unwrap_or(resp);
    let output = output.unwrap_or_else(|| {
        if rows(shown).is_some() && std::io::stdout().is_terminal() {
            Output::Table
        } else {
            Output::Json
        }
    });
    if let Some(text) = render(shown, output, terminal_width()) {
        println!("{text}");
    }
}

/// An answer in a format: nothing for none, a string as written.
fn render(v: &Value, output: Output, width: usize) -> Option<String> {
    match (v, output) {
        (Value::Null, _) => None,
        (Value::String(s), _) => Some(s.clone()),
        (v, Output::Table) => Some(table(v, width)),
        (v, Output::Yaml) => Some(
            serde_norway::to_string(v)
                .unwrap_or_else(|_| v.to_string())
                .trim_end()
                .to_string(),
        ),
        (v, Output::Json) => Some(pretty(v)),
    }
}

fn terminal_width() -> usize {
    crossterm::terminal::size()
        .map(|(w, _)| w as usize)
        .unwrap_or(usize::MAX)
}

/// The list an answer is: the array itself, or the one array an object carries (`{"models":[…]}`).
fn rows(v: &Value) -> Option<&Vec<Value>> {
    match v {
        Value::Array(a) => Some(a),
        Value::Object(o) => {
            let mut arrays = o.values().filter_map(Value::as_array);
            let only = arrays.next()?;
            arrays.next().is_none().then_some(only)
        }
        _ => None,
    }
}

/// A scalar as a table cell: text as written, null as `-`.
fn cell(v: &Value) -> Option<String> {
    match v {
        Value::Null => Some("-".into()),
        Value::String(s) => Some(s.replace('\n', " ")),
        Value::Bool(_) | Value::Number(_) => Some(v.to_string()),
        Value::Array(_) | Value::Object(_) => None,
    }
}

/// An answer as a table: a list is one row per item, a single object one row per field. The columns are the
/// scalar keys (an object's keys are sorted, so they are), the identifying ones first, as many as fit `width`,
/// the first always; nested values are left to `-o json`.
fn table(v: &Value, width: usize) -> String {
    let (header, grid) = match (rows(v), v) {
        (Some(list), _) => match columns(list) {
            Some(shape) => shape,
            None => return pretty(v),
        },
        (None, Value::Object(o)) => fields(o),
        _ => return pretty(v),
    };
    lay(&header, &grid, width)
}

fn pretty(v: &Value) -> String {
    serde_json::to_string_pretty(v).unwrap_or_else(|_| v.to_string())
}

/// The keys that identify an item, which lead its row.
const LEAD: [&str; 2] = ["id", "name"];

/// A list's header and rows, or `None` when its items have nothing scalar to show.
fn columns(list: &[Value]) -> Option<(Vec<String>, Vec<Vec<String>>)> {
    let mut keys: Vec<&str> = Vec::new();
    for item in list.iter().filter_map(Value::as_object) {
        for (key, value) in item {
            if cell(value).is_some() && !keys.contains(&key.as_str()) {
                keys.push(key);
            }
        }
    }
    if keys.is_empty() {
        return None;
    }
    keys.sort_by_key(|key| {
        LEAD.iter()
            .position(|lead| lead == key)
            .unwrap_or(LEAD.len())
    });
    let grid = list
        .iter()
        .map(|item| {
            keys.iter()
                .map(|key| item.get(*key).and_then(cell).unwrap_or_else(|| "-".into()))
                .collect()
        })
        .collect();
    Some((keys.iter().map(|key| key.to_uppercase()).collect(), grid))
}

/// One object's header and rows: a field and its value, for each field that is a scalar.
fn fields(o: &Map<String, Value>) -> (Vec<String>, Vec<Vec<String>>) {
    let grid = o
        .iter()
        .filter_map(|(key, value)| Some(vec![key.clone(), cell(value)?]))
        .collect();
    (vec!["FIELD".into(), "VALUE".into()], grid)
}

/// The text of a table: each column as wide as its widest cell, two spaces between, and the columns that fit
/// `width` (the first always; one too wide is left out and the narrower ones after it are kept).
fn lay(header: &[String], grid: &[Vec<String>], width: usize) -> String {
    let widths: Vec<usize> = (0..header.len())
        .map(|i| {
            grid.iter()
                .map(|row| row[i].chars().count())
                .chain([header[i].chars().count()])
                .max()
                .unwrap_or(0)
        })
        .collect();
    let mut keep: Vec<usize> = Vec::new();
    let mut used = 0;
    for (i, w) in widths.iter().enumerate() {
        let next = used + if keep.is_empty() { 0 } else { 2 } + w;
        if keep.is_empty() || next <= width {
            used = next;
            keep.push(i);
        }
    }
    let line = |cells: &[String]| {
        let padded: Vec<String> = keep
            .iter()
            .map(|&i| format!("{:<w$}", cells[i], w = widths[i]))
            .collect();
        padded.join("  ").trim_end().to_string()
    };
    std::iter::once(line(header))
        .chain(grid.iter().map(|row| line(row.as_slice())))
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod membership_query_tests {
    use super::memberships_query_ok;

    #[test]
    fn exactly_one_question_is_asked() {
        assert!(memberships_query_ok(&["user=z".to_string()]));
        assert!(memberships_query_ok(&["org=hanzo".to_string()]));
        assert!(!memberships_query_ok(&[]));
        assert!(!memberships_query_ok(&["user=z".to_string(), "org=hanzo".to_string()]));
    }
}
