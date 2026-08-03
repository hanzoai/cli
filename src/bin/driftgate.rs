//! `driftgate` — the shipped command surface and the live route table, held
//! against each other in BOTH directions, with the running host as the arbiter.
//!
//! The two ways a CLI and its server come apart are not the same defect and are
//! not found the same way, so the gate asks both questions and reports them
//! apart:
//!
//!   * PHANTOM — a route a command can address that the server does not serve.
//!     `hanzo` v1.7.2 shipped 149 of these; a later spec carried 129 more; the
//!     last four were `hanzo load-balancers …`, deleted upstream nine minutes
//!     after the spec that shipped them was cut.
//!   * ORPHAN — a product the server serves that no command reaches. 46 of these
//!     shipped at once when a stale capture was used as the route table, and 21
//!     `/v1/deploy` operations reached nobody for as long as a curation entry
//!     reserved the name for a local command that had been deleted.
//!
//! # 404 IS NOT 403, AND ONE 404 IS NOT A 404
//!
//! Everything here turns on that. `401`/`403` say the route is THERE and wants a
//! caller who is signed in; `404` says there is nothing at that address. An
//! earlier hand analysis of this exact surface conflated them and reported three
//! production breaks that were not breaks. A gate that makes that mistake is
//! worse than no gate: it is a red build that teaches people the build lies.
//!
//! The same mistake has a second floor, and this gate fell through it while it
//! was being built: fourteen `/v1/pricing` paths answered `404` to one concurrent
//! sweep and `200` to every serial re-ask a minute later. So a `404` is CONFIRMED
//! before it counts — re-asked serially, three times — and a `404` that does not
//! hold is `Flapping`: present, reported, never drift.
//!
//! | answer                          | verdict   |
//! |---------------------------------|-----------|
//! | `404` three times, serially     | ABSENT    |
//! | `404` that did not repeat       | FLAPPING (present, and said so) |
//! | `401` `403` `405` `2xx` `5xx` … | PRESENT   |
//! | no answer at all                | BLIND     |
//!
//! BLIND fails. A gate that cannot see must not pass — the one thing it may
//! never do is report "no drift" when what it means is "I could not look".
//!
//! # WHO IS ASKED, AND WHO CAN BE ASKED
//!
//! The route table answers first, because it is free and it is a projection of
//! the router itself:
//!
//!   * it OWNS the product and names no such
//!     route                                 → REFUTED. The table is complete for
//!     a product it serves, so this is decided without asking anyone.
//!   * it NAMES the route exactly            → served — but the table projects the
//!     ROUTER, and a route can be registered with a dead mount behind it, which
//!     the router cannot know and the table therefore cannot say. Ask anyway.
//!   * it names only a DOOR (`/v1/iam/*`)    → a door is not an answer. A `*`
//!     catch-all says something is mounted behind it, never what. Ask.
//!   * it is silent about the product        → the table this gate holds is one
//!     RELEASE's projection and the host is whatever is deployed, so silence can
//!     be skew rather than absence. Ask.
//!
//! THERE IS NO EDGE EXCEPTION, and the sentence that used to sit in the last
//! bullet — "the inference surface is answered at the edge" — was false. Measured
//! 2026-08-03 against api.hanzo.ai, every probe with a nonsense sibling under the
//! same prefix as its control: `GET /v1/models` 200 vs `/v1/models-zzq` 404;
//! `POST /v1/chat/completions` 401 vs `-zzq` 404 (and `GET` of it 405, POST-only);
//! `POST /v1/embeddings` 401 vs 404; `GET /v1/tools` 403 vs 404; `POST /v1/event`
//! 401 vs 404 — all `server: hanzo`, all `x-api-version: v1.801.383`, the same
//! router that answers the 404. All of them are in the emitted document and in
//! `spec/cloud.json`. A rationalization in a doc comment is how a whole false
//! category of "answered somewhere this pipeline cannot see" stayed alive, and it
//! was the stated reason a second authority was allowed to describe it.
//!
//! Roughly a third of this spec's operations sit behind a door or in a namespace
//! the table never mentions — a surface on which the document is constitutionally
//! unable to testify. That is the hole this gate
//! exists to close, and only the host can close it.
//!
//! But only a `GET` on a LITERAL path can be put to a host, and both halves of
//! that were measured rather than assumed:
//!
//!   * a `{param}` makes `404` mean "no route" OR "no such id", and a gate must
//!     not read a sentence with two meanings;
//!   * cloud's router answers `404`, not `405`, to a verb it does not have at a
//!     path it does — `POST /v1/admin/credits` is in the live table and a `GET`
//!     of it `404`s — so a `GET` says nothing about a `POST`-only route.
//!
//! Everything else is UNDECIDABLE and is counted as such. Naming that gap is
//! honest; closing it by guessing would not be. And the probe is a `GET` for the
//! reason it is only ever a `GET`: a gate that DELETEs to find out whether
//! something is there is not a gate.
//!
//! When the host does say ABSENT, whose defect it is turns on who claimed the
//! route. If no document named it, the CLI is carrying a route the server denies
//! — ours, and a hard failure. If the live table named it, cloud's own table and
//! cloud's own server disagree; no edit in this repo can settle that, so it is
//! reported against a CEILING that may not grow in silence and is free to fall
//! the moment somebody redeploys.
//!
//! # THE OTHER DIRECTION
//!
//! Reachability is asked of the BINARY — `hanzo <product> --help` — not derived
//! a second time from the data `genproduct` folded. Whether a person can reach a
//! product is a fact about the built CLI, and only the built CLI knows it: a
//! generated product that collides with a local command, or a relocation that
//! quietly stopped relocating, is invisible to any re-derivation and obvious to
//! one exec.
//!
//! A served product with no command is drift unless `src/curation.rs` says
//! otherwise. Those entries are the DECLARED exceptions: each names a spelling
//! the gate runs — so an excuse that stopped being true turns CI red — or says
//! in words that nothing reaches the surface. The gate counts the ones it had to
//! apply and pins the count, because an exception nobody counts is how 21 served
//! `/v1/deploy` operations came to be reachable by nothing while the table that
//! dropped them still called it a decision.
//!
//! # THE HALF THAT NEEDS NOTHING BUT THE DOCUMENT
//!
//! Both questions above need the network. NAMING does not: whether a path says
//! its own word twice is decidable from the document alone. `--lint` runs that
//! half and exits, so `cargo test` can enforce it on every push from a bare
//! checkout — see tests/spec_drift.rs. Same binary, same rules, same exit code;
//! a gate that runs a different derivation than the one that ships is a gate
//! about nothing.
//!
//! Usage: `driftgate [--lint] [--registry <url|path>] [--host <url>]
//!                   [--spec <path>] [--hanzo <path>]`

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::process::Command;

use serde_json::{Map, Value};

#[path = "../curation.rs"]
mod curation;
use curation::{Curated, Instead};

const VERBS: [&str; 5] = ["get", "post", "put", "patch", "delete"];
const DEFAULT_REGISTRY: &str = "https://api.hanzo.ai/v1/openapi.json";
const DEFAULT_HOST: &str = "https://api.hanzo.ai";

/// How many served products the curation table had to excuse the last time
/// anyone looked. Pinned, and asserted EXACTLY: the number is a claim about the
/// fleet, and a claim that changed must be restated by a person in the same
/// commit that changes it. Up means a new gap was excused without a decision;
/// down means one was closed and the ceiling should come with it.
const EXCUSED: usize = 7;

/// Routes cloud's own live table names and cloud's own host answers 404 to — a
/// route registered with a dead mount behind it. A CEILING, not an equality, and
/// the asymmetry is deliberate: see where it is applied.
///
/// 3 -> 6 at v1.801.383, and both halves of the move are worth reading.
///
/// The old 3 are GONE: `/v1/billing/{gpu-eligibility,payment-config,
/// payment-methods}` were fixed in cloud's router. The new 6 are ONE event —
/// `/v1/ai/{applications,permissions,sessions,sessions/duplicated,users,
/// users/table-infos}`. cloud renamed those resources (applications→deployments,
/// sessions→signin-sessions, users→usages, permissions back to IAM) and the
/// deployed binary's ROUTER serves the new nouns while the DOCUMENT that same
/// binary publishes still advertises the old ones. Measured, with controls:
/// `GET /v1/ai/deployments` 401 (routed, wants a caller) and
/// `GET /v1/ai/applications` 404, both `x-api-version: v1.801.383`.
///
/// The cause is a second authority INSIDE cloud, which is the same disease this
/// pipeline just cured on its own side: `apps/ai` projects from the committed
/// `plugin/ai/openapi.json` subset instead of the mounted plugin's live registry,
/// so its published names lag its routes. Nothing in this repo can settle it — the
/// fix is in hanzoai/cloud (task #146's seam), and the number is here so it cannot
/// be settled by forgetting.
///
/// TWO MORE WERE NOT REAL and are not counted: `/v1/o11y/complete/{google,oidc}`
/// answer `303`, and the transport used to follow the redirect and record the
/// landing page's 404 against the callback's name. Fixed where the client is
/// built; a redirect is an answer.
const CONTRADICTED: usize = 6;

// ---- naming ------------------------------------------------------------------
//
// THE THIRD THING THIS GATE RULES ON, and it belongs here rather than in a linter
// of its own for one reason: a name is only wrong on the WIRE. `/v1/index/indexes`
// is a perfectly good Go package and a bad URL, so the only place the rule can be
// checked is the emitted document — the same artifact the phantom check already
// reads, at the same moment, with the same exit code. A second tool over the same
// file would be a second authority about the same question.
//
// It is pure and needs NO network, which is what lets it run in `make verify` and
// in `cargo test` beside the derivation gates rather than in a nightly nobody
// watches. Three rules, and they are not the same rule twice:
//
//   (a) a child never repeats its parent — ADJACENT literal segments, at any
//       depth. `/v1/index/indexes`, `/v1/traces/trace`. A `{param}` between two
//       words separates them and resets the comparison.
//   (b) a collection root never repeats the product — the ONE segment directly
//       under the product namespace, because that is where a collection is
//       named. `/v1/domain/domains`.
//   (c) no upstream brand on our surface. `meili`, `minio` — the vendor we happen
//       to run is not the product a customer types, and a brand in a path
//       outlives the vendor. The upstream CRATE name (`milli`) is a real
//       dependency and is not in this list: the rule is about our surface.
//
// (b) OVERLAPS (a) at the collection root and that is deliberate, not an
// oversight: the two report the same path with different reasons and different
// fixes ("rename this collection" vs "this segment repeats the one above it"),
// and the message is what a person acts on.
//
// (b) IS DELIBERATELY NARROW. The first draft read "the product word anywhere
// below the product", which is a rule this surface genuinely breaks three times
// for good reasons — `/v1/search/indexes/{uid}/search` (a verb),
// `/v1/dataroom/analytics/dataroom/{id}` (which rollup), `/v1/books/scan/book`
// (what a scan posts). Those are not collection roots and the repeat carries
// meaning. A lint that condemns them is one people learn to route around, so it
// judges the collection root and nothing else.

/// Upstream vendor names that may not appear in a path we serve.
const BRANDS: [&str; 2] = ["meili", "minio"];

/// A path we still publish that breaks a rule above, with WHO can fix it. A
/// violation is only excused here when the fix is in another repo — this gate
/// reads hanzoai/cloud's document and cannot edit hanzoai/metrics — and the
/// entry has to name that repo, because an exception whose owner is unnamed is
/// how a "temporary" one becomes permanent. Pinned by `NAMED` so it cannot grow
/// in silence; a violation OWNED BY CLOUD belongs in cloud, never here.
const NAMED: [(&str, &str); 1] = [(
    "/v1/traces/trace",
    "hanzoai/metrics mount.go:164 — a trace is addressed by ?id=, so the fix is \
     /v1/traces/{id}; needs a metrics release and a cloud dep bump",
)];

/// Crude English singular, enough to see a plural child beside its singular
/// parent. `indexes`->`index`, `datarooms`->`dataroom`, `policies`->`policy`.
/// It only ever has to make two segments of OUR OWN vocabulary compare equal.
fn singular(s: &str) -> String {
    let s = s.to_ascii_lowercase();
    if s.len() > 4 && s.ends_with("ies") {
        return format!("{}y", &s[..s.len() - 3]);
    }
    if s.len() > 3 && s.ends_with("es") {
        let stem = &s[..s.len() - 2];
        if ["s", "x", "z", "ch", "sh"].iter().any(|s| stem.ends_with(s)) {
            return stem.to_string();
        }
    }
    if s.len() > 2 && s.ends_with('s') && !s.ends_with("ss") {
        return s[..s.len() - 1].to_string();
    }
    s
}

/// The three rules, over the document's own paths. Returns (a, b, c) as report
/// rows. Pure: same input, same answer, no clock and no socket.
fn naming(paths: &[String]) -> (Vec<String>, Vec<String>, Vec<String>) {
    let (mut a, mut b, mut c) = (Vec::new(), Vec::new(), Vec::new());
    let excused: std::collections::BTreeSet<&str> = NAMED.iter().map(|(p, _)| *p).collect();
    for p in paths {
        if excused.contains(p.as_str()) {
            continue;
        }
        // (a) ADJACENCY. A `{param}` is a value, not a word, and it genuinely does
        // separate the two words either side of it — `/v1/domain/{id}/domains` is
        // "the domains OF this domain", which reads fine and is rule (b)'s to
        // judge, not this one's. So a param RESETS the comparison rather than
        // being skipped over; collapsing params here is what made this rule
        // silently do (b)'s job and report the wrong reason.
        let mut prev: Option<String> = None;
        for s in segs(p).into_iter().filter(|s| *s != "v1") {
            if is_param(s) {
                prev = None;
                continue;
            }
            let cur = singular(s);
            if prev.as_deref() == Some(cur.as_str()) {
                a.push(format!("{p:<48}`{cur}` repeats its parent"));
                break;
            }
            prev = Some(cur);
        }
        // (b) THE COLLECTION ROOT, and only that — the one segment directly under
        // the product, which is where a collection is named. Not "the product word
        // anywhere below", which is a different and much weaker rule: it condemned
        // `/v1/search/indexes/{uid}/search` (search is a VERB on an index),
        // `/v1/dataroom/analytics/dataroom/{id}` (which of the two analytics
        // rollups this is) and `/v1/books/scan/book` (the bookkeeping entry a
        // scan posts) — three names where the repeat carries meaning and the
        // collection root is innocent. A lint that cries on those is one people
        // route around.
        // Positional in the PATH, not in the literals: the collection root is the
        // segment immediately after the product. If a `{param}` sits there, the
        // product's collection root is unnamed and there is nothing to judge.
        let raw: Vec<&str> = segs(p).into_iter().filter(|s| *s != "v1").collect();
        if let [product, root, ..] = raw[..] {
            if !is_param(product) && !is_param(root) && singular(product) == singular(root) {
                b.push(format!(
                    "{p:<48}`{root}` under `{product}` — the product namespace IS the collection"
                ));
            }
        }
        for s in segs(p).into_iter().filter(|s| !is_param(s)) {
            let low = s.to_ascii_lowercase();
            if let Some(brand) = BRANDS.iter().find(|b| low.contains(**b)) {
                c.push(format!("{p:<48}`{brand}` is an upstream vendor, not our product"));
            }
        }
    }
    (a, b, c)
}

// ---- the route table ---------------------------------------------------------

fn segs(p: &str) -> Vec<&str> {
    p.split('/').filter(|s| !s.is_empty()).collect()
}
fn is_param(s: &str) -> bool {
    s.starts_with('{') && s.ends_with('}')
}
/// A fiber `*` catch-all, which `openapi.translate` names `{wildcardN}`.
fn is_wild(s: &str) -> bool {
    is_param(s) && s.contains("wild")
}

/// What the live route table knows: the patterns it serves per method, and the
/// products it is the authority over.
struct Table {
    routes: BTreeMap<String, Vec<Vec<String>>>,
    owned: BTreeSet<String>,
}

/// The table's answer about one operation. `Door` and `Silent` are not answers —
/// they are the table saying it cannot answer, which is why they carry a probe.
enum Says {
    Serves,
    Refutes,
    Door,
    Silent,
}

impl Table {
    fn read(doc: &Value) -> Self {
        let (mut routes, mut owned) = (BTreeMap::<String, Vec<Vec<String>>>::new(), BTreeSet::new());
        for (path, item) in doc.get("paths").and_then(Value::as_object).into_iter().flatten() {
            let s = segs(path);
            // The bare `/v1/*` is the global fallthrough. Counting it as evidence
            // would make every conceivable path "served" and the gate a no-op.
            if s.len() < 2 || s[0] != "v1" || (s.len() == 2 && is_wild(s[1])) {
                continue;
            }
            if !is_param(s[1]) {
                owned.insert(s[1].to_string());
            }
            let pat: Vec<String> = s.iter().map(|x| (*x).to_string()).collect();
            for (m, _) in item.as_object().into_iter().flatten() {
                if VERBS.contains(&m.to_ascii_lowercase().as_str()) {
                    routes.entry(m.to_ascii_uppercase()).or_default().push(pat.clone());
                }
            }
        }
        Table { routes, owned }
    }

    /// Segment by segment: a literal matches itself, a `{param}` matches a
    /// `{param}` (the router's names are its own), a `{wildcardN}` swallows the
    /// rest.
    fn matches(pat: &[String], path: &[&str]) -> bool {
        for (k, ps) in pat.iter().enumerate() {
            if is_wild(ps) {
                return path.len() > k;
            }
            match path.get(k) {
                None => return false,
                Some(seg) if is_param(ps) => {
                    if !is_param(seg) {
                        return false;
                    }
                }
                Some(seg) if ps != seg => return false,
                _ => {}
            }
        }
        path.len() == pat.len()
    }

    fn says(&self, method: &str, path: &[&str]) -> Says {
        match self.routes.get(method).and_then(|r| r.iter().find(|p| Self::matches(p, path))) {
            Some(p) if p.iter().any(|s| is_wild(s)) => Says::Door,
            Some(_) => Says::Serves,
            None if self.owned.contains(path[1]) => Says::Refutes,
            None => Says::Silent,
        }
    }

    fn products(&self) -> BTreeSet<String> {
        self.owned.clone()
    }
}

/// Every product either reading names — the universe the ORPHAN direction asks
/// about. The live table's products plus the spec's, because the two are one
/// document at two commits: a product cloud typed after the pinned release is in
/// the live table and not the spec, and one retired since is in the spec and not
/// the table. Neither is a category of route this pipeline cannot see.
fn products(doc: &Value) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for path in doc.get("paths").and_then(Value::as_object).into_iter().flatten().map(|(p, _)| p) {
        let s = segs(path);
        if s.len() >= 2 && s[0] == "v1" && !is_param(s[1]) {
            out.insert(s[1].to_string());
        }
    }
    out
}

// ---- the arbiter -------------------------------------------------------------

#[derive(Clone, Copy, PartialEq)]
enum Probe {
    /// Three `404`s in a row. Nothing is routed here.
    Absent,
    Present(u16),
    /// A `404` that did not hold up. The route exists — a router with no such
    /// route cannot produce a `200` — but it is intermittently answering as if it
    /// did not, which is a production symptom worth naming and NOT drift.
    Flapping(u16),
    Blind,
}

/// How many serial `404`s confirm one.
const CONFIRM: usize = 3;

/// THE RULE OF THIS GATE, and it is a pure function of the answers so that it can
/// be stated as a test rather than only as a paragraph — see the tests at the foot
/// of this file. It is kept apart from the transport for exactly that reason.
///
/// `401`/`403` say the route is THERE and wants a caller who is signed in; `405`
/// says the path is routed and the verb is not; only `404` says there is nothing at
/// that address. An earlier hand analysis of this exact surface conflated them and
/// reported three production breaks that were not breaks.
///
/// And A SINGLE 404 IS NOT EVIDENCE. Measured on this surface: fourteen
/// `/v1/pricing` paths answered 404 to one concurrent sweep and 200 to every serial
/// re-ask a minute later. So a 404 counts only once it has held [`CONFIRM`] times;
/// one that did not hold is [`Probe::Flapping`] — present, reported, never drift.
/// A sequence that ran out before confirming, or that went silent, is
/// [`Probe::Blind`]: no answer is not "no drift", it is "I could not look".
fn verdict(answers: &[Option<u16>]) -> Probe {
    let mut seen404 = 0;
    for a in answers {
        match *a {
            Some(404) => seen404 += 1,
            Some(code) if seen404 > 0 => return Probe::Flapping(code),
            Some(code) => return Probe::Present(code),
            None => return Probe::Blind,
        }
    }
    if seen404 >= CONFIRM {
        Probe::Absent
    } else {
        Probe::Blind
    }
}

/// Ask the host whether anything is routed at this path, and hand the answers to
/// [`verdict`]. Read-only by construction: a `GET`, and a `405` is a PRESENT
/// answer, not a failure.
async fn probe(client: &reqwest::Client, host: &str, path: &str) -> Probe {
    let url = format!("{}{}", host.trim_end_matches('/'), path);
    let mut answers = Vec::with_capacity(CONFIRM);
    // Up to CONFIRM rounds, serially; each tolerates one transport failure, because
    // a dropped connection is not evidence about a route. Only a 404 is re-asked —
    // every other answer has already settled the question.
    while answers.len() < CONFIRM {
        if !answers.is_empty() {
            tokio::time::sleep(std::time::Duration::from_millis(400)).await;
        }
        let mut answer = None;
        for attempt in 0..2 {
            match client.get(&url).send().await {
                Ok(r) => {
                    answer = Some(r.status().as_u16());
                    break;
                }
                Err(_) if attempt == 0 => continue,
                Err(_) => break,
            }
        }
        answers.push(answer);
        if answer != Some(404) {
            break;
        }
    }
    verdict(&answers)
}

// ---- reachability ------------------------------------------------------------

/// Does this spelling resolve to a command? Asked of the built binary, because
/// that is the only thing that knows. clap prints `Usage: hanzo <spelling> …`
/// for a command it has and falls back to the ROOT usage for one it does not, so
/// the usage line is the answer and no help-page format is parsed.
fn resolves(hanzo: &std::path::Path, spelling: &str) -> bool {
    let out = Command::new(hanzo)
        .args(spelling.split_whitespace())
        .arg("--help")
        .env("NO_COLOR", "1")
        .env("TERM", "dumb")
        .output()
        .unwrap_or_else(|e| panic!("run {} {spelling} --help: {e}", hanzo.display()));
    let text = String::from_utf8_lossy(&out.stdout) + String::from_utf8_lossy(&out.stderr);
    let want = format!("Usage: hanzo {spelling}");
    text.lines().any(|l| l.strip_prefix(&want).is_some_and(|r| r.is_empty() || r.starts_with(' ')))
}

/// The spelling a curated entry says reaches this surface instead — the claim the
/// gate is able to falsify.
fn claim(c: &Curated) -> Option<String> {
    match c.instead {
        Instead::Under(parent) => Some(format!("{parent} {}", c.product)),
        Instead::Claimed(name) => Some(name.to_string()),
        Instead::Nothing => None,
    }
}

// ---- arguments ---------------------------------------------------------------

const USAGE: &str =
    "usage: driftgate [--lint] [--registry <url|path>] [--host <url>] [--spec <path>] [--hanzo <path>]";

struct Args {
    registry: String,
    host: String,
    spec: PathBuf,
    hanzo: PathBuf,
    /// Run ONLY the naming rules and exit. No network, no `hanzo` binary, no
    /// registry — just the document. This is the half of the gate that can be
    /// answered from a checkout alone, and `cargo test` runs it that way (see
    /// tests/spec_drift.rs) so a stutter is refused on every push rather than
    /// only when somebody runs the full gate against a live host.
    lint: bool,
}

fn args() -> Args {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    // Cargo puts every binary of a profile in ONE directory, so the `hanzo` built
    // beside this gate is the `hanzo` this gate is about.
    let sibling = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join(if cfg!(windows) { "hanzo.exe" } else { "hanzo" })))
        .unwrap_or_else(|| manifest.join("target/debug/hanzo"));
    let mut a = Args {
        registry: DEFAULT_REGISTRY.to_string(),
        host: DEFAULT_HOST.to_string(),
        spec: manifest.join("spec/cloud.json"),
        hanzo: sibling,
        lint: false,
    };
    // The flag is decided BEFORE its value is required, so an unknown flag says so
    // and `--help` is answered rather than told it needs a value.
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let mut i = 0;
    while i < argv.len() {
        let flag = argv[i].clone();
        let set: fn(&mut Args, String) = match flag.as_str() {
            "--registry" => |a, v| a.registry = v,
            "--host" => |a, v| a.host = v,
            "--spec" => |a, v| a.spec = PathBuf::from(v),
            "--hanzo" => |a, v| a.hanzo = PathBuf::from(v),
            "--lint" => {
                a.lint = true;
                i += 1;
                continue;
            }
            "-h" | "--help" => {
                println!("{USAGE}");
                std::process::exit(0)
            }
            other => panic!("{USAGE}\nunknown: {other}"),
        };
        set(&mut a, argv.get(i + 1).cloned().unwrap_or_else(|| panic!("{flag} needs a value\n{USAGE}")));
        i += 2;
    }
    a
}

/// Read the route table. Retried for the same reason a 404 is confirmed: one
/// dropped connection is not a fact about anything, and a gate that fails the
/// build over it is an intermittent red gate, which is how a gate dies.
async fn read(src: &str) -> Value {
    let body = if src.starts_with("http") {
        let mut last = String::new();
        let mut got = None;
        for attempt in 0..3 {
            if attempt > 0 {
                tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            }
            match reqwest::get(src).await.and_then(reqwest::Response::error_for_status) {
                Ok(r) => match r.text().await {
                    Ok(t) => {
                        got = Some(t);
                        break;
                    }
                    Err(e) => last = e.to_string(),
                },
                Err(e) => last = e.to_string(),
            }
        }
        let Some(body) = got else {
            // The same law the probes obey, at the top of the run: a gate that
            // cannot see must not pass. Said plainly, not as a backtrace.
            println!(
                "driftgate: BLIND — {src} did not answer, three times: {last}\n\
                 Nothing was checked. This is not \"no drift\" — it is \"I could not look\"."
            );
            std::process::exit(1);
        };
        body
    } else {
        std::fs::read_to_string(src).unwrap_or_else(|e| panic!("read {src}: {e}"))
    };
    serde_json::from_str(&body).unwrap_or_else(|e| panic!("{src} is not the JSON route table: {e}"))
}

// ---- the gate ----------------------------------------------------------------

#[tokio::main]
async fn main() {
    // `driftgate | head` should end like `ls | head` does, not with a Rust panic
    // about a broken pipe. Rust ignores SIGPIPE so a closed stdout surfaces as a
    // write error; this report is meant to be read through a pager.
    #[cfg(unix)]
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
    }

    let a = args();

    // NAMING FIRST, and it returns before anything reaches for a socket or for
    // the built `hanzo`: this half is a fact about the document and nothing else,
    // which is exactly what lets `cargo test` run it from a bare checkout.
    if a.lint {
        let spec: Value = serde_json::from_slice(
            &std::fs::read(&a.spec).unwrap_or_else(|e| panic!("read {}: {e}", a.spec.display())),
        )
        .unwrap_or_else(|e| panic!("{} is not a spec: {e}", a.spec.display()));
        let paths: Vec<String> = spec
            .get("paths")
            .and_then(Value::as_object)
            .map(|m| m.keys().cloned().collect())
            .unwrap_or_default();
        let (child, root, brand) = naming(&paths);
        for (title, rows) in [
            ("NAMING — a child repeats its parent. The path says the word twice", &child),
            ("NAMING — a collection root repeats its product. The namespace IS the collection", &root),
            ("NAMING — an upstream vendor's brand is on our surface. It outlives the vendor", &brand),
        ] {
            if !rows.is_empty() {
                println!("\n!! {title}");
                for r in rows.iter() {
                    println!("   {r}");
                }
            }
        }
        for (path, why) in NAMED {
            println!("   {path:<24}{why}");
        }
        if child.is_empty() && root.is_empty() && brand.is_empty() {
            println!("driftgate --lint: {} paths, no naming violation.", paths.len());
            std::process::exit(0);
        }
        std::process::exit(1);
    }
    assert!(a.hanzo.is_file(), "no `hanzo` at {} — build it first (`cargo build --bin hanzo`), \
         because whether a product is reachable is a fact about the built CLI", a.hanzo.display());

    let table = Table::read(&read(&a.registry).await);
    let spec: Value = serde_json::from_slice(
        &std::fs::read(&a.spec).unwrap_or_else(|e| panic!("read {}: {e}", a.spec.display())),
    )
    .unwrap_or_else(|e| panic!("{} is not a spec: {e}", a.spec.display()));
    let paths = spec.get("paths").and_then(Value::as_object).cloned().unwrap_or_else(Map::new);

    // A REDIRECT IS AN ANSWER, and following it asks a different question about a
    // different address. reqwest follows up to 10 by default, and that turned two
    // live routes into false 404s: `GET /v1/o11y/complete/google` answers 303 (an
    // OAuth callback), the gate followed it to `/v1/o11y/login?...` and recorded
    // that page's 404 against the callback's name — reporting cloud as
    // contradicting itself about a route that had just answered. Same class of
    // defect as reading a `403` as absence: the status this gate reasons about
    // must be the status of the address it asked about.
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(20))
        .redirect(reqwest::redirect::Policy::none())
        .user_agent("hanzo-driftgate")
        .build()
        .expect("http client");

    // ---- direction one: a route the server does not serve --------------------
    //
    // Asked of the SPEC, not of the folded tree: every command's route comes from
    // here, and a phantom the curation table happens to hide today ships the day
    // that entry is removed. The superset is the honest question.
    // Only a GET operation on a literal path can be put to the host, and both
    // halves of that are measured, not assumed:
    //
    //   * `{param}` — a 404 from `/v1/things/{id}` means "no route" or "no such
    //     id" and a gate must not read a sentence with two meanings.
    //   * not GET — cloud's router answers 404, not 405, to a method it does not
    //     have at a path it does: `POST /v1/admin/credits` is in the live table
    //     and a GET of it 404s. So a GET says nothing about a POST-only route,
    //     and the honest thing is to leave the table's word as the only word.
    //
    // The probe is a GET for the same reason it is only ever a GET: a gate that
    // DELETEs to find out whether something is there is not a gate.
    let (mut serves, mut refutes, mut undecidable) = (0usize, Vec::new(), 0usize);
    let mut ask: BTreeMap<String, bool> = BTreeMap::new();
    for (path, item) in &paths {
        let s = segs(path);
        if s.len() < 2 || s[0] != "v1" || is_param(s[1]) || path.contains('?') || path.contains('#') {
            continue;
        }
        let literal = !s.iter().any(|x| is_param(x));
        for m in item.as_object().into_iter().flatten().map(|(m, _)| m) {
            if !VERBS.contains(&m.to_ascii_lowercase().as_str()) {
                continue;
            }
            let m = m.to_ascii_uppercase();
            let askable = literal && m == "GET";
            match table.says(&m, &s) {
                // The table names this route exactly. It is still worth asking a
                // host that can be asked — the table is a projection of the
                // ROUTER, and a route can be registered with a dead mount behind
                // it, which the router cannot know and the table therefore cannot
                // say. `serves` counts the ones nobody can re-ask.
                Says::Serves if askable => {
                    ask.insert(path.clone(), true);
                }
                Says::Serves => serves += 1,
                Says::Refutes => refutes.push((m, path.clone())),
                // A door or a silence is not an answer; only the host has one.
                Says::Door | Says::Silent if askable => {
                    ask.entry(path.clone()).or_insert(false);
                }
                Says::Door | Says::Silent => undecidable += 1,
            }
        }
    }

    let targets: Vec<String> = ask.keys().cloned().collect();
    let mut answers: BTreeMap<String, Probe> = BTreeMap::new();
    for batch in targets.chunks(16) {
        let mut set = tokio::task::JoinSet::new();
        for p in batch {
            let (c, h, p) = (client.clone(), a.host.clone(), p.clone());
            set.spawn(async move {
                let v = probe(&c, &h, &p).await;
                (p, v)
            });
        }
        while let Some(r) = set.join_next().await {
            let (p, v) = r.expect("probe task");
            answers.insert(p, v);
        }
    }

    // The histogram is printed, not just totalled. Conflating 404 with 401/403 is
    // the one mistake this gate exists not to make, so every CI log carries the
    // split that proves it did not: a run whose "present" is all 401 has still
    // seen 401, and anyone reading the log can tell.
    let mut codes: BTreeMap<u16, usize> = BTreeMap::new();
    let (mut present, mut absent, mut contradicted, mut flapping, mut blind) =
        (0usize, Vec::new(), Vec::new(), Vec::new(), Vec::new());
    for (path, table_serves) in &ask {
        match answers.get(path).copied().unwrap_or(Probe::Blind) {
            Probe::Present(c) => {
                present += 1;
                *codes.entry(c).or_default() += 1;
            }
            Probe::Flapping(c) => {
                present += 1;
                *codes.entry(c).or_default() += 1;
                flapping.push(path.clone());
            }
            // WHOSE defect it is turns on who claimed the route. If the table
            // named it, cloud's own table and cloud's own server disagree and no
            // edit in this repo can settle that. If nothing named it, the CLI is
            // carrying a route the server denies, and that is ours.
            Probe::Absent if *table_serves => contradicted.push(path.clone()),
            Probe::Absent => absent.push(path.clone()),
            Probe::Blind => blind.push(path.clone()),
        }
    }
    let split = codes.iter().map(|(c, n)| format!("{c}×{n}")).collect::<Vec<_>>().join(" ");

    // ---- direction two: a served product no command reaches ------------------
    let mut universe = table.products();
    universe.extend(products(&spec));
    let (mut reachable, mut orphans, mut excused) = (0usize, Vec::new(), Vec::new());
    for p in &universe {
        if resolves(&a.hanzo, p) {
            reachable += 1;
        } else if let Some(c) = curation::curated(p) {
            excused.push(c);
        } else {
            orphans.push(p.clone());
        }
    }

    // An excuse is only an excuse while it is true. Every claim in the curation
    // table names a spelling; the gate runs all of them, applied or not, because
    // the reservation that goes stale FIRST is the one nothing is leaning on yet.
    let stale: Vec<(&str, String)> = curation::CURATED
        .iter()
        .filter_map(|c| claim(c).map(|s| (c.product, s)))
        .filter(|(_, s)| !resolves(&a.hanzo, s))
        .collect();

    // ---- the report ----------------------------------------------------------
    println!("driftgate — {} against {}", a.spec.display(), a.registry);
    println!("  the table settled     {serves:>5} served, {} refuted", refutes.len());
    println!(
        "  the host settled      {present:>5} present, {} absent, {} contradicted, {} unanswered",
        absent.len(),
        contradicted.len(),
        blind.len()
    );
    println!("                              answers: {split}   (a CONFIRMED 404 ⇒ absent; everything else ⇒ present)");
    println!("  nobody can settle     {undecidable:>5} (a `{{param}}`, or a verb a read-only probe cannot ask about)");
    println!("  products reachable    {reachable:>5} of {}", universe.len());
    println!("  declared exceptions   {:>5} applied of {} in src/curation.rs", excused.len(), curation::CURATED.len());
    if !flapping.is_empty() {
        println!("\n   {} route(s) answered 404 and then answered — present, but not reliably:", flapping.len());
        for p in &flapping {
            println!("   {p}");
        }
    }

    let mut fail = false;
    let mut bad = |title: &str, rows: Vec<String>| {
        if rows.is_empty() {
            return;
        }
        fail = true;
        println!("\n!! {title}");
        for r in &rows {
            println!("   {r}");
        }
    };

    bad(
        "PHANTOM — the route table owns this product and serves no such route",
        refutes.iter().map(|(m, p)| format!("{m:<7}{p}")).collect(),
    );
    bad(
        "PHANTOM — no document claims this route and the host answers 404, three times",
        absent.clone(),
    );
    bad(
        "BLIND — the host did not answer at all. A gate that cannot see must not pass",
        blind.clone(),
    );
    bad(
        "ORPHAN — served, and no command reaches it. Add the command, or declare it in src/curation.rs with a reason",
        orphans.clone(),
    );
    bad(
        "STALE EXCEPTION — the curation table sends people to a command that does not exist",
        stale.iter().map(|(p, s)| format!("{p:<16}claims `hanzo {s}`")).collect(),
    );

    // NAMING, over the same document the phantom check just read. Same gate, same
    // exit code, no network: a name is only wrong on the wire, so the emitted
    // document is the only place the rule can be put, and putting it anywhere
    // else would be a second authority about the same question.
    let (stutter_child, stutter_root, branded) = naming(&paths.keys().cloned().collect::<Vec<_>>());
    bad(
        "NAMING — a child repeats its parent. The path says the word twice",
        stutter_child,
    );
    bad(
        "NAMING — a collection root repeats its product. The namespace IS the collection",
        stutter_root,
    );
    bad(
        "NAMING — an upstream vendor's brand is on our surface. It outlives the vendor",
        branded,
    );
    // A named violation is still a violation; it is excused only because its fix
    // is in another repo, and it is printed every run so it cannot go quiet.
    if !NAMED.is_empty() {
        println!(
            "\n   {} naming violation(s) owned by another repo, each with the repo that fixes it:",
            NAMED.len()
        );
        for (p, why) in NAMED {
            println!("   {p:<24}{why}");
        }
    }

    // Cloud's table and cloud's server disagreeing is REAL and it is not ours: no
    // edit in this repo makes `GET /v1/billing/payment-config` answer, and a gate
    // that turns this build red for it is a gate people learn to switch off. So
    // it is a CEILING, not an equality — it may not grow in silence, and it is
    // allowed to fall the moment somebody redeploys, without turning a nightly
    // run red for the crime of production getting better.
    if contradicted.len() > CONTRADICTED {
        fail = true;
    }
    if !contradicted.is_empty() {
        println!(
            "\n{} CLOUD CONTRADICTS ITSELF — the live route table names these routes and the host \
             answers 404 ({} of at most {CONTRADICTED})",
            if contradicted.len() > CONTRADICTED { "!!" } else { "  " },
            contradicted.len()
        );
        for p in &contradicted {
            println!("   {p}");
        }
        println!(
            "   A route registered with a dead mount behind it: the router knows it, so the table\n   \
             it projects claims it, and the server still has nothing to run. The fix is in\n   \
             hanzoai/cloud's router, never a list here. If this grew, file it there; if it shrank,\n   \
             bring CONTRADICTED down to {} in src/bin/driftgate.rs.",
            contradicted.len()
        );
    }

    if excused.len() != EXCUSED {
        fail = true;
        let verb = if excused.len() > EXCUSED { "GREW" } else { "SHRANK" };
        println!(
            "\n!! DECLARED EXCEPTIONS {verb}: {} applied, EXCUSED says {EXCUSED}",
            excused.len()
        );
        for c in &excused {
            println!("   {:<16}{}", c.product, c.why);
        }
        println!(
            "   Set EXCUSED = {} in src/bin/driftgate.rs. It is pinned so the number cannot move\n   \
             without somebody deciding it should: an exception nobody counts is how 21 served\n   \
             /v1/deploy operations came to be reachable by nothing.",
            excused.len()
        );
    } else if !excused.is_empty() {
        println!("\n   {} declared exceptions, each with a written reason:", excused.len());
        for c in &excused {
            println!("   {:<16}{}", c.product, c.why);
        }
    }

    if fail {
        println!("\ndriftgate: the CLI surface and the live route table disagree.");
        std::process::exit(1);
    }
    println!("\ndriftgate: no drift.");
}

/// The gate's two predicates, pinned. Both are pure — that is why they were
/// separated from the network and from the report — and both encode a rule this
/// gate exists to keep, which is a rule a paragraph cannot enforce.
#[cfg(test)]
mod tests {
    use super::*;

    /// THE rule, and the reason this gate is worth having. `401`/`403` say the
    /// route is THERE and wants a caller; `405` says the path is routed and the
    /// verb is not; only `404` says nothing is at that address. An earlier hand
    /// analysis of this exact surface conflated them and reported three production
    /// breaks that were not breaks — a red build that teaches people the build
    /// lies. Anything that ever makes this test fail has reintroduced that.
    #[test]
    fn an_auth_refusal_is_a_route_that_exists_and_only_a_confirmed_404_is_absent() {
        // 303 is in this list because it was MEASURED: `GET /v1/o11y/complete/google`
        // answers it, and the transport used to follow the redirect and report the
        // 404 of wherever it landed — a present route recorded as absent.
        for code in [200, 201, 204, 302, 303, 400, 401, 403, 405, 409, 429, 500, 502, 503] {
            assert!(
                matches!(verdict(&[Some(code)]), Probe::Present(c) if c == code),
                "{code} is an answer FROM a route — a router with nothing at that address \
                 cannot produce it, so it is never absence"
            );
        }
        assert!(matches!(verdict(&[Some(404); CONFIRM]), Probe::Absent));
    }

    /// THE NAMING LINT BITES. A gate is worth exactly what it refuses, so the
    /// rules are asserted against paths that BREAK them — the stutters this
    /// rename just removed, put back one at a time. A lint that only ever sees a
    /// clean document has never been shown to fail.
    #[test]
    fn naming_refuses_a_stutter_a_repeated_product_and_a_vendor_brand() {
        let (a, _, _) = naming(&["/v1/index/indexes".into()]);
        assert_eq!(a.len(), 1, "index/indexes is a child repeating its parent");

        let (a, _, _) = naming(&["/v1/dataroom/datarooms/{id}/documents".into()]);
        assert_eq!(a.len(), 1, "a param between them does not separate the words");

        // Both rules see a collection root that repeats its product, and report it
        // with different reasons. That overlap is the design.
        let (a, b, _) = naming(&["/v1/domain/domains".into()]);
        assert_eq!((a.len(), b.len()), (1, 1), "one path, two reasons, two fixes");

        // A param separates the words either side of it: "the domains OF this
        // domain" is a legitimate sub-collection, and `domains` is not the
        // collection root. Neither rule fires.
        let (a, b, _) = naming(&["/v1/domain/{id}/domains".into()]);
        assert!(a.is_empty() && b.is_empty(), "{a:?} {b:?}");

        // (b) judges the COLLECTION ROOT and nothing deeper. These three are real
        // paths where the product word repeats and MEANS something — a verb, a
        // discriminator, an object. A lint that condemns them gets switched off.
        for innocent in [
            "/v1/search/indexes/{uid}/search",
            "/v1/dataroom/analytics/dataroom/{dataroomId}",
            "/v1/books/scan/book",
        ] {
            let (_, b, _) = naming(&[innocent.into()]);
            assert!(b.is_empty(), "{innocent} is not a collection root repeating its product");
        }

        let (_, _, c) = naming(&["/v1/search/meilisearch/health".into()]);
        assert_eq!(c.len(), 1, "an upstream vendor may not be on our surface");

        // The upstream CRATE name is a dependency, not a brand leak: the rule is
        // about the surface a customer types, and `milli` never appears in it.
        let (_, _, c) = naming(&["/v1/search/milli".into()]);
        assert!(c.is_empty(), "milli is a real dependency, not a brand on our surface");
    }

    /// And it PASSES the names the rename produced — otherwise the test above
    /// would be satisfied by a lint that fails on everything.
    #[test]
    fn naming_admits_the_names_this_rename_produced() {
        let clean = [
            "/v1/search/indexes/{uid}/documents",
            "/v1/ai/evals/datasets/{name}/items",
            "/v1/zt/networks/routers",
            "/v1/dataroom/{id}/documents",
            "/v1/domain",
            "/v1/payments/{id}",
            "/v1/integrations/cloudflare/zones/{zone}/purge",
            "/v1/visor/balancers/{id}",
            "/v1/admin/search/indexes",
        ]
        .map(String::from);
        let (a, b, c) = naming(&clean);
        assert!(a.is_empty() && b.is_empty() && c.is_empty(), "{a:?} {b:?} {c:?}");
    }

    /// The excuse is keyed by the EXACT path, so it cannot quietly cover a
    /// sibling that breaks the same rule for a reason nobody wrote down.
    #[test]
    fn a_named_exception_excuses_only_itself() {
        let (a, ..) = naming(&[NAMED[0].0.into()]);
        assert!(a.is_empty(), "the declared path is excused");
        let (a, ..) = naming(&["/v1/traces/trace/{id}/trace".into()]);
        assert_eq!(a.len(), 1, "a different path is not");
    }

    /// A SINGLE 404 IS NOT EVIDENCE — the same mistake one layer down. Fourteen
    /// `/v1/pricing` paths answered 404 to one concurrent sweep and 200 to every
    /// serial re-ask a minute later; condemning on the first answer would have
    /// reported fourteen more breaks that were not breaks.
    #[test]
    fn a_404_that_does_not_hold_is_flapping_and_never_drift() {
        assert!(matches!(verdict(&[Some(404), Some(200)]), Probe::Flapping(200)));
        assert!(matches!(verdict(&[Some(404), Some(404), Some(403)]), Probe::Flapping(403)));
        assert!(
            matches!(verdict(&[Some(404), Some(404)]), Probe::Blind),
            "two 404s are not the {CONFIRM} that confirm one"
        );
    }

    /// No answer is not "no drift" — it is "I could not look". A gate that cannot
    /// see must not pass, so silence is its own verdict and never absence.
    #[test]
    fn silence_is_never_read_as_an_answer() {
        assert!(matches!(verdict(&[None]), Probe::Blind));
        assert!(matches!(verdict(&[Some(404), None]), Probe::Blind));
        assert!(matches!(verdict(&[]), Probe::Blind));
    }

    /// What the table may and may not settle on its own. It is complete for a
    /// product it OWNS, so a missing route there is refuted without asking anyone;
    /// a `*` door and a silence are NOT answers and carry a probe instead. The bare
    /// `/v1/*` fallthrough is evidence of nothing — counting it would make every
    /// conceivable path "served" and this gate a no-op.
    #[test]
    fn the_table_refutes_only_inside_a_product_it_owns_and_a_door_is_not_an_answer() {
        let t = Table::read(&serde_json::json!({"paths": {
            "/v1/billing/usage": {"get": {}},
            "/v1/iam/{wildcard1}": {"get": {}},
            "/v1/{wildcard1}": {"get": {}}
        }}));
        assert!(matches!(t.says("GET", &segs("/v1/billing/usage")), Says::Serves));
        assert!(matches!(t.says("GET", &segs("/v1/billing/no-such-route")), Says::Refutes));
        assert!(matches!(t.says("GET", &segs("/v1/iam/anything/at/all")), Says::Door));
        assert!(matches!(t.says("GET", &segs("/v1/nosuchproduct/x")), Says::Silent));
        assert!(matches!(t.says("POST", &segs("/v1/billing/usage")), Says::Refutes), "a verb is part of the route");
        assert_eq!(t.products(), ["billing", "iam"].iter().map(ToString::to_string).collect());
    }
}
