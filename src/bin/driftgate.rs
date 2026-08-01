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
//!   * it is silent about the product        → the router is not the authority
//!     (the inference surface is answered at the edge). Ask.
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
//! Usage: `driftgate [--registry <url|path>] [--host <url>] [--spec <path>]
//!                   [--hanzo <path>]`

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
const CONTRADICTED: usize = 3;

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

/// Every product either document names — the universe the ORPHAN direction asks
/// about. The table's own products plus the spec's, because a product answered
/// at the edge (`/v1/models`) is in no route table and is served all the same.
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

/// Ask the host whether anything is routed at this path. Read-only by
/// construction: a `GET`, and a `405` is a PRESENT answer, not a failure.
///
/// A SINGLE 404 IS NOT EVIDENCE. Measured on this surface: fourteen `/v1/pricing`
/// paths answered 404 to one concurrent sweep and 200 to every serial re-ask a
/// minute later. A gate that condemned on the first answer would have reported
/// fourteen production breaks that were not breaks — the same class of mistake as
/// reading 403 as absent, one layer down. So a 404 is CONFIRMED before it counts:
/// re-asked serially, and only a 404 that holds three times is an answer.
async fn probe(client: &reqwest::Client, host: &str, path: &str) -> Probe {
    let url = format!("{}{}", host.trim_end_matches('/'), path);
    let mut seen404 = 0;
    // Three rounds; each tolerates one transport failure, because a dropped
    // connection is not evidence about a route. Silence all the way down is
    // evidence that the gate cannot see, which is a different and reportable
    // thing from evidence that the route is gone.
    for round in 0..3 {
        if round > 0 {
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
        match answer {
            Some(404) => seen404 += 1,
            Some(code) if seen404 > 0 => return Probe::Flapping(code),
            Some(code) => return Probe::Present(code),
            None => return Probe::Blind,
        }
    }
    Probe::Absent
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

struct Args {
    registry: String,
    host: String,
    spec: PathBuf,
    hanzo: PathBuf,
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
    };
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let mut i = 0;
    while i < argv.len() {
        let val = argv.get(i + 1).cloned().unwrap_or_else(|| panic!("{} needs a value", argv[i]));
        match argv[i].as_str() {
            "--registry" => a.registry = val,
            "--host" => a.host = val,
            "--spec" => a.spec = PathBuf::from(val),
            "--hanzo" => a.hanzo = PathBuf::from(val),
            other => panic!(
                "usage: driftgate [--registry <url|path>] [--host <url>] [--spec <path>] [--hanzo <path>]\nunknown: {other}"
            ),
        }
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
    assert!(a.hanzo.is_file(), "no `hanzo` at {} — build it first (`cargo build --bin hanzo`), \
         because whether a product is reachable is a fact about the built CLI", a.hanzo.display());

    let table = Table::read(&read(&a.registry).await);
    let spec: Value = serde_json::from_slice(
        &std::fs::read(&a.spec).unwrap_or_else(|e| panic!("read {}: {e}", a.spec.display())),
    )
    .unwrap_or_else(|e| panic!("{} is not a spec: {e}", a.spec.display()));
    let paths = spec.get("paths").and_then(Value::as_object).cloned().unwrap_or_else(Map::new);

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(20))
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
