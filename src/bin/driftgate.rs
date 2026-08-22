//! `driftgate` — the shipped command surface and the live route table, held
//! against each other in BOTH directions, HERMETICALLY, against evidence that is
//! checked in.
//!
//! # WHY THIS GATE IS NOT THE ONE IT REPLACED
//!
//! The gate before it compared `generated.rs` against `spec/cloud.json` — two
//! DERIVED artifacts, which agree with each other exactly as long as one derives
//! from the other, whether or not either is still true of anything. Both can be
//! stale together and the gate stays green. It could not have caught a single
//! defect this repo actually shipped.
//!
//! What decides drift is the SERVER, and the server can only be asked over the
//! network — which a gate must not need, because a gate that needs the network in
//! CI gets switched off, and a switched-off gate is worse than none. So the two
//! are decomplected:
//!
//!   * `--refresh` ASKS (network). It writes `spec/live.json`: the live table's
//!     word about every coordinate, the host's raw answers, and the control
//!     answers that make those answers mean something. Evidence, not verdicts.
//!   * the default RULES (no network). It reads that evidence, re-derives every
//!     verdict from it, and refuses a tree the evidence contradicts. This is what
//!     `cargo test` and `make verify` run, and it needs nothing but a checkout.
//!
//! One binary, because a gate that runs a different derivation than the writer
//! tests something nobody ships. `--refresh --check` re-asks and refuses to write
//! when a verdict moved — that is the nightly, and it is how production moving
//! under a committed tree becomes a red build rather than a user's bug report.
//!
//! # 404 IS NOT 403, AND NEITHER MEANS ANYTHING WITHOUT A CONTROL
//!
//! `401`/`403` say the route is THERE and wants a caller who is signed in; `405`
//! says the path is routed and the verb is not; only `404` says there is nothing
//! at that address. An earlier hand analysis of this exact surface conflated them
//! and reported three production breaks that were not breaks.
//!
//! THAT RULE IS NOT ENOUGH, and believing it was cost this project a whole
//! parallel document. A relay door or an auth wall answers IDENTICALLY for a real
//! path and an invented one: `/v1/bot/*` 403s everything, so 32 authored `/v1/bot`
//! operations "verified" against a door that cannot tell them from nonsense.
//! 66 of the 71 operations a second authority contributed were confirmed exactly
//! that way. A door is not a list, and a 403 with no control is not evidence.
//!
//! So EVERY probe carries a CONTROL — a nonsense sibling under the same prefix,
//! asked the same way — and the control is what makes the answer mean something:
//!
//! | control | real     | verdict        | why                                        |
//! |---------|----------|----------------|--------------------------------------------|
//! | `404`   | `404`    | ABSENT         | the prefix discriminates, and denies this   |
//! | `404`   | anything | PRESENT        | the prefix discriminates, and answers this  |
//! | not 404 | anything | UNFALSIFIABLE  | the prefix answers for nonsense too         |
//! | blind   | —        | BLIND          | nothing was learned                         |
//!
//! UNFALSIFIABLE is a first-class verdict and it is COUNTED, never quietly read
//! as presence. A stated blind spot is worth more than a confident guess, and the
//! count is pinned so the blind spot cannot grow in silence.
//!
//! And a SINGLE 404 is not a 404 either: fourteen `/v1/pricing` paths answered
//! `404` to one concurrent sweep and `200` to every serial re-ask a minute later.
//! A `404` is confirmed [`CONFIRM`] times, serially, before it counts; one that
//! does not hold is FLAPPING — present, reported, never drift. BLIND fails: the
//! one thing a gate may never do is report "no drift" when it means "I could not
//! look".
//!
//! # WHO CAN BE ASKED
//!
//! Only a `GET` on a LITERAL path, and both halves were measured rather than
//! assumed: a `{param}` makes `404` mean "no route" OR "no such id", and cloud's
//! router answers `404`, not `405`, to a verb it lacks at a path it has (`POST
//! /v1/admin/credits` is in the live table and a `GET` of it 404s). Everything
//! else is settled by the live table's own word, or is UNFALSIFIABLE. The probe is
//! only ever a `GET`: a gate that DELETEs to find out whether something is there
//! is not a gate.
//!
//! # WHICH SIDE IS WRONG
//!
//! Both failures are real and they have different owners, so they are reported
//! apart and named apart:
//!
//!   * PHANTOM — a HAND-WRITTEN command sends a route no document declares, under
//!     a product cloud owns. OURS, and a hard failure. Those routes are literals
//!     in the source and were read by nothing at all until 1.9.46 — see [`sent`].
//!     A GENERATED coordinate can never be one: it exists because the document
//!     declares it, and the chain digests refuse a hand-edited `generated.rs`.
//!   * AHEAD — the host denies it, the live table does not name it, and the pinned
//!     document does. The deploy has not landed. Not ours, not cloud's table's
//!     either: a wait, pinned so it cannot lengthen unread.
//!   * CONTRADICTED — the host denies it and cloud's OWN live table claims it.
//!     Cloud's table and cloud's server disagree; no edit in this repo settles
//!     that, so it is a CEILING that may not grow in silence and is free to fall
//!     the moment somebody redeploys.
//!   * ORPHAN — a served product no command reaches. OURS: add the command, or
//!     declare it in `src/curation.rs` with a reason a person can act on.
//!
//! Every row names the COMMAND, not only the coordinate, because the command is
//! what a person types and what a fix has to touch.
//!
//! # THE CHAIN, AND WHY A HAND EDIT ANYWHERE IN IT IS REFUSED
//!
//! ```text
//!   hanzoai/cloud@<tag> openapi.yaml   ── .spec-lock  (repo, path, ref, sha256)
//!        │ genspec
//!   spec/cloud.json                    ── spec/live.json's `spec_sha256`
//!        │ genproduct
//!   src/commands/product/generated.rs  ── genproduct --check
//! ```
//!
//! Each link is pinned by a digest whose writer is a generator, so an artifact
//! edited by hand stops matching the thing it is supposed to be derived from.
//! `spec/live.json` carries the digest of the `spec/cloud.json` it is evidence
//! ABOUT, and its own digest over its own payload. Every coordinate must have
//! evidence: a coordinate nobody has ever asked about is unproven, and unproven
//! fails.
//!
//! # THE OTHER DIRECTION ASKS THE BINARY
//!
//! `hanzo <product> --help`, not a second derivation of the data `genproduct`
//! folded. Whether a person can reach a product is a fact about the BUILT CLI: a
//! generated product that collides with a local command, or a relocation that
//! quietly stopped relocating, is invisible to any re-derivation and obvious to
//! one exec. A served product with no command is drift unless `src/curation.rs`
//! says otherwise, every excuse naming a spelling is RUN, and the applied count is
//! pinned — an exception nobody counts is how 21 served `/v1/deploy` operations
//! came to be reachable by nothing.
//!
//! Usage: `driftgate [--refresh [--check]] [--registry <url|path>] [--host <url>]
//!                   [--spec <path>] [--live <path>] [--hanzo <path>]`

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Command;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::Digest;

#[path = "../curation.rs"]
mod curation;
use curation::{Curated, Instead};

// The generated command table, by VALUE — the same static this binary's sibling
// `hanzo` builds its parser from, not a re-parse of the file it lives in. A gate
// that re-reads a generated artifact with a regex is a second reading of one
// value, which is the disease this pipeline exists to end, one layer down.
// The gate reads (product, nodes, verb, method, path) — enough to name a command;
// `hanzo` reads the rest of every row. Two readers of one value legitimately use
// different halves of it, which is what `dead_code` cannot know.
#[path = "../commands/product/op.rs"]
#[allow(dead_code)]
mod op;
#[allow(unused_imports)] // `Field` and `Ty` are used BY `generated` through `super::`.
use op::{Field, Op, Ty};
#[path = "../commands/product/generated.rs"]
#[allow(dead_code)]
mod generated;
use generated::OPS;

const VERBS: [&str; 5] = ["get", "post", "put", "patch", "delete"];
const DEFAULT_REGISTRY: &str = "https://api.hanzo.ai/v1/openapi.json";
const DEFAULT_HOST: &str = "https://api.hanzo.ai";

/// The nonsense segment a CONTROL is asked at. It has to be shaped like a route
/// somebody could have written — a segment of ASCII and hyphens — so that nothing
/// but the router's own table can distinguish it from a real one.
const NONSENSE: &str = "zzq9-no-such-route";

/// How many serial `404`s confirm one.
const CONFIRM: usize = 3;

/// How many served products the curation table had to excuse the last time
/// anyone looked. Pinned, and asserted EXACTLY: the number is a claim about the
/// fleet, and a claim that changed must be restated by a person in the same
/// commit that changes it. Up means a new gap was excused without a decision;
/// down means one was closed and the ceiling should come with it.
const EXCUSED: usize = 0;

/// Routes cloud's own live table names and cloud's own host answers 404 to — a
/// route registered with a dead mount behind it. A CEILING, not an equality, and
/// the asymmetry is deliberate: see where it is applied.
///
/// The six are ONE event: `/v1/ai/{applications,permissions,sessions,
/// sessions/duplicated,users,users/table-infos}`. cloud renamed those resources
/// (applications→deployments, sessions→signin-sessions, users→usages, permissions
/// back to IAM) and the deployed binary's ROUTER serves the new nouns while the
/// DOCUMENT that same binary publishes still advertises the old ones. Measured,
/// with controls: `GET /v1/ai/deployments` 401 (routed, wants a caller) and
/// `GET /v1/ai/applications` 404 against a `/v1/ai/<nonsense>` control that also
/// 404s — so the prefix discriminates and the denial is real.
///
/// The cause is a second authority INSIDE cloud, which is the same disease this
/// pipeline cured on its own side: `apps/ai` projects from the committed
/// `plugin/ai/openapi.json` subset instead of the mounted plugin's live registry,
/// so its published names lag its routes. Nothing in this repo can settle it — the
/// fix is in hanzoai/cloud (task #146's seam), and the number is here so it cannot
/// be settled by forgetting.
///
/// 6 -> 0: cloud deleted the door that threw those schemas away, so `apps/ai`
/// projects its own live registry and the six stale nouns left the document with
/// it. The class is empty and the ceiling is on the floor, which is where a
/// ceiling belongs when nobody is under it.
///
/// 0 -> 16, and every one of them is `GET /v1/billing/*`: accounts, alerts,
/// alerts/authorize, credit-balance, credits, crypto/options, invoices, methods,
/// payouts, plans, portal/methods, settings, subscriptions, tier, transactions,
/// wire. Measured with a control: each answers 404 three times while
/// `/v1/billing/balance` and `/v1/billing/usage` answer 401 under the SAME
/// prefix, so the prefix discriminates and the denial belongs to the route
/// rather than to a wall in front of it.
///
/// 16 -> 5 on re-pinning onto cloud's main: billing answers, and the whole class
/// is `/v1/o11y/{healthz,livez,readyz,complete/google,complete/oidc}`. The ceiling
/// comes down with it, because a ceiling with eleven units of slack is not a
/// ceiling — it is room for eleven new dead mounts to ship as commands that 404.
/// This is the one census the gate still REFUSES on rather than merely recording,
/// and the line is user-visible breakage: an untyped write still works through
/// `--data`, while a command addressing a dead mount cannot work at all.
const CONTRADICTED: usize = 5;

/// Coordinates the PINNED document declares, the live table does not name, and
/// the host denies. That is the document running AHEAD of the deploy: cloud's
/// source says the route exists and the binary answering api.hanzo.ai was built
/// before it did. It falls on its own at the next deploy.
///
/// A CEILING, like [`CONTRADICTED`], and for the same reason — no edit in this
/// repo makes `GET /v1/commerce/org` answer, and a gate that reddens for it is a
/// gate people switch off. Today it is 3 (`GET /v1/commerce/org`,
/// `POST /v1/iam/link`, `PUT /v1/iam/password`), all three among the eleven
/// operations this re-pin added.
///
/// IT USED TO BE CALLED A PHANTOM, and that was false by construction. A phantom
/// is a command addressing a route NO DOCUMENT claims; every generated coordinate
/// is claimed by the document it was generated from, and the chain digests refuse
/// a hand-edited `generated.rs`, so this bucket could never hold one. Reading
/// "the deploy has not landed" as "this repo invented a route" points a hard
/// failure at the wrong owner and buries the real phantom class — the
/// hand-written literals, which keep the name and keep failing hard.
const AHEAD: usize = 3;

/// Coordinates whose evidence cannot decide anything, because the prefix that
/// answers for them answers the same way for a route nobody wrote — a relay door
/// (`/v1/bot/*` 403s everything) or an auth wall that refuses before it routes.
///
/// A CEILING, and the most important number in this file. It is exactly the
/// surface on which a second authority could once say anything it liked and call
/// it verified: 66 of the 71 operations the deleted master contributed were
/// "confirmed" by a door that cannot tell a real path from an invented one. The
/// count may not grow in silence — a growing blind spot is a growing licence to
/// guess — and it falls on its own as cloud types those relays into real
/// operations.
///
/// 177 -> 90 on the evidence this tree carries. It fell exactly as described,
/// and the ceiling stayed where it was: 87 coordinates the gate can now decide
/// were still budgeted as undecidable, which is 87 routes' worth of licence to
/// guess held open by a number nobody brought down. A ceiling pinned above
/// reality is the thing this comment says it exists to prevent.
///
/// 90 -> 92, and the two are production's, not this tree's: the live table stopped
/// naming `/v1/agent/*` at all (four routes, `serves` -> `silent` between two
/// captures). Two of the four are askable GETs and were decided by the host; the
/// other two — `POST /v1/agent` and `GET /v1/agent/conversations/{id}` — can be
/// asked of nobody, so the table's silence is the whole evidence. That is the
/// `agent` vs `agents` split showing up in the route table: one noun, two
/// products, and the fix is a route move in hanzoai/agent.
///
/// 92 -> 115, and production owns this one as well: `/v1/commerce` became a
/// door. It answers `{"error":"commerce unavailable","code":503}` for every path
/// beneath it, an invented one included, while `/v1/admin/<nonsense>` still 404s
/// — so the control discriminates, and commerce's answer is not about any route.
/// 123 coordinates sit under that prefix. `GET /v1/commerce/org` is among them,
/// which is the whole cost: the one coordinate the previous capture could still
/// call CONTRADICTED is now undecidable, so a blind spot has swallowed evidence
/// the gate used to hold. It falls when the plugin is mounted again.
const UNFALSIFIABLE: usize = 115;

// ---- the live route table ----------------------------------------------------

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
/// A coordinate: the METHOD and the path template, as one key.
fn coord(method: &str, path: &str) -> String {
    format!("{method} {path}")
}
/// The prefix a CONTROL is asked under — the coordinate's own parent. Asking
/// `/v1/o11y/<nonsense>` is what makes a `404` at `/v1/o11y/services` mean the
/// router denies THAT ROUTE rather than "this whole prefix answers 404 to
/// everything" (or, the other way round, that it answers 403 to everything).
fn parent(path: &str) -> String {
    let s = segs(path);
    format!("/{}", s[..s.len() - 1].join("/"))
}
fn control_of(path: &str) -> String {
    format!("{}/{NONSENSE}", parent(path).trim_end_matches('/'))
}

/// What the live route table knows: the patterns it serves per method, and the
/// products it is the authority over.
struct Table {
    routes: BTreeMap<String, Vec<Vec<String>>>,
    owned: BTreeSet<String>,
}

/// The table's answer about one operation, recorded in the capture so the
/// hermetic run can reason with it. `Door` and `Silent` are not answers — they are
/// the table saying it cannot answer.
#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Debug)]
#[serde(rename_all = "lowercase")]
enum Says {
    /// The table names this exact route.
    Serves,
    /// The table OWNS this product and names no such route — decided without
    /// asking anyone, because the table is complete for a product it serves.
    Refutes,
    /// Only a `/v1/<product>/*` catch-all matches. A door says something is
    /// mounted behind it, never what.
    Door,
    /// The table has never heard of this product.
    Silent,
}

impl Says {
    /// Does the LIVE table claim to answer here? This is the question that decides
    /// WHOSE defect an absence is, and it is the only thing `Door` is good for: a
    /// door claims the subtree relays, so a denial behind one is cloud's table
    /// disagreeing with cloud's server, not a route this repo invented.
    fn claims(self) -> bool {
        matches!(self, Says::Serves | Says::Door)
    }
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
}

/// Every product a document names — the universe the ORPHAN direction asks about,
/// taken from BOTH readings, because they are one document at two commits: a
/// product cloud typed after the pinned release is in the live table and not the
/// spec, and one retired since is in the spec and not the table.
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

/// Every coordinate a document carries, and whether a host can be asked about it.
/// Only a `GET` on a LITERAL path can: see the module docs for why each half of
/// that is a measurement rather than a convention.
fn coordinates(doc: &Value) -> Vec<(String, String, bool)> {
    let mut out = Vec::new();
    for (path, item) in doc.get("paths").and_then(Value::as_object).into_iter().flatten() {
        let s = segs(path);
        if s.len() < 2 || s[0] != "v1" || is_param(s[1]) || path.contains('?') || path.contains('#') {
            continue;
        }
        let literal = !s.iter().any(|x| is_param(x));
        for m in item.as_object().into_iter().flatten().map(|(m, _)| m) {
            if VERBS.contains(&m.to_ascii_lowercase().as_str()) {
                let m = m.to_ascii_uppercase();
                let askable = literal && m == "GET";
                out.push((m, path.clone(), askable));
            }
        }
    }
    out
}

/// Does the document declare this path? Segment by segment, so a `{param}` in
/// the source matches the document's `{param}` whatever either chose to call it
/// — the router's names are its own.
fn declares(doc: &Value, path: &str) -> bool {
    let want = segs(path);
    doc.get("paths")
        .and_then(Value::as_object)
        .into_iter()
        .flatten()
        .any(|(k, _)| Table::matches(&segs(k).iter().map(|s| (*s).to_string()).collect::<Vec<_>>(), &want))
}

/// Every `/v1` path the HAND-WRITTEN commands send, read out of the source that
/// sends them, keyed to the file it was read from.
///
/// The gate's notion of "what this repo can call" was `generated::OPS` and
/// nothing else, so a route only a local command sends was ruled on by no one.
/// `hanzo billing deposit` posted to `/v1/billing/deposit` for as long as it
/// existed — a route hanzoai/cloud has never served, in a product whose whole
/// route list the document carries — and the gate that exists to make a phantom
/// impossible could not see it, because the phantom was not in the generated
/// table.
///
/// The DOCUMENT is authority only over the products it owns, and that is enough
/// to decide this without an exception list: `hanzo fabric` talks to a hanzo
/// NODE, whose `/v1/node/cluster/*` routes cloud does not own and does not
/// publish, so they are not cloud's to refute and are left alone. A literal
/// under a product cloud DOES own is a claim about cloud's surface, and the
/// document is the whole of that surface.
///
/// It reads paths, not coordinates: a literal is a `&str`, and the method sits
/// at the call site. That is the granularity a lexical read can honestly reach,
/// and it is the granularity the defect lives at.
fn sent(src: &Path) -> BTreeMap<String, String> {
    fn walk(dir: &Path, out: &mut BTreeMap<String, String>) {
        let mut entries: Vec<PathBuf> =
            std::fs::read_dir(dir).unwrap_or_else(|e| panic!("read {}: {e}", dir.display()))
                .map(|e| e.expect("dir entry").path())
                .collect();
        entries.sort();
        for p in entries {
            let name = p.file_name().unwrap_or_default().to_string_lossy().to_string();
            if p.is_dir() {
                // `bin/` is the maintainer tools, not the shipped binary.
                if name != "bin" {
                    walk(&p, out);
                }
                continue;
            }
            // The generated tree is ruled on above, coordinate by coordinate,
            // and test code is not wire — a mock serves the routes it invents
            // and an assertion names paths on purpose.
            if !name.ends_with(".rs") || name == "generated.rs" || name.contains("test") {
                continue;
            }
            let text = std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()));
            let text = &text[..text.find("cfg(test)").unwrap_or(text.len())];
            let file = p.display().to_string();
            let mut rest = text;
            while let Some(i) = rest.find("\"/v1/") {
                rest = &rest[i + 1..];
                let Some(end) = rest.find('"') else { break };
                let (lit, tail) = rest.split_at(end);
                rest = tail;
                // A path is one token: a literal carrying spaces is prose that
                // happens to quote a route, and a `?` begins a query string,
                // which is not part of the address.
                let lit = lit.split('?').next().unwrap_or(lit).trim_end_matches('/');
                if lit.contains(' ') || segs(lit).len() < 2 {
                    continue;
                }
                out.entry(lit.to_string()).or_insert_with(|| file.clone());
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(src, &mut out);
    out
}

// ---- the evidence ------------------------------------------------------------

/// `spec/live.json` — what the live server said, checked in, so the gate that
/// rules on it needs no network. EVIDENCE, never verdicts: the raw answer
/// sequences are stored and every verdict is re-derived from them by [`verdict`]
/// and [`settle`], which are pure and are pinned by the tests at the foot of this
/// file. A capture holding conclusions instead of answers could not be re-judged
/// when the rule is corrected — and the rule has been corrected twice.
#[derive(Serialize, Deserialize)]
struct Capture {
    evidence: Evidence,
    /// sha256 of `evidence`, canonically encoded — this file's own integrity.
    digest: String,
}

#[derive(Serialize, Deserialize)]
struct Evidence {
    /// Which release, which host, and — the load-bearing one — the digest of the
    /// `spec/cloud.json` this evidence was taken ABOUT. A capture that does not
    /// name the artifact it describes cannot catch a hand edit to it.
    source: Source,
    /// The products the LIVE table serves. Half of the ORPHAN universe, and the
    /// half a committed spec cannot know: a product cloud started serving after
    /// the pinned release shows up here and nowhere else.
    products: BTreeSet<String>,
    /// Per coordinate, the live table's own word.
    table: BTreeMap<String, Says>,
    /// Per literal path, the host's answers to a `GET`. `null` is a transport
    /// failure, and it is recorded rather than dropped.
    probes: BTreeMap<String, Vec<Option<u16>>>,
    /// Per prefix, the host's answers to a `GET` of a route NOBODY WROTE. Keyed by
    /// prefix and not by path, because "does this prefix discriminate?" is one
    /// question per prefix and asking it 744 times would be 744 requests proving
    /// the same thing.
    controls: BTreeMap<String, Vec<Option<u16>>>,
}

#[derive(Serialize, Deserialize, PartialEq)]
struct Source {
    /// The hanzoai/cloud release tag `.spec-lock` pins. Evidence taken about a
    /// different release is evidence about a different surface.
    r#ref: String,
    host: String,
    registry: String,
    spec_sha256: String,
}

fn sha256(bytes: &[u8]) -> String {
    format!("{:x}", sha2::Sha256::digest(bytes))
}

impl Capture {
    fn seal(evidence: Evidence) -> Self {
        let digest = sha256(&serde_json::to_vec(&evidence).expect("encode evidence"));
        Capture { evidence, digest }
    }
    /// Was this file written by the refresh, or by a person? Same trick as every
    /// other link in the chain: the digest's only writer is the generator.
    fn sealed(&self) -> bool {
        self.digest == sha256(&serde_json::to_vec(&self.evidence).expect("encode evidence"))
    }
}

// ---- the rules, which are pure -----------------------------------------------

#[derive(Clone, Copy, PartialEq, Debug)]
enum Probe {
    /// [`CONFIRM`] `404`s in a row. Nothing is routed here.
    Absent,
    Answered(u16),
    /// A `404` that did not hold up. The route exists — a router with no such
    /// route cannot produce a `200` — but it is intermittently answering as if it
    /// did not, which is a production symptom worth naming and NOT drift.
    Flapping(u16),
    Blind,
}

/// A `404` is only a `404` once it has held [`CONFIRM`] times, serially. Measured
/// on this surface: fourteen `/v1/pricing` paths answered `404` to one concurrent
/// sweep and `200` to every serial re-ask a minute later, so condemning on the
/// first answer would have reported fourteen breaks that were not breaks. Silence
/// is [`Probe::Blind`] and never absence: no answer is not "no drift", it is "I
/// could not look".
fn verdict(answers: &[Option<u16>]) -> Probe {
    let mut seen404 = 0;
    for a in answers {
        match *a {
            Some(404) => seen404 += 1,
            Some(code) if seen404 > 0 => return Probe::Flapping(code),
            Some(code) => return Probe::Answered(code),
            None => return Probe::Blind,
        }
    }
    if seen404 >= CONFIRM {
        Probe::Absent
    } else {
        Probe::Blind
    }
}

/// What one coordinate's evidence actually decides.
#[derive(Clone, Copy, PartialEq, Debug, Serialize)]
#[serde(rename_all = "lowercase")]
enum Settled {
    Present,
    Absent,
    /// The prefix answers the same for a route nobody wrote, so nothing about
    /// THIS route was learned. Counted, never read as presence.
    Unfalsifiable,
    /// Nobody could be asked, and the table has no word either.
    Blind,
}

/// THE RULE OF THIS GATE, and the reason it is a pure function of the evidence
/// rather than a paragraph: it can then be stated as a test, and the tests at the
/// foot of this file are that statement.
///
/// The CONTROL decides whether the real answer means anything at all. A prefix
/// that answers a route nobody wrote is a prefix whose answers say nothing about
/// any particular route — that is a relay door or an auth wall, and reading its
/// `403` as "served" is exactly how 66 invented operations were once "verified".
/// Only where the control is a confirmed `404` does the real answer decide, and
/// then it decides both ways: `404` is absence, anything else is presence.
///
/// Where no host can be asked, the live table's own word stands: it is complete
/// for a product it OWNS (so a missing route there is refuted without asking
/// anyone), and a door or a silence is not an answer.
fn settle(says: Says, probe: Option<(Probe, Probe)>) -> Settled {
    match probe {
        Some((real, control)) => match (control, real) {
            (Probe::Blind, _) | (_, Probe::Blind) => Settled::Blind,
            // The control answered. The prefix cannot tell a real route from an
            // invented one, so it has not testified about this one.
            (Probe::Answered(_) | Probe::Flapping(_), _) => Settled::Unfalsifiable,
            (Probe::Absent, Probe::Absent) => Settled::Absent,
            (Probe::Absent, _) => Settled::Present,
        },
        None => match says {
            Says::Serves => Settled::Present,
            Says::Refutes => Settled::Absent,
            Says::Door | Says::Silent => Settled::Unfalsifiable,
        },
    }
}

// ---- reachability ------------------------------------------------------------

/// Does this spelling resolve to a command? Asked of the built binary, because
/// that is the only thing that knows. clap prints `Usage: hanzo <spelling> …`
/// for a command it has and falls back to the ROOT usage for one it does not, so
/// the usage line is the answer and no help-page format is parsed.
fn resolves(hanzo: &Path, spelling: &str) -> bool {
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

/// Which command a coordinate IS, in the words a person types. A gate that can
/// only print `GET /v1/x` makes the reader do the join; and a coordinate with no
/// command at all is a different fact — elided by the verb fold, or curated out —
/// which is worth saying rather than leaving as a blank.
fn commands(method: &str, path: &str) -> Vec<String> {
    OPS.iter()
        .filter(|o| o.method == method && o.path == path)
        .map(|o| {
            let mut s = vec!["hanzo", o.product];
            s.extend(o.nodes.iter().copied());
            s.push(o.verb);
            s.join(" ")
        })
        .collect()
}

fn named(method: &str, path: &str) -> String {
    let cmds = commands(method, path);
    if cmds.is_empty() {
        format!("{method:<7}{path}  (no command — elided by the verb fold, or curated out)")
    } else {
        format!("{method:<7}{path}  ⇒  {}", cmds.join(" | "))
    }
}

// ---- arguments ---------------------------------------------------------------

const USAGE: &str = "usage: driftgate [--refresh [--check]] [--registry <url|path>] [--host <url>] \
                     [--spec <path>] [--live <path>] [--hanzo <path>]";

struct Args {
    refresh: bool,
    check: bool,
    registry: String,
    host: String,
    spec: PathBuf,
    live: PathBuf,
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
        refresh: false,
        check: false,
        registry: DEFAULT_REGISTRY.to_string(),
        host: DEFAULT_HOST.to_string(),
        spec: manifest.join("spec/cloud.json"),
        live: manifest.join("spec/live.json"),
        hanzo: sibling,
    };
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let mut i = 0;
    while i < argv.len() {
        let flag = argv[i].clone();
        // The valueless flags are decided first, so an unknown flag says so and
        // `--help` is answered rather than told it needs a value.
        match flag.as_str() {
            "--refresh" => {
                a.refresh = true;
                i += 1;
                continue;
            }
            "--check" => {
                a.check = true;
                i += 1;
                continue;
            }
            "-h" | "--help" => {
                println!("{USAGE}");
                std::process::exit(0)
            }
            _ => {}
        }
        let set: fn(&mut Args, String) = match flag.as_str() {
            "--registry" => |a, v| a.registry = v,
            "--host" => |a, v| a.host = v,
            "--spec" => |a, v| a.spec = PathBuf::from(v),
            "--live" => |a, v| a.live = PathBuf::from(v),
            "--hanzo" => |a, v| a.hanzo = PathBuf::from(v),
            other => panic!("{USAGE}\nunknown: {other}"),
        };
        set(&mut a, argv.get(i + 1).cloned().unwrap_or_else(|| panic!("{flag} needs a value\n{USAGE}")));
        i += 2;
    }
    assert!(!(a.check && !a.refresh), "--check is a mode of --refresh: it re-asks the host and \
         refuses to write when a verdict moved. The default run is already a check, and it needs \
         no network.\n{USAGE}");
    a
}

// ---- asking (the ONLY part that touches the network) -------------------------

/// Ask the host whether anything is routed at this path. Read-only by
/// construction: a `GET`, and a `405` is an ANSWER, not a failure. Only a `404` is
/// re-asked — every other answer has already settled the question — and each round
/// tolerates one transport failure, because a dropped connection is not evidence
/// about a route.
async fn ask(client: &reqwest::Client, host: &str, path: &str) -> Vec<Option<u16>> {
    let url = format!("{}{}", host.trim_end_matches('/'), path);
    let mut answers = Vec::with_capacity(CONFIRM);
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
    answers
}

/// Read the live route table. Retried for the same reason a 404 is confirmed: one
/// dropped connection is not a fact about anything.
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
            println!(
                "driftgate: BLIND — {src} did not answer, three times: {last}\n\
                 Nothing was captured. This is not \"no drift\" — it is \"I could not look\"."
            );
            std::process::exit(1);
        };
        body
    } else {
        std::fs::read_to_string(src).unwrap_or_else(|e| panic!("read {src}: {e}"))
    };
    serde_json::from_str(&body).unwrap_or_else(|e| panic!("{src} is not the JSON route table: {e}"))
}

/// Ask everything, once, and return the evidence. Concurrency is bounded because
/// this is somebody's production API, and a gate that reads like an attack gets
/// itself rate-limited into a BLIND verdict.
async fn gather(a: &Args, spec: &Value, spec_sha: String, spec_ref: String) -> Evidence {
    let table = Table::read(&read(&a.registry).await);
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(20))
        // A REDIRECT IS AN ANSWER, and following it asks a different question
        // about a different address. This turned two live routes into false 404s:
        // `GET /v1/o11y/complete/google` answers 303 (an OAuth callback), the gate
        // followed it to `/v1/o11y/login?…` and recorded THAT page's 404 against
        // the callback's name — reporting cloud as contradicting itself about a
        // route that had just answered. Same class as reading a 403 as absence:
        // the status this gate reasons about must be the status of the address it
        // asked about.
        .redirect(reqwest::redirect::Policy::none())
        .user_agent("hanzo-driftgate")
        .build()
        .expect("http client");

    let mut says = BTreeMap::new();
    let mut targets: BTreeSet<String> = BTreeSet::new();
    for (m, path, askable) in coordinates(spec) {
        says.insert(coord(&m, &path), table.says(&m, &segs(&path)));
        if askable {
            targets.insert(path);
        }
    }
    // One control per PREFIX: "does this prefix discriminate?" is one question per
    // prefix, and asking it once per path would be 744 requests proving the same
    // thing 221 times over.
    let controls: BTreeSet<String> = targets.iter().map(|p| control_of(p)).collect();

    let mut probes = BTreeMap::new();
    let mut control_answers = BTreeMap::new();
    let all: Vec<(String, bool)> = targets
        .iter()
        .map(|p| (p.clone(), false))
        .chain(controls.iter().map(|p| (p.clone(), true)))
        .collect();
    let total = all.len();
    let mut done = 0usize;
    for batch in all.chunks(16) {
        let mut set = tokio::task::JoinSet::new();
        for (p, is_control) in batch {
            let (c, h, p, is_control) = (client.clone(), a.host.clone(), p.clone(), *is_control);
            set.spawn(async move {
                let answers = ask(&c, &h, &p).await;
                (p, is_control, answers)
            });
        }
        while let Some(r) = set.join_next().await {
            let (p, is_control, answers) = r.expect("probe task");
            if is_control {
                control_answers.insert(parent(&p), answers);
            } else {
                probes.insert(p, answers);
            }
        }
        done += batch.len();
        eprint!("\rdriftgate: asked {done}/{total}");
    }
    eprintln!();

    Evidence {
        source: Source {
            r#ref: spec_ref,
            host: a.host.clone(),
            registry: a.registry.clone(),
            spec_sha256: spec_sha,
        },
        products: table.owned,
        table: says,
        probes,
        controls: control_answers,
    }
}

// ---- ruling (no network, ever) -----------------------------------------------

/// One coordinate's whole story, as the report needs it.
struct Row {
    method: String,
    path: String,
    says: Says,
    verdict: Settled,
    /// The code the host answered, where one was asked for — printed so a reader
    /// can see the 401/403 split with their own eyes rather than trusting that the
    /// gate did not conflate them.
    code: Option<u16>,
    flapping: bool,
}

/// Re-derive every verdict from the evidence. This is the whole ruling, and it is
/// a pure function of (spec, capture) — which is what makes the gate hermetic and
/// what makes a corrected rule re-judge old evidence instead of needing new.
fn rule(spec: &Value, ev: &Evidence) -> (Vec<Row>, Vec<(String, String)>) {
    let (mut rows, mut unproven) = (Vec::new(), Vec::new());
    for (method, path, askable) in coordinates(spec) {
        let key = coord(&method, &path);
        let Some(&says) = ev.table.get(&key) else {
            unproven.push((method, path));
            continue;
        };
        let probe = if askable {
            let real = ev.probes.get(&path).map(|a| verdict(a));
            let control = ev.controls.get(&parent(&path)).map(|a| verdict(a));
            match (real, control) {
                (Some(r), Some(c)) => Some((r, c)),
                // A coordinate the capture calls askable but never asked about is
                // not "probably fine" — it is unproven, and unproven fails.
                _ => {
                    unproven.push((method, path));
                    continue;
                }
            }
        } else {
            None
        };
        let (code, flapping) = match probe.map(|(r, _)| r) {
            Some(Probe::Answered(c)) => (Some(c), false),
            Some(Probe::Flapping(c)) => (Some(c), true),
            _ => (None, false),
        };
        rows.push(Row { method, path, says, verdict: settle(says, probe), code, flapping });
    }
    (rows, unproven)
}

// ---- the gate ----------------------------------------------------------------

fn lock_ref(manifest: &Path) -> String {
    let lock = std::fs::read_to_string(manifest.join(".spec-lock")).expect("read .spec-lock");
    lock.lines()
        .find_map(|l| l.strip_prefix("ref="))
        .expect(".spec-lock has no ref= — this tree does not name a document")
        .to_string()
}

#[tokio::main]
async fn main() {
    // `driftgate | head` should end like `ls | head` does, not with a Rust panic
    // about a broken pipe.
    #[cfg(unix)]
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
    }

    let a = args();
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let spec_bytes = std::fs::read(&a.spec).unwrap_or_else(|e| panic!("read {}: {e}", a.spec.display()));
    let spec: Value = serde_json::from_slice(&spec_bytes)
        .unwrap_or_else(|e| panic!("{} is not a spec: {e}", a.spec.display()));
    let spec_sha = sha256(&spec_bytes);
    let spec_ref = lock_ref(&manifest);

    if a.refresh {
        refresh(&a, &spec, spec_sha, spec_ref).await;
        return;
    }
    gate(&a, &spec, &spec_sha, &spec_ref);
}

/// ASK. The only mode that touches the network, and the only one that writes.
async fn refresh(a: &Args, spec: &Value, spec_sha: String, spec_ref: String) {
    let ev = gather(a, spec, spec_sha, spec_ref).await;

    // A capture with a hole in it is a gate that will pass on a blind spot for as
    // long as nobody refreshes again. Refuse to write one.
    let blind: Vec<&String> = ev
        .probes
        .iter()
        .chain(ev.controls.iter())
        .filter(|(_, ans)| verdict(ans) == Probe::Blind)
        .map(|(p, _)| p)
        .collect();
    if !blind.is_empty() {
        println!("driftgate --refresh: BLIND on {} target(s) — nothing written.", blind.len());
        for p in blind.iter().take(20) {
            println!("   {p}");
        }
        println!("   A capture with a hole in it passes the gate on that hole forever. Re-run.");
        std::process::exit(1);
    }

    let fresh = Capture::seal(ev);
    if a.check {
        // The NIGHTLY. Not a byte comparison — a 401 that became a 403 is the same
        // fact about the same route, and a gate that goes red for it is a gate
        // people learn to switch off. What must not move in silence is a VERDICT.
        let have: Capture = serde_json::from_slice(
            &std::fs::read(&a.live).unwrap_or_else(|e| panic!("read {}: {e}", a.live.display())),
        )
        .unwrap_or_else(|e| panic!("{} is not a capture: {e}", a.live.display()));
        let (was, _) = rule(spec, &have.evidence);
        let (now, _) = rule(spec, &fresh.evidence);
        let key = |r: &Row| coord(&r.method, &r.path);
        let old: BTreeMap<String, Settled> = was.iter().map(|r| (key(r), r.verdict)).collect();
        let mut moved: Vec<String> = Vec::new();
        for r in &now {
            match old.get(&key(r)) {
                Some(&before) if before != r.verdict => moved.push(format!(
                    "{:<7}{}  {:?} → {:?}   {}",
                    r.method,
                    r.path,
                    before,
                    r.verdict,
                    commands(&r.method, &r.path).join(" | ")
                )),
                None => moved.push(format!("{:<7}{}  NEW", r.method, r.path)),
                _ => {}
            }
        }
        let gone: BTreeSet<&String> = have.evidence.products.difference(&fresh.evidence.products).collect();
        let new: BTreeSet<&String> = fresh.evidence.products.difference(&have.evidence.products).collect();
        if moved.is_empty() && gone.is_empty() && new.is_empty() {
            println!("driftgate --refresh --check: the live server still says what {} records.", a.live.display());
            return;
        }
        println!("!! THE LIVE SERVER MOVED under the committed evidence.");
        for m in &moved {
            println!("   {m}");
        }
        for p in gone {
            println!("   product {p} is no longer served");
        }
        for p in new {
            println!("   product {p} is served now and was not");
        }
        println!("\nRe-capture with `make live`, commit spec/live.json, and read what the gate then says.");
        std::process::exit(1);
    }

    let bytes = serde_json::to_vec(&fresh).expect("encode capture");
    std::fs::write(&a.live, &bytes).unwrap_or_else(|e| panic!("write {}: {e}", a.live.display()));
    let (rows, _) = rule(spec, &fresh.evidence);
    let count = |v: Settled| rows.iter().filter(|r| r.verdict == v).count();
    println!(
        "driftgate --refresh: {} coordinates — {} present, {} absent, {} unfalsifiable; {} products \
         served -> {} ({} bytes)",
        rows.len(),
        count(Settled::Present),
        count(Settled::Absent),
        count(Settled::Unfalsifiable),
        fresh.evidence.products.len(),
        a.live.display(),
        bytes.len()
    );
}

/// RULE. No network, no clock, no host — a pure function of what is committed.
fn gate(a: &Args, spec: &Value, spec_sha: &str, spec_ref: &str) {
    assert!(
        a.hanzo.is_file(),
        "no `hanzo` at {} — build it first (`cargo build --bin hanzo`), because whether a product \
         is reachable is a fact about the built CLI",
        a.hanzo.display()
    );
    let raw = std::fs::read(&a.live).unwrap_or_else(|e| {
        panic!("read {}: {e}\nNo evidence, no verdict. Capture it with `make live`.", a.live.display())
    });
    let cap: Capture = serde_json::from_slice(&raw)
        .unwrap_or_else(|e| panic!("{} is not a capture: {e}", a.live.display()));
    let ev = &cap.evidence;

    let mut fail = false;
    let broke = |title: &str, rows: Vec<String>| {
        if rows.is_empty() {
            return;
        }
        println!("\n!! {title}");
        for r in &rows {
            println!("   {r}");
        }
    };

    // ---- THE CHAIN. Every link pinned by a digest whose writer is a generator,
    // so an artifact edited by hand stops matching what it is derived from.
    let mut chain = Vec::new();
    if !cap.sealed() {
        chain.push(format!(
            "{} does not match its own digest — it was edited by hand. Evidence is CAPTURED, never \
             written: re-run `make live`.",
            a.live.display()
        ));
    }
    if ev.source.spec_sha256 != spec_sha {
        chain.push(format!(
            "{} is evidence about sha256:{}, and {} hashes to sha256:{}.\n   \
             Either that spec was hand-edited — it is @generated from hanzoai/cloud's document and \
             nothing else — or it was regenerated onto a new release and nobody re-asked the server. \
             `make live` settles the second; `make spec-check` settles the first.",
            a.live.display(),
            &ev.source.spec_sha256[..16],
            a.spec.display(),
            &spec_sha[..16]
        ));
    }
    if ev.source.r#ref != spec_ref {
        chain.push(format!(
            "{} is evidence about hanzoai/cloud@{}, and .spec-lock pins @{spec_ref}. Evidence about \
             one release cannot rule on a projection of another — `make live`.",
            a.live.display(),
            ev.source.r#ref
        ));
    }
    if !chain.is_empty() {
        // Nothing below this line can mean anything if the chain is broken, and a
        // gate that reports fifty consequential failures for one broken link
        // teaches people to skim. Stop here.
        println!("driftgate — the derivation chain");
        for c in &chain {
            println!("\n!! {c}");
        }
        println!("\ndriftgate: the chain is broken. Nothing else was checked.");
        std::process::exit(1);
    }

    // ---- direction one: a route the server does not serve --------------------
    let (rows, unproven) = rule(spec, ev);
    let mut codes: BTreeMap<u16, usize> = BTreeMap::new();
    for r in rows.iter().filter_map(|r| r.code) {
        *codes.entry(r).or_default() += 1;
    }
    let count = |v: Settled| rows.iter().filter(|r| r.verdict == v).count();
    let by = |v: Settled, claims: bool| -> Vec<String> {
        rows.iter()
            .filter(|r| r.verdict == v && r.says.claims() == claims)
            .map(|r| named(&r.method, &r.path))
            .collect()
    };
    let ahead = by(Settled::Absent, false);
    let contradicted = by(Settled::Absent, true);
    let blind: Vec<String> = by(Settled::Blind, true).into_iter().chain(by(Settled::Blind, false)).collect();
    let flapping: Vec<&Row> = rows.iter().filter(|r| r.flapping).collect();

    // ---- the same direction, asked of the OTHER tree --------------------------
    // The local commands' routes are literals, and the document decides them the
    // same way it decides a generated coordinate. A product cloud does not own is
    // not cloud's to answer for — see [`sent`].
    let owned = products(spec);
    let local = sent(&PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src"));
    assert!(
        !local.is_empty(),
        "the sweep read no /v1 path out of src/ at all — the local commands do send some, so the \
         read is broken and a broken read reports 'no drift' for the one reason a gate may never \
         report it"
    );
    let invented: Vec<String> = local
        .iter()
        .filter(|(p, _)| owned.contains(segs(p)[1]) && !declares(spec, p))
        .map(|(p, f)| format!("{p:<40}sent by {f}"))
        .collect();

    // ---- direction two: a served product no command reaches ------------------
    let mut universe = ev.products.clone();
    universe.extend(products(spec));
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
    println!(
        "driftgate — {} against hanzoai/cloud@{spec_ref}, on evidence in {}",
        a.spec.display(),
        a.live.display()
    );
    println!(
        "  coordinates          {:>5}   {} present, {} absent, {} unfalsifiable",
        rows.len(),
        count(Settled::Present),
        count(Settled::Absent),
        count(Settled::Unfalsifiable)
    );
    // The histogram is printed, not just totalled. Conflating 404 with 401/403 is
    // the one mistake this gate exists not to make, so every log carries the split
    // that proves it did not: a run whose "present" is all 401 has still seen 401,
    // and anyone reading can tell.
    println!(
        "  the host answered    {:>5}   {}",
        codes.values().sum::<usize>(),
        codes.iter().map(|(c, n)| format!("{c}×{n}")).collect::<Vec<_>>().join(" ")
    );
    println!(
        "  controls             {:>5}   prefixes asked with a route nobody wrote; {} of them \
         answered, so nothing under those prefixes can be decided",
        ev.controls.len(),
        ev.controls.values().filter(|a| verdict(a) != Probe::Absent).count()
    );
    println!("  products reachable   {reachable:>5}   of {}", universe.len());
    // Printed, because a sweep that quietly reads nothing is a gate that passes
    // for the wrong reason — the failure mode of every lexical read ever written.
    println!(
        "  hand-written routes  {:>5}   /v1 paths the local commands send, read from src/; {} \
         under a product cloud owns",
        local.len(),
        local.keys().filter(|p| owned.contains(segs(p)[1])).count()
    );
    println!(
        "  declared exceptions  {:>5}   applied of {} in src/curation.rs",
        excused.len(),
        curation::CURATED.len()
    );
    if !flapping.is_empty() {
        println!("\n   {} route(s) answered 404 and then answered — present, but not reliably:", flapping.len());
        for r in &flapping {
            println!("   {:<7}{}", r.method, r.path);
        }
    }

    broke(
        "UNPROVEN — spec/cloud.json carries a coordinate the evidence has never seen. \
         Either the capture is stale (`make live`), or this coordinate was added by hand to a \
         @generated file, in which case it is a phantom by construction",
        unproven.iter().map(|(m, p)| named(m, p)).collect(),
    );
    fail |= !unproven.is_empty();

    broke(
        "PHANTOM — a hand-written command sends a route the document does not declare, in a \
         product whose whole route list the document carries. Delete the command, or serve the \
         route in hanzoai/cloud",
        invented.clone(),
    );
    fail |= !invented.is_empty();

    broke(
        "BLIND — nothing was learned about this coordinate. A gate that cannot see must not pass",
        blind.clone(),
    );
    fail |= !blind.is_empty();

    broke(
        "ORPHAN — served, and no command reaches it. Add the command, or declare it in \
         src/curation.rs with a reason a person can act on",
        orphans.clone(),
    );
    fail |= !orphans.is_empty();

    broke(
        "STALE EXCEPTION — the curation table sends people to a command that does not exist",
        stale.iter().map(|(p, s)| format!("{p:<16}claims `hanzo {s}`")).collect(),
    );
    fail |= !stale.is_empty();

    // The pinned document naming a route the deploy has not caught up with is a
    // WAIT, and waiting is not a defect anybody here can fix. Pinned so the wait
    // cannot lengthen unread, and free to fall the moment cloud ships.
    if !ahead.is_empty() {
        let over = ahead.len() > AHEAD;
        fail |= over;
        println!(
            "\n{} AHEAD OF THE DEPLOY — the pinned document declares these routes, the live table \
             does not name them, and the host denies them ({} of at most {AHEAD})",
            if over { "!!" } else { "  " },
            ahead.len()
        );
        for p in &ahead {
            println!("   {p}");
        }
        println!(
            "   cloud's source says the route exists and the binary answering api.hanzo.ai was built\n   \
             before it did. Nothing here makes it answer. If this grew, the pin moved further ahead of\n   \
             production than a release should sit; if it shrank, the deploy landed — bring AHEAD down\n   \
             to {} in src/bin/driftgate.rs.",
            ahead.len()
        );
    }

    // Cloud's table and cloud's server disagreeing is REAL and it is not ours: no
    // edit in this repo makes `GET /v1/ai/applications` answer, and a gate that
    // turns this build red for it is a gate people learn to switch off. So it is a
    // CEILING — it may not grow in silence, and it is allowed to fall the moment
    // somebody redeploys, without turning a nightly red for the crime of
    // production getting better.
    if !contradicted.is_empty() {
        let over = contradicted.len() > CONTRADICTED;
        fail |= over;
        println!(
            "\n{} CLOUD CONTRADICTS ITSELF — cloud's own live table claims these routes and cloud's \
             own host denies them ({} of at most {CONTRADICTED})",
            if over { "!!" } else { "  " },
            contradicted.len()
        );
        for p in &contradicted {
            println!("   {p}");
        }
        println!(
            "   A route registered with a dead mount behind it: the router knows it, so the table it\n   \
             projects claims it, and the server still has nothing to run. The fix is in hanzoai/cloud,\n   \
             never a list here. If this grew, file it there; if it shrank, bring CONTRADICTED down to\n   \
             {} in src/bin/driftgate.rs.",
            contradicted.len()
        );
    }

    let unfalsifiable = count(Settled::Unfalsifiable);
    if unfalsifiable > UNFALSIFIABLE {
        fail = true;
        println!(
            "\n!! THE BLIND SPOT GREW: {unfalsifiable} coordinates sit behind a prefix that answers \
             the same for a route nobody wrote, and UNFALSIFIABLE says {UNFALSIFIABLE}.\n   \
             This is the exact surface on which a second authority could once assert anything and \
             call it verified —\n   66 of the 71 operations the deleted master contributed were \
             'confirmed' by a relay door. It falls on its own\n   as hanzoai/cloud types those \
             relays into real operations; it may not rise without somebody saying why."
        );
    }

    if excused.len() != EXCUSED {
        fail = true;
        let verb = if excused.len() > EXCUSED { "GREW" } else { "SHRANK" };
        println!("\n!! DECLARED EXCEPTIONS {verb}: {} applied, EXCUSED says {EXCUSED}", excused.len());
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
        println!("\ndriftgate: the CLI surface and the live server disagree.");
        std::process::exit(1);
    }
    println!("\ndriftgate: no drift.");
}

/// The gate's rules, pinned. All three are pure — that is why they were separated
/// from the transport and from the report — and each encodes a rule this gate
/// exists to keep, which is a rule a paragraph cannot enforce.
#[cfg(test)]
mod tests {
    use super::*;

    /// `401`/`403` say the route is THERE and wants a caller; `405` says the path
    /// is routed and the verb is not; only `404` says nothing is at that address.
    /// An earlier hand analysis of this exact surface conflated them and reported
    /// three production breaks that were not breaks — a red build that teaches
    /// people the build lies.
    #[test]
    fn an_auth_refusal_is_a_route_that_exists_and_only_a_confirmed_404_is_absent() {
        // 303 is in this list because it was MEASURED: `GET /v1/o11y/complete/google`
        // answers it, and the transport used to follow the redirect and report the
        // 404 of wherever it landed — a present route recorded as absent.
        for code in [200, 201, 204, 302, 303, 400, 401, 403, 405, 409, 429, 500, 502, 503] {
            assert!(
                matches!(verdict(&[Some(code)]), Probe::Answered(c) if c == code),
                "{code} is an answer FROM a route — a router with nothing at that address cannot \
                 produce it, so it is never absence"
            );
        }
        assert_eq!(verdict(&[Some(404); CONFIRM]), Probe::Absent);
    }

    /// A SINGLE 404 IS NOT EVIDENCE — the same mistake one layer down. Fourteen
    /// `/v1/pricing` paths answered 404 to one concurrent sweep and 200 to every
    /// serial re-ask a minute later.
    #[test]
    fn a_404_that_does_not_hold_is_flapping_and_never_drift() {
        assert_eq!(verdict(&[Some(404), Some(200)]), Probe::Flapping(200));
        assert_eq!(verdict(&[Some(404), Some(404), Some(403)]), Probe::Flapping(403));
        assert_eq!(verdict(&[Some(404), Some(404)]), Probe::Blind, "two 404s are not the {CONFIRM} that confirm one");
        assert_eq!(verdict(&[None]), Probe::Blind);
        assert_eq!(verdict(&[Some(404), None]), Probe::Blind);
        assert_eq!(verdict(&[]), Probe::Blind);
    }

    /// THE CONTROL IS WHAT MAKES AN ANSWER MEAN ANYTHING. A prefix that answers a
    /// route nobody wrote — a relay door, an auth wall — has said nothing about
    /// any particular route under it, and reading its `403` (or its `200`) as
    /// "served" is exactly how 66 invented operations were once verified.
    #[test]
    fn a_prefix_that_answers_for_nonsense_has_testified_about_nothing() {
        let (real, absent) = (Probe::Answered(200), Probe::Absent);
        // The door: control 403, so a 403 on the real path proves nothing…
        assert_eq!(settle(Says::Door, Some((Probe::Answered(403), Probe::Answered(403)))), Settled::Unfalsifiable);
        // …and neither does a 404 under a prefix that 200s for nonsense.
        assert_eq!(settle(Says::Serves, Some((absent, Probe::Answered(200)))), Settled::Unfalsifiable);
        // A discriminating prefix is what makes both readings possible.
        assert_eq!(settle(Says::Silent, Some((real, absent))), Settled::Present);
        assert_eq!(settle(Says::Serves, Some((absent, absent))), Settled::Absent);
        // Silence is never an answer, from either side.
        assert_eq!(settle(Says::Serves, Some((Probe::Blind, absent))), Settled::Blind);
        assert_eq!(settle(Says::Serves, Some((real, Probe::Blind))), Settled::Blind);
    }

    /// Where no host can be asked, the table's own word stands — and it may only
    /// refute inside a product it OWNS. A door and a silence are not answers.
    #[test]
    fn the_table_refutes_only_inside_a_product_it_owns_and_a_door_is_not_an_answer() {
        let t = Table::read(&serde_json::json!({"paths": {
            "/v1/billing/usage": {"get": {}},
            "/v1/iam/{wildcard1}": {"get": {}},
            "/v1/{wildcard1}": {"get": {}}
        }}));
        assert_eq!(t.says("GET", &segs("/v1/billing/usage")), Says::Serves);
        assert_eq!(t.says("GET", &segs("/v1/billing/no-such-route")), Says::Refutes);
        assert_eq!(t.says("GET", &segs("/v1/iam/anything/at/all")), Says::Door);
        assert_eq!(t.says("GET", &segs("/v1/nosuchproduct/x")), Says::Silent);
        assert_eq!(t.says("POST", &segs("/v1/billing/usage")), Says::Refutes, "a verb is part of the route");
        assert_eq!(t.owned, ["billing", "iam"].iter().map(ToString::to_string).collect());

        assert_eq!(settle(Says::Serves, None), Settled::Present);
        assert_eq!(settle(Says::Refutes, None), Settled::Absent);
        assert_eq!(settle(Says::Door, None), Settled::Unfalsifiable);
        assert_eq!(settle(Says::Silent, None), Settled::Unfalsifiable);
    }

    /// WHOSE defect an absence is turns on who claimed the route, and `Door` is on
    /// the claiming side: a door says the subtree relays, so a denial behind one is
    /// cloud's table disagreeing with cloud's server — not a route this repo made up.
    #[test]
    fn only_a_route_no_document_claims_is_this_repos_phantom() {
        assert!(Says::Serves.claims() && Says::Door.claims());
        assert!(!Says::Refutes.claims() && !Says::Silent.claims());
    }

    /// A CONTROL IS A SIBLING, not a child and not a cousin: same prefix, one
    /// segment, a name nobody wrote. `/v1/models`'s sibling lives under the bare
    /// `/v1` on purpose — that is precisely the question "does the global
    /// fallthrough swallow everything?".
    #[test]
    fn a_control_is_a_nonsense_sibling_under_the_same_prefix() {
        assert_eq!(control_of("/v1/o11y/services"), format!("/v1/o11y/{NONSENSE}"));
        assert_eq!(control_of("/v1/models"), format!("/v1/{NONSENSE}"));
        assert_eq!(parent(&control_of("/v1/o11y/services")), "/v1/o11y");
    }

    /// The report names the COMMAND, because that is what a person types and what
    /// a fix has to touch — and it says so plainly when a coordinate has none.
    #[test]
    fn a_failing_coordinate_is_named_as_the_command_it_is() {
        let op = OPS.iter().find(|o| o.method == "GET").expect("the tree has a read");
        let line = named(op.method, op.path);
        assert!(line.contains(&format!("hanzo {}", op.product)), "{line}");
        assert!(named("GET", "/v1/no-such-product/nothing").contains("no command"));
    }

    /// THE SWEEP MUST ACTUALLY READ SOMETHING. A lexical read that quietly
    /// matches nothing reports "no drift" for the one reason a gate may never
    /// report it, so the routes the hand-written commands are KNOWN to send are
    /// asserted by name: `hanzo status` composes exactly these three.
    #[test]
    fn the_sweep_reads_the_routes_the_local_commands_send() {
        let local = sent(&PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src"));
        for p in ["/v1/k8s/clusters", "/v1/deploy/applications", "/v1/fleet/workers"] {
            let from = local.get(p).unwrap_or_else(|| panic!("the sweep missed {p}"));
            assert!(from.ends_with("status.rs"), "{p} is sent by {from}");
        }
        // Prose that quotes a route is not a route, and a query string is not
        // part of an address.
        assert!(local.keys().all(|p| !p.contains(' ') && !p.contains('?')), "{local:?}");
    }

    /// THE DOCUMENT DECIDES, and only for the products it owns. `hanzo fabric`
    /// talks to a hanzo NODE, so `/v1/node/*` is not cloud's to refute — which is
    /// why the sweep needs no exception list beside the document.
    #[test]
    fn the_sweep_rules_only_where_the_document_is_the_authority() {
        let doc = serde_json::json!({"paths": {
            "/v1/billing/balance": {"get": {}},
            "/v1/agents/sessions/{id}/events": {"post": {}},
        }});
        assert!(declares(&doc, "/v1/billing/balance"));
        // The router's parameter names are its own.
        assert!(declares(&doc, "/v1/agents/sessions/{session}/events"));
        // The phantom: a product the document owns, at an address it does not.
        assert!(!declares(&doc, "/v1/billing/deposit"));
        assert!(products(&doc).contains("billing"));
        // And the node's routes are outside the document's authority entirely.
        assert!(!products(&doc).contains("node"));
    }

    /// Evidence is CAPTURED, never written. The seal is the only thing standing
    /// between "the server said so" and "somebody typed it".
    #[test]
    fn a_capture_that_was_edited_by_hand_is_not_sealed() {
        let ev = Evidence {
            source: Source {
                r#ref: "v0.0.0".into(),
                host: "h".into(),
                registry: "r".into(),
                spec_sha256: "s".into(),
            },
            products: BTreeSet::new(),
            table: BTreeMap::from([(coord("GET", "/v1/kv"), Says::Serves)]),
            probes: BTreeMap::new(),
            controls: BTreeMap::new(),
        };
        let mut cap = Capture::seal(ev);
        assert!(cap.sealed());
        cap.evidence.table.insert(coord("GET", "/v1/invented"), Says::Serves);
        assert!(!cap.sealed(), "a row was added and the file still claimed its own digest");
    }
}
