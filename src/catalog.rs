//! The platform's command surface, read once and grafted onto this tree.
//!
//! `GET https://api.hanzo.ai/v1/commands` is the platform's own projection of
//! the operations it serves: one row per operation carrying the service it
//! belongs to, the verb it is called by, the path parameters it takes and the
//! flags it accepts. That document is the ONE declaration of what a command is.
//!
//! So this CLI does not keep a second one. There is no generated Rust tree to
//! regenerate, no table to reconcile, and no window in which the two can
//! disagree: the clap tree is BUILT from the document at startup, which means
//! `hanzo <service> <verb>` answers exactly the set the platform publishes, by
//! construction rather than by convention. The failure this replaces was a
//! served token — `agents delete` — that the binary met with "unrecognized
//! subcommand", which is what a fold of one truth into two files always ends up
//! printing.
//!
//! A local verb keeps its name. `deploy`, `docs` and `mcp` are each both a
//! thing this CLI does to your machine and a service the platform serves, so
//! the served verbs are grafted UNDER the local command rather than over it and
//! neither loses.

use anyhow::{anyhow, bail, Context, Result};
use clap::parser::ValueSource;
use clap::{Arg, ArgAction, ArgMatches, Command};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::collections::btree_map::Entry;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::{Duration, SystemTime};

/// Where the document is served, under the one API origin and the one version.
pub const PATH: &str = "/v1/commands";

/// How long a read copy is reused before the document is read again. A cache is
/// derived, never edited: it exists so `hanzo up` costs no round trip, and the
/// check below never reads it.
const FRESH: Duration = Duration::from_secs(3600);

/// A path parameter, named exactly as the `:segment` it fills.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Param {
    #[serde(rename = "Name")]
    pub name: String,
    #[serde(rename = "Help")]
    pub help: Option<String>,
}

/// A body or query field, offered as a long flag.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Flag {
    #[serde(rename = "Name")]
    pub name: String,
    #[serde(rename = "Field")]
    pub field: String,
    #[serde(rename = "Type")]
    pub kind: String,
    #[serde(rename = "Help")]
    pub help: Option<String>,
    #[serde(rename = "Required", default)]
    pub required: bool,
}

/// One served operation, as the platform describes it.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Op {
    #[serde(rename = "Service")]
    pub service: String,
    #[serde(rename = "Name")]
    pub name: String,
    #[serde(rename = "OperationID")]
    pub id: String,
    #[serde(rename = "Summary")]
    pub summary: Option<String>,
    #[serde(rename = "Method")]
    pub method: String,
    #[serde(rename = "Path")]
    pub path: String,
    #[serde(rename = "Args", default)]
    pub args: Vec<Param>,
    #[serde(rename = "Flags", default)]
    pub flags: Option<Vec<Flag>>,
}

impl Op {
    /// The two words a caller types to reach this operation.
    #[cfg(test)]
    pub fn token(&self) -> (&str, &str) {
        (&self.service, &self.name)
    }
}

/// The document, indexed by the token it publishes.
pub struct Catalog {
    ops: BTreeMap<(String, String), Op>,
    /// Rows the platform publishes under a token another row already took. Two
    /// operations, one spelling — the caller can only ever reach the first.
    #[cfg_attr(not(test), allow(dead_code))]
    shadowed: Vec<Op>,
    /// Rows the platform publishes with no service, which name no token at all.
    #[cfg_attr(not(test), allow(dead_code))]
    nameless: Vec<Op>,
}

impl Catalog {
    /// Read the document from the platform. No cache on either side of this —
    /// it is what the check uses, and a check that answers from a copy is not
    /// checking anything.
    #[cfg(test)]
    pub async fn live(base: &str) -> Result<Self> {
        Ok(Self::of(fetch(base).await?))
    }

    /// Read the document for a run: the read copy if it is still fresh, the
    /// platform otherwise, and a stale copy rather than nothing when the
    /// platform cannot be reached.
    pub async fn open(base: &str) -> Result<Self> {
        if let Some(ops) = cached(FRESH) {
            return Ok(Self::of(ops));
        }
        match fetch(base).await {
            Ok(ops) => {
                keep(&ops);
                Ok(Self::of(ops))
            }
            Err(e) => Ok(Self::of(cached(Duration::MAX).ok_or(e)?)),
        }
    }

    fn of(rows: Vec<Op>) -> Self {
        let mut rows = rows;
        // Sorted so the row a shadowed token loses to is the same one on every
        // machine: the caller gets a stable answer, and the check names a
        // stable pair.
        rows.sort_by(|a, b| (&a.service, &a.name, &a.id).cmp(&(&b.service, &b.name, &b.id)));
        let (mut ops, mut shadowed, mut nameless) = (BTreeMap::new(), vec![], vec![]);
        for op in rows {
            if op.service.is_empty() {
                nameless.push(op);
                continue;
            }
            match ops.entry((op.service.clone(), op.name.clone())) {
                Entry::Vacant(seat) => {
                    seat.insert(op);
                }
                Entry::Occupied(_) => shadowed.push(op),
            }
        }
        Self { ops, shadowed, nameless }
    }

    /// What the graft could not reach, which only the check asks for: every
    /// operation that has a token, every row whose token another row already
    /// took, and every row published with no service and so no token at all.
    #[cfg(test)]
    pub fn ops(&self) -> impl Iterator<Item = &Op> {
        self.ops.values()
    }

    #[cfg(test)]
    pub fn shadowed(&self) -> &[Op] {
        &self.shadowed
    }

    #[cfg(test)]
    pub fn nameless(&self) -> &[Op] {
        &self.nameless
    }

    /// The operation a pair of words reaches, if any.
    pub fn op(&self, service: &str, name: &str) -> Option<&Op> {
        self.ops.get(&(service.to_string(), name.to_string()))
    }

    /// Graft the document onto a tree. A service the tree already has keeps its
    /// own arguments and gains the served verbs as subcommands; a service it
    /// does not have becomes one.
    pub fn graft(&self, mut root: Command) -> Command {
        let mut by_service: BTreeMap<&str, Vec<&Op>> = BTreeMap::new();
        for op in self.ops.values() {
            by_service.entry(&op.service).or_default().push(op);
        }
        for (service, ops) in by_service {
            let leaves: Vec<Command> = ops.iter().map(|op| leaf(op)).collect();
            let about = format!("{} — {} served commands", service, leaves.len());
            root = if root.find_subcommand(service).is_some() {
                root.mut_subcommand(service, move |c| {
                    leaves.into_iter().fold(c, |c, l| c.subcommand(l))
                })
            } else {
                root.subcommand(Command::new(service.to_string()).about(about).subcommands(leaves))
            };
        }
        root
    }
}

/// One served operation as a subcommand: its path parameters positionally, in
/// the order the path spells them, and its fields as long flags.
fn leaf(op: &Op) -> Command {
    let mut c = Command::new(op.name.clone()).about(op.summary.clone().unwrap_or_default());
    for a in &op.args {
        c = c.arg(
            Arg::new(a.name.clone())
                .required(true)
                .help(a.help.clone().unwrap_or_default()),
        );
    }
    for f in op.flags.iter().flatten() {
        let mut arg = Arg::new(f.name.clone())
            .long(f.name.clone())
            .required(f.required)
            .help(f.help.clone().unwrap_or_default());
        arg = if f.kind == "boolean" {
            arg.action(ArgAction::SetTrue)
        } else {
            arg.action(ArgAction::Set).value_name(f.kind.to_uppercase())
        };
        c = c.arg(arg);
    }
    // `--version` is a field on a handful of operations. Nothing is grafted
    // deeply enough for clap's own version flag to reach here, but saying so is
    // cheaper than finding out from a panic.
    c.disable_version_flag(true)
}

/// Call an operation: its path filled from the positionals, its fields sent as
/// query on a read and as a body on a write, with whatever identity IAM has
/// already issued this machine.
pub async fn call(op: &Op, m: &ArgMatches, base: &str) -> Result<()> {
    let mut url = format!("{}{}", base.trim_end_matches('/'), op.path);
    for a in &op.args {
        let v = m
            .get_one::<String>(&a.name)
            .ok_or_else(|| anyhow!("{} {} needs <{}>", op.service, op.name, a.name))?;
        url = url.replace(&format!(":{}", a.name), v);
    }

    let read = matches!(op.method.as_str(), "GET" | "HEAD" | "DELETE");
    let mut body = Map::new();
    let mut query: Vec<(String, String)> = vec![];
    for f in op.flags.iter().flatten() {
        let Some(v) = given(f, m) else { continue };
        if read {
            query.push((f.field.clone(), text(&v)));
        } else {
            body.insert(f.field.clone(), v);
        }
    }

    let method = reqwest::Method::from_bytes(op.method.as_bytes())
        .with_context(|| format!("{} is not a method", op.method))?;
    let mut req = reqwest::Client::new().request(method, &url).query(&query);
    if let Ok(creds) = crate::iam::credentials::Credentials::load() {
        if !creds.access_token.is_empty() {
            req = req.bearer_auth(&creds.access_token);
        }
    }
    if !read {
        req = req.json(&Value::Object(body));
    }

    let res = req.send().await.with_context(|| format!("calling {url}"))?;
    let status = res.status();
    let text = res.text().await.unwrap_or_default();
    let shown = serde_json::from_str::<Value>(&text)
        .map(|v| serde_json::to_string_pretty(&v).unwrap_or_else(|_| text.clone()))
        .unwrap_or(text);
    if !status.is_success() {
        bail!("{} {}\n{}", status.as_u16(), url, shown);
    }
    println!("{shown}");
    Ok(())
}

/// A field's value, or nothing when the caller did not give one. A boolean that
/// was never typed is absent, not false — the platform's own default stands.
fn given(f: &Flag, m: &ArgMatches) -> Option<Value> {
    if m.value_source(&f.name) != Some(ValueSource::CommandLine) {
        return None;
    }
    if f.kind == "boolean" {
        return Some(Value::Bool(m.get_flag(&f.name)));
    }
    let raw = m.get_one::<String>(&f.name)?;
    Some(match f.kind.as_str() {
        "integer" | "number" => raw.parse::<serde_json::Number>().map(Value::Number).ok(),
        _ => None,
    }
    .unwrap_or_else(|| Value::String(raw.clone())))
}

fn text(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// Long enough to read three megabytes over a bad connection, short enough that
/// a laptop with no network still runs a local verb instead of hanging on a TCP
/// handshake that will never complete.
const PATIENCE: Duration = Duration::from_secs(10);

async fn fetch(base: &str) -> Result<Vec<Op>> {
    let url = format!("{}{}", base.trim_end_matches('/'), PATH);
    let res = reqwest::Client::builder()
        .timeout(PATIENCE)
        .build()?
        .get(&url)
        .send()
        .await
        .with_context(|| format!("reading {url}"))?;
    if !res.status().is_success() {
        bail!("reading {url}: HTTP {}", res.status().as_u16());
    }
    res.json().await.with_context(|| format!("parsing {url}"))
}

fn copy() -> Option<PathBuf> {
    Some(dirs::cache_dir()?.join("hanzo").join("commands.json"))
}

fn cached(within: Duration) -> Option<Vec<Op>> {
    let path = copy()?;
    let meta = std::fs::metadata(&path).ok()?;
    if SystemTime::now().duration_since(meta.modified().ok()?).ok()? > within {
        return None;
    }
    serde_json::from_slice(&std::fs::read(&path).ok()?).ok()
}

fn keep(ops: &[Op]) {
    let Some(path) = copy() else { return };
    let Some(dir) = path.parent() else { return };
    if std::fs::create_dir_all(dir).is_err() {
        return;
    }
    if let Ok(bytes) = serde_json::to_vec(ops) {
        let _ = std::fs::write(path, bytes);
    }
}
