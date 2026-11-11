//! `hanzo status` — THE HANZO CLOUD in one screen: what is broken FIRST, then
//! the clusters, the applications and the machines that reported.
//!
//! Composed from three surfaces cloud already serves, read CONCURRENTLY through
//! the one authenticated seam (`product::Seam` — origin from `network`, bearer
//! from the active identity, no org header ever):
//!
//! | surface        | route                              | rows       | the command it is |
//! |----------------|------------------------------------|------------|-------------------|
//! | clusters       | `GET /v1/compute/k8s/clusters`     | `clusters` | `hanzo compute k8s clusters list` |
//! | applications   | `GET /v1/deploy/applications`      | `items` (argocd) | `hanzo deploy applications list` |
//! | compute nodes  | `GET /v1/compute/fleet/workers`    | `workers`  | `hanzo compute fleet workers` |
//!
//! There is no fourth wire and no CLI-only side channel: everything here is a
//! route the API, the SDKs and the console already read. The last column is why
//! this is a VIEW and not a duplicate — each of those three is a generated
//! command anybody can run, and `status` adds only the arrangement:
//! unhealthy-first, counted, and a surface that did not answer rendered as
//! unavailable rather than zero. It introduces no route of its own.
//!
//! ## Two laws
//!
//! **Most important first, and drift is not breakage.** What is BROKEN leads —
//! an application whose HEALTH is not `Healthy`, a cluster that is not running, a
//! node that is not online — enumerated and named. An application that is
//! `Healthy` but not `Synced` has merely DRIFTED from the tag the universe
//! declares; on this fleet that is 193 of 339, so it gets ONE counted line under
//! the alarms. Collapsing the two buried the single `Missing` application under
//! 193 that were serving fine. The rest is grouped and COUNTED, so 339
//! applications are four lines instead of 339.
//!
//! **A surface that did not answer is UNAVAILABLE, never zero.** A 403 rendered
//! as "0 applications" is a lie that reads exactly like a healthy fleet — the
//! failure this command exists to prevent. So a refusal prints the status AND
//! the server's own reason, an empty 200 prints "none reported", and an answer
//! whose list we cannot find prints "unreadable". One failing surface never
//! fails the command; only failing to read ANY of them does.

use anyhow::{bail, Result};
use colored::*;
use hanzo_client::{Method, Reply};
use serde_json::Value;

use crate::commands::product::{envelope_error, Seam};
use crate::config::Config;

/// One surface's answer: the rows it reported, or WHY it could not be read.
pub(crate) type Reading = std::result::Result<Vec<Value>, String>;

/// One surface's full answer when read for the status page: its rows plus any
/// partial-source failures (`degraded: [{source, reason}]`) reported beside them.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Surface {
    pub rows: Vec<Value>,
    pub degraded: Vec<(String, String)>,
}

type SurfaceReading = std::result::Result<Surface, String>;

/// One thing that needs attention: `(what it is, its name, the verdict)`.
type Problem = (&'static str, String, String);

pub(crate) const CLUSTERS_ROUTE: &str = "/v1/compute/k8s/clusters";
pub(crate) const APPS_ROUTE: &str = "/v1/deploy/applications";
pub(crate) const WORKERS_ROUTE: &str = "/v1/compute/fleet/workers";
pub(crate) const SANDBOXES_ROUTE: &str = "/v1/sandbox";

pub async fn run(cfg: &mut Config, infer_only: bool) -> Result<()> {
    if infer_only {
        let telem = crate::commands::monitor::collect_telemetry().await;
        crate::commands::monitor::render_dashboard(&telem);
        return Ok(());
    }

    // Signed out, this refuses and names the way in: an empty cloud is never
    // rendered as though it were the truth. The local GPU machines are
    // `--infer`, and never stand in for a cloud that did not answer.
    let seam = Seam::open(cfg, crate::iam::paths::DEFAULT_BRAND).await?;
    // Concurrent: three independent reads, so the page costs one round trip.
    let (clusters, apps, nodes) = tokio::join!(
        read_surface(&seam, CLUSTERS_ROUTE, "clusters"),
        read_surface(&seam, APPS_ROUTE, "items"),
        read_surface(&seam, WORKERS_ROUTE, "workers"),
    );

    render(&clusters, &apps, &nodes);

    if clusters.is_err() && apps.is_err() && nodes.is_err() {
        bail!("no surface answered — nothing above was read from the cloud");
    }
    Ok(())
}

/// `hanzo sandbox list` / `hanzo ls` — lists the calling org's live sandboxes
/// from `GET /v1/sandbox` instead of printing local TUI sample fixtures.
pub async fn list_sandboxes(cfg: &mut Config) -> Result<()> {
    let seam = Seam::open(cfg, crate::iam::paths::DEFAULT_BRAND).await?;
    let rows = read(&seam, SANDBOXES_ROUTE, "sandboxes")
        .await
        .map_err(|e| anyhow::anyhow!("sandbox list unavailable — {e}"))?;
    for line in format_sandbox_table(&rows) {
        println!("{line}");
    }
    Ok(())
}

pub(crate) fn format_sandbox_table(rows: &[Value]) -> Vec<String> {
    if rows.is_empty() {
        return vec!["No active sandboxes in this organization.".to_string()];
    }
    let mut out = Vec::with_capacity(rows.len() + 1);
    out.push(format!(
        "{:<28} {:<10} {:<10} {:<10} {:<20} IMAGE",
        "ID", "CLASS", "RUNTIME", "STATUS", "PROJECT"
    ));
    for sbx in rows {
        out.push(format!(
            "{:<28} {:<10} {:<10} {:<10} {:<20} {}",
            first(sbx, &["/id", "/name"], "—"),
            first(sbx, &["/class"], "—"),
            first(sbx, &["/runtime"], "—"),
            first(sbx, &["/status"], "unknown"),
            first(sbx, &["/project"], "—"),
            first(sbx, &["/image"], "—"),
        ));
    }
    out
}

/// Read ONE surface into its rows, or into the reason there are none.
///
/// A transport fault, a non-2xx, cloud's own 200-with-an-error envelope and an
/// answer with no list in it all become the SAME honest value — a reason string
/// — so an empty page is only ever printed when the server actually said empty.
pub(crate) async fn read(seam: &Seam, path: &str, key: &str) -> Reading {
    read_surface(seam, path, key).await.map(|s| s.rows)
}

async fn read_surface(seam: &Seam, path: &str, key: &str) -> SurfaceReading {
    let Reply { status, body } = seam
        .send(Method::GET, path, &[], None)
        .await
        .map_err(|e| format!("{e:#}"))?;
    if !(200..300).contains(&status) {
        return Err(reason(status, &body));
    }
    parse_surface(&body, key)
}

/// Pure body decoder for [`read_surface`]: extracts `key` and any additive
/// `degraded: [{source, reason}]` list. When `rows` is empty ONLY because an
/// upstream source failed, this returns `Err` rather than `Ok(empty)` so an
/// upstream 403/502 folded into `[]` never renders as `"none reported"`.
fn parse_surface(body: &Value, key: &str) -> SurfaceReading {
    if let Some(msg) = envelope_error(body) {
        return Err(msg);
    }
    // The `/v1` envelope is `{status,msg,data}` where there is one; a plane that
    // answers its payload directly is read directly. Same rule the product tree
    // prints by, so both surface the same thing.
    let payload = body.get("data").unwrap_or(body);
    let Some(rows) = payload.get(key).and_then(Value::as_array) else {
        return Err(format!("unreadable — the answer carried no `{key}` list"));
    };
    let degraded = parse_degraded(payload);
    if rows.is_empty() && !degraded.is_empty() {
        let reasons = degraded
            .iter()
            .map(|(src, why)| {
                if why.starts_with(&format!("{src}:")) {
                    why.clone()
                } else {
                    format!("{src}: {why}")
                }
            })
            .collect::<Vec<_>>()
            .join("; ");
        return Err(format!("degraded — {reasons}"));
    }
    Ok(Surface {
        rows: rows.clone(),
        degraded,
    })
}

fn parse_degraded(payload: &Value) -> Vec<(String, String)> {
    let Some(items) = payload.get("degraded").and_then(Value::as_array) else {
        return Vec::new();
    };
    items
        .iter()
        .map(|d| {
            let src = first(d, &["/source"], "upstream");
            let why = terse(&first(d, &["/reason", "/error", "/msg"], "unavailable"));
            (src, why)
        })
        .collect()
}

/// The server's own words for a refusal, behind its status: `403 not authorized
/// for this deploy console`. Never a bare code, and never silence.
fn reason(status: u16, body: &Value) -> String {
    let msg = envelope_error(body).unwrap_or_else(|| match body {
        Value::Null => String::new(),
        Value::String(s) => s.to_string(),
        v => v.to_string(),
    });
    format!("{status} {}", terse(&msg)).trim_end().to_string()
}

/// The server's words as ONE short line. A section heading must stay a heading,
/// so an ingress's HTML error page is collapsed and clipped rather than dumped
/// over the page it was meant to explain — and the clip is MARKED, never silent.
fn terse(s: &str) -> String {
    let one = s.split_whitespace().collect::<Vec<_>>().join(" ");
    match one.char_indices().nth(120) {
        Some((i, _)) => format!("{}…", &one[..i]),
        None => one,
    }
}

// ---- reading a row honestly --------------------------------------------------

/// The first of `ptrs` (JSON pointers) the server actually answered, else
/// `fallback`. The ONE reader for every field on this page, so "the server did
/// not say" has exactly one spelling and no field is ever invented.
fn first(v: &Value, ptrs: &[&str], fallback: &str) -> String {
    ptrs.iter()
        .filter_map(|p| v.pointer(p).and_then(Value::as_str))
        .find(|s| !s.trim().is_empty())
        .unwrap_or(fallback)
        .to_string()
}

/// A cluster's lifecycle state. The fold serves DOKS and BYO clusters through
/// one list and they do not spell status alike, so both spellings are read — and
/// a cluster whose state we cannot read is UNKNOWN, which is not "running".
fn state(c: &Value) -> String {
    first(c, &["/status/state", "/status", "/state"], "unknown")
}

/// How many worker nodes a cluster reports, when it reports any. `None` is
/// UNKNOWN and prints as nothing — a node count the server never sent must never
/// appear as a zero.
fn node_count(c: &Value) -> Option<usize> {
    // The cluster list states its own total, and the server's number beats one
    // this tree re-derives: a cluster can report a total with its pools elided,
    // and summing an absent `nodePools` would answer UNKNOWN about a count we
    // were just handed.
    let stated = ["/nodeCount", "/node_count"].iter().find_map(|p| c.pointer(p));
    if let Some(n) = stated.and_then(Value::as_u64) {
        return Some(n as usize);
    }
    let pools = ["/nodePools", "/node_pools"]
        .iter()
        .find_map(|p| c.pointer(p))
        .and_then(Value::as_array)?;
    Some(
        pools
            .iter()
            .map(|p| {
                p.get("nodes")
                    .and_then(Value::as_array)
                    .map(Vec::len)
                    .or_else(|| p.get("count").and_then(Value::as_u64).map(|n| n as usize))
                    .unwrap_or(0)
            })
            .sum(),
    )
}

/// An application's verdict — `<health> / <sync>`, each as the server stated it.
fn verdict(a: &Value) -> String {
    format!(
        "{} / {}",
        first(a, &["/status/health/status"], "Unknown"),
        first(a, &["/status/sync/status"], "Unknown")
    )
}

/// Join the parts a host actually reported, dropping the ones it did not.
fn join(parts: &[String]) -> String {
    parts.iter().filter(|s| !s.is_empty()).cloned().collect::<Vec<_>>().join(" · ")
}

// ---- the page ----------------------------------------------------------------

fn render(clusters: &SurfaceReading, apps: &SurfaceReading, nodes: &SurfaceReading) {
    let problems = problems(clusters, apps, nodes);
    if problems.is_empty() {
        // Only claim health when every surface was actually READ. A page with an
        // unavailable surface says nothing about the fleet, and saying "all
        // clear" over it would be the exact lie this command refuses to tell.
        if [clusters, apps, nodes].iter().all(|r| r.is_ok()) {
            println!("{}", "all clear — nothing unhealthy reported".green());
        }
    } else {
        println!("{} — {}", "attention".red().bold(), problems.len());
        for (kind, name, verdict) in &problems {
            println!("  {:<12} {:<28} {}", kind, name, verdict.red());
        }
    }
    // Drift is not an alarm: these are serving. It goes UNDER the alarms, in one
    // line, so a fleet that deploys constantly does not read as a fleet on fire.
    let drifted = drift(apps);
    if drifted > 0 {
        println!("{} — {drifted} serving an older tag than declared", "drift".yellow().bold());
    }

    println!();
    head("clusters", clusters);
    if let Ok(s) = clusters {
        for c in &s.rows {
            let count = node_count(c).map(|n| format!("{n} nodes")).unwrap_or_default();
            println!(
                "  {:<28} {}",
                first(c, &["/name", "/id"], "unnamed"),
                join(&[state(c), first(c, &["/region"], ""), count])
            );
        }
    }

    println!();
    head("applications", apps);
    if let Ok(s) = apps {
        // Grouped, never enumerated: the unhealthy ones are already named above,
        // so here every verdict is one counted line — 336 applications included.
        let mut counts: std::collections::BTreeMap<String, usize> = Default::default();
        for a in &s.rows {
            *counts.entry(verdict(a)).or_default() += 1;
        }
        for (v, n) in counts {
            println!("  {:<28} {n}", v);
        }
    }

    println!();
    head("compute", nodes);
    if let Ok(s) = nodes {
        for n in &s.rows {
            node(n);
        }
    }
}

/// One section heading: the count, "none reported", or WHY there is no count.
fn head(label: &str, r: &SurfaceReading) {
    match r {
        Err(why) => println!("{}: {} ({why})", label.bold(), "unavailable".yellow()),
        Ok(s) if s.rows.is_empty() => println!("{}: {}", label.bold(), "none reported".dimmed()),
        Ok(s) => println!("{} — {}", label.bold(), s.rows.len()),
    }
}

/// One BYO machine, in the detail shape the fleet view has always used.
fn node(n: &Value) {
    let status = first(n, &["/status"], "unknown");
    let dot = if status == "online" { "●".green() } else { "●".red() };
    println!("{} {}", dot, first(n, &["/hostname", "/id"], "unnamed").bold());

    row("status", format!("{status}{}", provider(n)));
    row(
        "cpu",
        join(&[
            num(n, "/cpus").map(|c| format!("{c} cores")).unwrap_or_default(),
            first(n, &["/arch"], ""),
            first(n, &["/cpuModel"], ""),
        ]),
    );
    row("memory", num(n, "/memory").map(gib).unwrap_or_default());
    let gpus: &[Value] = n.pointer("/gpus").and_then(Value::as_array).map_or(&[], Vec::as_slice);
    for (i, g) in gpus.iter().enumerate() {
        let mem = match first(g, &["/memoryTotal"], "") {
            m if m.is_empty() => String::new(),
            m => format!(" ({m})"),
        };
        row(&format!("gpu[{i}]"), format!("{}{mem}", first(g, &["/name"], "unnamed")));
    }
    row("engine", engine(n));
    row("heartbeat", first(n, &["/lastHeartbeat"], ""));
}

/// A whole number the server stated at `ptr`, or `None` when it did not.
fn num(v: &Value, ptr: &str) -> Option<u64> {
    v.pointer(ptr).and_then(Value::as_u64)
}

/// The provider, parenthesised as the fleet view states it — omitted entirely
/// when the host did not report one.
fn provider(n: &Value) -> String {
    match first(n, &["/provider"], "") {
        p if p.is_empty() => String::new(),
        p => format!(" ({p})"),
    }
}

/// The local engine a host is serving, as it reported it.
fn engine(n: &Value) -> String {
    let Some(e) = n.pointer("/engine") else { return String::new() };
    let (url, st) = (first(e, &["/url"], ""), first(e, &["/status"], ""));
    let head = match (url.is_empty(), st.is_empty()) {
        (false, false) => format!("{url} — {st}"),
        (false, true) => url,
        (true, _) => st,
    };
    let models = e
        .pointer("/models")
        .and_then(Value::as_array)
        .map(|m| format!("{} models", m.len()))
        .unwrap_or_default();
    join(&[head, models])
}

/// Bytes as the whole GiB a person reads.
fn gib(bytes: u64) -> String {
    format!("{} GiB", (bytes as f64 / (1024.0 * 1024.0 * 1024.0)).round())
}

/// One detail row — printed ONLY when the host actually reported the value.
fn row(label: &str, value: String) {
    if !value.is_empty() {
        println!("    {label:<10}{value}");
    }
}

/// Everything that is BROKEN, in one list, most-important-first order:
/// applications, then clusters, then machines.
///
/// Broken is judged on HEALTH alone, never on sync. An application that is
/// Healthy but OutOfSync has DRIFTED — its declared tag and its running tag
/// disagree — and drift is a routine state of a fleet that deploys constantly,
/// not an incident. Collapsing the two put 182 drifted-but-serving apps in the
/// same list as the one that was actually down, which buries the incident in
/// the noise and teaches the reader to skip the block that matters most.
/// Drift is counted by `drift`, one line, below the alarms.
fn is_healthy_cluster_state(s: &str) -> bool {
    // Managed DOKS clusters report "running"; attached BYO kubeconfig clusters
    // report "attached" (cloud/apps/compute/byo.go).
    s.eq_ignore_ascii_case("running") || s.eq_ignore_ascii_case("attached")
}

fn problems(clusters: &SurfaceReading, apps: &SurfaceReading, nodes: &SurfaceReading) -> Vec<Problem> {
    let mut out: Vec<Problem> = Vec::new();
    if let Ok(s) = apps {
        for a in &s.rows {
            if first(a, &["/status/health/status"], "Unknown") != "Healthy" {
                out.push(("application", first(a, &["/metadata/name"], "unnamed"), verdict(a)));
            }
        }
        for (src, why) in &s.degraded {
            out.push(("source", src.clone(), why.clone()));
        }
    }
    if let Ok(s) = clusters {
        for c in &s.rows {
            let st = state(c);
            if !is_healthy_cluster_state(&st) {
                out.push(("cluster", first(c, &["/name", "/id"], "unnamed"), st));
            }
        }
        for (src, why) in &s.degraded {
            out.push(("source", src.clone(), why.clone()));
        }
    }
    if let Ok(s) = nodes {
        for n in &s.rows {
            let st = first(n, &["/status"], "unknown");
            if st != "online" {
                out.push(("node", first(n, &["/hostname", "/id"], "unnamed"), st));
            }
        }
        for (src, why) in &s.degraded {
            out.push(("source", src.clone(), why.clone()));
        }
    }
    out
}

/// Applications that are serving but whose running tag is not the declared one.
/// Reported as a COUNT: naming 182 rows that are all fine is not information.
fn drift(apps: &SurfaceReading) -> usize {
    let Ok(s) = apps else { return 0 };
    s.rows
        .iter()
        .filter(|a| {
            first(a, &["/status/health/status"], "Unknown") == "Healthy"
                && first(a, &["/status/sync/status"], "Unknown") != "Synced"
        })
        .count()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn app(name: &str, health: &str, sync: &str) -> Value {
        json!({"metadata":{"name":name},"status":{"health":{"status":health},"sync":{"status":sync}}})
    }

    // ---- a surface that did not answer is never a zero ----------------------

    /// THE DEFECT: a 403 rendered as "0 applications" reads exactly like a
    /// healthy fleet. It must carry the status AND the server's own reason.
    #[test]
    fn a_refusal_carries_the_status_and_the_servers_own_reason() {
        let body = json!({"status": 403, "error": "not authorized for this deploy console"});
        assert_eq!(
            reason(403, &body),
            "403 not authorized for this deploy console"
        );
    }

    /// An ingress that answers HTML is still answering — but a heading must stay
    /// a heading, so the page is collapsed to one clipped line, marked as clipped.
    #[test]
    fn an_html_error_page_is_clipped_to_one_marked_line() {
        let page = Value::String(format!("<html>\n  <body>{}</body>\n</html>", "x".repeat(500)));
        let r = reason(404, &page);
        assert!(r.starts_with("404 <html> <body>xxx"), "{r}");
        assert!(r.ends_with('…'), "a clip must be visible: {r}");
        assert!(r.chars().count() <= 126, "one line, not a page: {} chars", r.chars().count());
    }

    /// An empty 200 is the server SAYING empty — a different fact from a refusal,
    /// and the only one that may render as "none reported".
    #[test]
    fn an_empty_200_is_rows_and_a_missing_list_is_not() {
        // The three live shapes, exactly as the routes answer them.
        assert_eq!(rows(&json!({"clusters": []}), "clusters"), Ok(vec![]));
        assert_eq!(rows(&json!({"kind": "ApplicationList", "items": []}), "items"), Ok(vec![]));
        // A shape with no list at all is UNREADABLE, never an empty fleet.
        assert!(rows(&json!({"unexpected": "shape"}), "workers").is_err());
    }

    /// The `/v1` envelope, when a plane wraps its payload in one.
    #[test]
    fn an_enveloped_payload_is_read_through_data() {
        assert_eq!(rows(&json!({"status":"ok","data":{"workers":[1]}}), "workers"), Ok(vec![json!(1)]));
    }

    fn ok_rows(rows: Vec<Value>) -> SurfaceReading {
        Ok(Surface {
            rows,
            degraded: Vec::new(),
        })
    }

    /// The pure half of [`read`] — the exact [`parse_surface`] body decoder, without a wire.
    fn rows(body: &Value, key: &str) -> std::result::Result<Vec<Value>, String> {
        parse_surface(body, key).map(|s| s.rows)
    }

    /// An empty list accompanied by `degraded` means a folded upstream failed
    /// (`visor: upstream 403`), NEVER `"none reported"` or `"all clear"`.
    #[test]
    fn degraded_empty_clusters_is_unavailable_not_none_reported() {
        let body = json!({
            "clusters": [],
            "degraded": [{"source": "visor", "reason": "visor: upstream 403"}]
        });
        assert_eq!(
            parse_surface(&body, "clusters"),
            Err("degraded — visor: upstream 403".into())
        );
    }

    /// Attached BYO clusters (`status == "attached"`) are healthy alongside
    /// managed `"running"` clusters, while partial `degraded` sources still
    /// surface in `attention`.
    #[test]
    fn attached_byo_cluster_is_healthy_and_partial_degraded_leads_attention() {
        let healthy_byo = parse_surface(
            &json!({
                "clusters": [
                    {"name": "hanzo-aws", "region": "aws-us-east-1", "status": "attached", "nodeCount": 26},
                    {"name": "hanzo-doks", "region": "nyc3", "status": "running", "nodeCount": 3}
                ]
            }),
            "clusters",
        );
        assert!(problems(&healthy_byo, &ok_rows(vec![]), &ok_rows(vec![])).is_empty());

        let partial = parse_surface(
            &json!({
                "clusters": [
                    {"name": "hanzo-aws", "region": "aws-us-east-1", "status": "attached", "nodeCount": 26}
                ],
                "degraded": [{"source": "visor", "reason": "visor: unreachable"}]
            }),
            "clusters",
        );
        assert_eq!(
            problems(&partial, &ok_rows(vec![]), &ok_rows(vec![])),
            vec![("source", "visor".into(), "visor: unreachable".into())]
        );
    }

    // ---- most important first -----------------------------------------------

    /// Only the BROKEN lead the page, and each names what it is, which one, and
    /// the verdict — applications first, then clusters, then machines. A Healthy
    /// but OutOfSync app (`kms` here) has drifted and is deliberately NOT here:
    /// it is serving, and 182 of these would bury the one that is down.
    #[test]
    fn only_the_broken_lead_the_page_and_drift_is_not_broken() {
        let apps = ok_rows(vec![
            app("gateway", "Healthy", "Synced"),
            app("iam", "Degraded", "Synced"),
            app("kms", "Healthy", "OutOfSync"),
        ]);
        let clusters = ok_rows(vec![
            json!({"name": "hanzo-k8s", "status": "running"}),
            json!({"name": "hanzo-aws", "status": "attached"}),
            json!({"name": "zoo-k8s", "status": {"state": "provisioning"}}),
        ]);
        let nodes = ok_rows(vec![
            json!({"hostname": "evo", "status": "online"}),
            json!({"hostname": "spark", "status": "offline"}),
        ]);

        let p = problems(&clusters, &apps, &nodes);
        assert_eq!(
            p,
            vec![
                ("application", "iam".into(), "Degraded / Synced".into()),
                ("cluster", "zoo-k8s".into(), "provisioning".into()),
                ("node", "spark".into(), "offline".into()),
            ]
        );
        // The drifted one is counted, not alarmed about.
        assert_eq!(drift(&apps), 1);
    }

    /// A cluster's own stated total wins over a re-derivation, and a cluster
    /// that states neither a total nor pools is UNKNOWN — never a zero.
    #[test]
    fn a_cluster_reports_its_own_node_total() {
        assert_eq!(node_count(&json!({"nodeCount": 21, "nodePools": []})), Some(21));
        assert_eq!(node_count(&json!({"nodePools": [{"count": 3}, {"count": 2}]})), Some(5));
        assert_eq!(node_count(&json!({"nodePools": [{"nodes": [{}, {}]}]})), Some(2));
        assert_eq!(node_count(&json!({"name": "silent"})), None);
    }

    /// Drift is HEALTH-gated: an app that is both unhealthy AND out of sync is
    /// an incident, already named above, and must not be double-counted as
    /// drift. Only the serving-but-stale are drift.
    #[test]
    fn drift_counts_only_the_healthy_but_stale() {
        let apps = ok_rows(vec![
            app("a", "Healthy", "OutOfSync"),
            app("b", "Healthy", "Unknown"),
            app("c", "Degraded", "OutOfSync"),
            app("d", "Healthy", "Synced"),
        ]);
        assert_eq!(drift(&apps), 2);
        // And a surface that did not answer contributes no drift, never a zero.
        assert_eq!(drift(&Err("403 nope".into())), 0);
    }

    /// A verdict the server did not state is UNKNOWN — and unknown is not
    /// healthy, so it leads the page rather than hiding in the counts.
    #[test]
    fn an_unstated_verdict_is_unknown_and_never_healthy() {
        let apps = ok_rows(vec![json!({"metadata": {"name": "mystery"}})]);
        let p = problems(&ok_rows(vec![]), &apps, &ok_rows(vec![]));
        assert_eq!(p, vec![("application", "mystery".into(), "Unknown / Unknown".into())]);
        assert_eq!(state(&json!({"name": "x"})), "unknown");
    }

    /// An unavailable surface contributes NO problems — we know nothing about it,
    /// and inventing failures is the mirror of inventing health.
    #[test]
    fn an_unavailable_surface_is_not_a_problem_and_not_a_zero() {
        let out = problems(&Err("403 nope".into()), &Err("403 nope".into()), &ok_rows(vec![]));
        assert!(out.is_empty());
    }

    // ---- the machine detail shape -------------------------------------------

    /// The fleet detail shape, from the live `/v1/fleet/workers` row — and only
    /// the fields the host actually reported.
    #[test]
    fn a_node_renders_only_what_the_host_reported() {
        let n = json!({
            "hostname": "evo", "status": "online", "provider": "byo",
            "cpus": 32, "arch": "x86_64", "cpuModel": "AMD RYZEN AI MAX+ 395 w/ Radeon 8060S",
            "memory": 133_622_599_680u64,
            "gpus": [{"name": "AMD Ryzen AI Max+ 395 (gfx1151)", "memoryTotal": "131072 MiB"}],
            "engine": {"url": "http://localhost:1234", "status": "ready", "models": ["a", "b"]},
            "lastHeartbeat": "2026-08-01T18:35:09Z"
        });
        assert_eq!(format!("{}{}", first(&n, &["/status"], ""), provider(&n)), "online (byo)");
        assert_eq!(
            join(&[
                num(&n, "/cpus").map(|c| format!("{c} cores")).unwrap_or_default(),
                first(&n, &["/arch"], ""),
                first(&n, &["/cpuModel"], ""),
            ]),
            "32 cores · x86_64 · AMD RYZEN AI MAX+ 395 w/ Radeon 8060S"
        );
        assert_eq!(gib(133_622_599_680), "124 GiB");
        assert_eq!(engine(&n), "http://localhost:1234 — ready · 2 models");
        assert_eq!(node_count(&json!({"nodePools": [{"count": 3}, {"nodes": [1, 2]}]})), Some(5));
        // A cluster that never stated its pools has an UNKNOWN node count, which
        // prints as nothing — never as "0 nodes".
        assert_eq!(node_count(&json!({"name": "x"})), None);
        // A host that reported no engine gets no engine row at all.
        assert_eq!(engine(&json!({"hostname": "bare"})), "");
    }

    /// `hanzo status` and `hanzo sandbox list` address the canonical HIP-0139
    /// routes under `/v1/compute/*` and `/v1/sandbox`, and render the calling
    /// org's sandboxes (never hardcoded TUI demo fixtures).
    #[test]
    fn canonical_status_and_sandbox_list_routes_and_org_table() {
        assert_eq!(CLUSTERS_ROUTE, "/v1/compute/k8s/clusters");
        assert_eq!(WORKERS_ROUTE, "/v1/compute/fleet/workers");
        assert_eq!(SANDBOXES_ROUTE, "/v1/sandbox");

        assert_eq!(
            format_sandbox_table(&[]),
            vec!["No active sandboxes in this organization.".to_string()]
        );

        let org_rows = rows(
            &json!({
                "sandboxes": [{
                    "id": "m_af4b4e5a5c8fdce4b32fbb4f",
                    "org": "jamessburnss1313",
                    "class": "dev",
                    "runtime": "visor",
                    "status": "running",
                    "project": "guided-growth",
                    "image": "oci.hanzo.ai/hanzoai/sandbox:latest"
                }]
            }),
            "sandboxes",
        )
        .expect("sandboxes list");
        let lines = format_sandbox_table(&org_rows);
        assert_eq!(lines.len(), 2);
        assert!(lines[1].contains("m_af4b4e5a5c8fdce4b32fbb4f"), "{}", lines[1]);
        assert!(lines[1].contains("guided-growth"), "{}", lines[1]);
        for demo in ["hanzo-bot-1", "claude-workspace", "qwen3-eval", "lux-dex-prover"] {
            assert!(!lines.join("\n").contains(demo), "must not contain sample fixture {demo}");
        }
    }
}
