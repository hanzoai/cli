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

use clap::{Arg, ArgAction, ArgMatches, Command};
use serde_json::{json, Map, Value};

mod generated;
pub(crate) use generated::{OPS, PRODUCTS};

// The table's TYPES are their own module because `src/bin/driftgate.rs` reads the
// same generated table — see `op.rs`.
mod op;
pub use op::{Field, Op, Ty};

/// A non-2xx HTTP status returned by the cloud API. 4xx responses are normal
/// user, auth, or validation refusals; 5xx responses are upstream server/gateway
/// faults already tracked server-side. Neither is a CLI client crash.
#[derive(Debug)]
pub struct HttpStatusError {
    pub path: String,
    pub status: u16,
    pub detail: String,
    pub hint: Option<&'static str>,
}

impl std::fmt::Display for HttpStatusError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} -> {}: {}", self.path, self.status, self.detail)?;
        if let Some(hint) = self.hint {
            write!(f, " — {hint}")?;
        }
        Ok(())
    }
}

impl std::error::Error for HttpStatusError {}

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
    if is_agent_run(op) {
        // POST /v1/agent/{ref}/run is SYNCHRONOUS. A one-shot bot can hold that
        // request open past the gateway's timeout while the routed run keeps
        // going. The async route is POST /v1/agent/coding, which answers 202 with
        // a session id the existing `agent sessions` commands can poll. These
        // controls are mounted ONLY here: every other generated leaf keeps the
        // contract cloud's document declares.
        c = c
            .arg(
                Arg::new("async")
                    .long("async")
                    .action(ArgAction::SetTrue)
                    .help("submit async and return the session handle at once"),
            )
            .arg(
                Arg::new("wait")
                    .long("wait")
                    .action(ArgAction::SetTrue)
                    .requires("async")
                    .help("with --async, follow the session until it finishes"),
            )
            .arg(
                Arg::new("timeout")
                    .long("timeout")
                    .value_name("SECONDS")
                    .value_parser(clap::value_parser!(u64).range(1..))
                    .requires("wait")
                    .help("with --wait, stop following after SECONDS (default 300)"),
            );
    }
    c.arg(output_arg())
}

/// The one generated op whose POST is synchronous and long enough to outlive a
/// gateway. Named by its coordinate (not a string match on the route) so a move
/// in cloud shows up as a failed pin in `exactly_one_agent_run_has_async` rather
/// than as flags silently going missing.
fn is_agent_run(op: &Op) -> bool {
    op.product == "agent" && op.nodes.is_empty() && op.verb == "run" && op.method == "POST"
}

/// How an answer is printed (`-o`): a table, JSON or YAML. One formatter serves every generated command.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Output {
    Table,
    Json,
    Yaml,
}

pub(super) fn output_arg() -> Arg {
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

/// The words a person types to run an operation: product, resource nodes, verb.
pub(super) fn command_words(op: &Op) -> String {
    let mut words = vec![op.product];
    words.extend(op.nodes);
    words.push(op.verb);
    words.join(" ")
}

/// The refusal for `--data` on an operation that types its body: the flags it takes, by name.
pub(super) fn flags_not_data(op: &'static Op) -> String {
    let flags: Vec<String> = op
        .fields
        .iter()
        .filter(|f| !f.query && !f.secret)
        .map(|f| format!("--{}", field_flag(f)))
        .collect();
    format!(
        "`hanzo {}` takes its body as flags, not --data: {}",
        command_words(op),
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
pub(super) fn field_flag(f: &'static Field) -> &'static str {
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

/// How one call waits for the work it starts. `Sync` IS the generated contract:
/// only `agent run` offers the other two, so every other command is unchanged.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Submit {
    #[default]
    Sync,
    /// Submit through `POST /v1/agent/coding`, print the session id, return.
    Async,
    /// Submit async, then poll the session until it ends or SECONDS elapse.
    Wait(u64),
}

/// What a matched generated command resolves to. Pure over the clap matches —
/// no config, no keychain — so it is unit-testable without a network.
pub enum Resolved {
    Leaf {
        op: &'static Op,
        values: Vec<String>,
        body: LeafBody,
        query: Vec<String>,
        output: Option<Output>,
        submit: Submit,
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
    let submit = if is_agent_run(op) && m.get_flag("async") {
        match m.get_flag("wait") {
            false => Submit::Async,
            true => Submit::Wait(m.get_one::<u64>("timeout").copied().unwrap_or(300)),
        }
    } else {
        Submit::Sync
    };
    Resolved::Leaf {
        op,
        values,
        body,
        query: typed_query(op, m),
        output: output_of(m),
        submit,
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
            // A repeatable JSON/Union flag represents a flat 1D schema array
            // (genproduct classifies nested arrays with repeat=false). Callers
            // naturally pass either repeated item objects (`--files '{…}' --files '{…}'`)
            // or a JSON array (`--files '[{…},{…}]'`); flatten top-level JSON arrays
            // so the latter is not double-wrapped into `[[{…}]]`.
            Ty::Json | Ty::Union => m
                .get_many::<Value>(f.id)?
                .flat_map(|v| match v {
                    Value::Array(items) => items.clone(),
                    other => vec![other.clone()],
                })
                .collect(),
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
pub(super) fn insert_path(map: &mut Map<String, Value>, key: &str, value: Value) {
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
pub(super) fn memberships_query_ok(query: &[String]) -> bool {
    let has = |key: &str| query.iter().any(|q| q.starts_with(&format!("{key}=")));
    has("user") != has("org")
}


mod dispatch;
pub use dispatch::dispatch;
pub(crate) use dispatch::{call, envelope_error, request_id, Seam};
#[cfg(test)]
use dispatch::*;

#[cfg(test)]
mod tests;
#[cfg(test)]
mod tests_routes;
#[cfg(test)]
mod tests_secrets;

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
