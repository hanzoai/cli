//! `hanzo <product> <resource…> <verb>` — cloud's Go surface as FIRST-CLASS
//! product commands, generated from the router's SHAPE.
//!
//! Cloud serves ~1000 `/v1` operations across ~130 products. `hanzo api` reaches
//! every one through the one seam; this gives the enumerable ones real verbs with
//! real `--help`, WITHOUT hand-writing them: a build-time generator folds each
//! path into a (product, resource nodes, verb, method, path template, params)
//! coordinate and commits it as pure DATA (`generated.rs`). At runtime we build a
//! clap tree from that data and dispatch through the SAME `api::call` seam — so
//! the trust boundary is identical to `hanzo api`: the ORIGIN comes from
//! `network`, the BEARER from `store`, and the data contributes only the path
//! template + which segments are positionals. It contains no host, no URL and no
//! auth (a test fails the build otherwise), so a hostile snapshot can at worst
//! shape a call to YOUR OWN cloud with YOUR OWN token — never redirect it.
//!
//! The spec is router-derived and SHAPE-ONLY: it carries no request/response
//! schemas. So every write is `--data '<json>'` (or `--data -` from stdin),
//! exactly as `hanzo api`; there are no typed body flags because there are no
//! body types to generate.

use anyhow::{anyhow, bail, Result};
use clap::{Arg, ArgAction, ArgMatches, Command};

use crate::config::Config;
use crate::iam::{paths, store};

mod generated;
pub(crate) use generated::{OPS, PASSTHROUGH};

/// One generated operation — pure DATA derived from the router SHAPE.
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
/// The flagship `code` wrapper is special: its `/v1/code` verbs mount UNDER it,
/// so `hanzo code "task"` still runs the wrapper and `hanzo code ask|search` hit
/// cloud.
pub fn augment(mut cmd: Command) -> Command {
    let taken: std::collections::HashSet<String> =
        cmd.get_subcommands().map(|s| s.get_name().to_string()).collect();

    // Nest the /v1/code verbs under the existing wrapper. clap's `mut_subcommand`
    // panics on an absent name, so guard on the wrapper actually being present
    // (it always is in the real tree; a bare test tree may omit it).
    if taken.contains("code") {
        let code_leaves: Vec<Command> =
            OPS.iter().filter(|o| o.product == "code").map(leaf).collect();
        if !code_leaves.is_empty() {
            cmd = cmd.mut_subcommand("code", |c| {
                code_leaves.into_iter().fold(c, |c, l| c.subcommand(l))
            });
        }
    }

    for product in product_names() {
        if product == "code" || taken.contains(product) {
            continue;
        }
        cmd = cmd.subcommand(build_product(product));
    }
    for &product in PASSTHROUGH {
        if taken.contains(product) {
            continue;
        }
        cmd = cmd.subcommand(passthrough(product));
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

/// A leaf command: positionals for the path params, plus the SHAPE-ONLY write
/// controls (`--data`/`--query`/`--raw`) shared verbatim with `hanzo api`.
fn leaf(op: &'static Op) -> Command {
    let mut c = Command::new(op.verb).about(format!("{} {}", op.method, op.path));
    for &p in op.params {
        c = c.arg(Arg::new(p).required(true).help(format!("path parameter {{{p}}}")));
    }
    if op.is_write() {
        c = c.arg(data_arg());
    }
    c.arg(query_arg()).arg(raw_arg())
}

fn passthrough(product: &'static str) -> Command {
    Command::new(product)
        .about(format!("Passthrough to /v1/{product}/* (wildcard product — no fixed verbs)"))
        .arg(
            Arg::new("subpath")
                .value_name("SUBPATH")
                .help(format!("sub-path under /v1/{product}, e.g. `queues/default`")),
        )
        .arg(
            Arg::new("method")
                .short('X')
                .long("method")
                .default_value("GET")
                .help("HTTP method: GET|POST|PUT|PATCH|DELETE|HEAD"),
        )
        .arg(data_arg())
        .arg(query_arg())
        .arg(raw_arg())
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
        data: Option<String>,
        query: Vec<String>,
        raw: bool,
    },
    Pass {
        product: &'static str,
        subpath: Option<String>,
        method: String,
        data: Option<String>,
        query: Vec<String>,
        raw: bool,
    },
}

/// If `matches` selected a GENERATED product (or a `hanzo code` verb), resolve
/// it; otherwise `None` so the derive tree handles it. Bare `hanzo code` and
/// `hanzo code "task"` resolve to `None` — the wrapper, not a cloud verb.
pub fn resolve(matches: &ArgMatches) -> Option<Resolved> {
    let (top, sub) = matches.subcommand()?;

    // A pure catch-all product: forward the sub-path (the &'static name outlives
    // the borrowed matches, so it can travel into `Resolved`).
    if let Some(&product) = PASSTHROUGH.iter().find(|&&p| p == top) {
        return Some(pass(product, sub));
    }
    if top == "code" {
        sub.subcommand()?; // bare/`task` -> None -> the wrapper owns it
    } else if !is_product(top) {
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
    Some(Resolved::Leaf {
        op,
        values,
        // `--data` exists only on write leaves — never look it up on a read leaf.
        data: op.is_write().then(|| m.get_one::<String>("data").cloned()).flatten(),
        query: m.get_many::<String>("query").map(|v| v.cloned().collect()).unwrap_or_default(),
        raw: m.get_flag("raw"),
    })
}

fn pass(product: &'static str, sub: &ArgMatches) -> Resolved {
    Resolved::Pass {
        product,
        subpath: sub.get_one::<String>("subpath").cloned(),
        method: sub.get_one::<String>("method").cloned().unwrap_or_else(|| "GET".into()),
        data: sub.get_one::<String>("data").cloned(),
        query: sub.get_many::<String>("query").map(|v| v.cloned().collect()).unwrap_or_default(),
        raw: sub.get_flag("raw"),
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

/// Bind the resolved call to a concrete request and send it through `api::call`
/// — the SAME seam `hanzo api` uses. The org scope is filled from the active
/// identity's `owner` (via the seam), never asked; all other params are the
/// user's positionals. The bearer + origin are `api::call`'s to resolve.
pub async fn dispatch(cfg: &mut Config, resolved: Resolved) -> Result<()> {
    match resolved {
        Resolved::Leaf { op, values, data, query, raw } => {
            let owner = store::active(cfg, paths::DEFAULT_BRAND).map(|i| i.owner);
            let path = fill_path(op.path, owner.as_deref(), &values)?;
            let method = super::api::parse_method(op.method)?;
            let body = super::api::read_body(data, &method)?;
            super::api::call(cfg, method, path, body, query, raw).await
        }
        Resolved::Pass { product, subpath, method, data, query, raw } => {
            let method = super::api::parse_method(&method)?;
            let path = passthrough_path(product, subpath.as_deref())?;
            let body = super::api::read_body(data, &method)?;
            super::api::call(cfg, method, path, body, query, raw).await
        }
    }
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

fn passthrough_path(product: &str, sub: Option<&str>) -> Result<String> {
    let mut p = format!("/v1/{product}");
    if let Some(s) = sub {
        for seg in s.trim_matches('/').split('/').filter(|s| !s.is_empty()) {
            match seg {
                "." | ".." => bail!("'{seg}' is not a path segment"),
                _ => {
                    p.push('/');
                    p.push_str(&enc(seg));
                }
            }
        }
    }
    Ok(p)
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

#[cfg(test)]
mod tests;
