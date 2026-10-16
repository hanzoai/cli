//! The console's Savings page: what coding through Hanzo has used and saved,
//! measured apart from estimated.
//!
//! Two sources, read in the background and never added together:
//!
//! - **The cloud**: `GET /v1/usage/summary` for the org, over the selected range,
//!   through the one authenticated seam. Read every ten seconds; it shows the fields
//!   the cloud answers (`hanzo_gen::savings::Llm`) and says so when it answers none of
//!   the cache fields. The linked provider accounts sit beside the org's
//!   Hanzo-routed usage, as the cloud sends them.
//! - **A local kai-gateway** (`hanzo-inc/kai-gateway`): `GET /dashboard/savings`
//!   on each of its default ports, 8787 to 8793, every two seconds like the
//!   machine's own readings. Its summary has a MEASURED part (provider-reported
//!   tokens at the listed price, cache hits and their dollars, Kai's own use) and an
//!   ESTIMATED part (the calls Kai answered directly, priced as the range's average
//!   call), and this page keeps the two in separate columns and colours.
//!
//! A source that did not answer is shown as unavailable, never as zero.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{channel, Receiver};
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, SecondsFormat, Utc};
use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Paragraph, Wrap},
    Frame,
};
use serde::Deserialize;
use serde_json::Value;
use tokio::sync::Notify;

use crate::commands::savings;
use hanzo_gen::savings::{int, pct, usd, Llm, Plan};
use crate::config::Config;

/// The windows `t` steps through.
pub const RANGES: [&str; 3] = ["24h", "7d", "30d"];

/// kai-gateway's default ports, by the client each serves.
pub const GATEWAYS: [(u16, &str); 7] = [
    (8787, "default"),
    (8788, "gemini"),
    (8789, "claude"),
    (8790, "codex"),
    (8791, "opencode"),
    (8792, "devin"),
    (8793, "kilo"),
];

/// How often the local gateways are read, and the cloud.
const LOCAL: Duration = Duration::from_secs(2);
const CLOUD: Duration = Duration::from_secs(10);

// ---- what a gateway answers ---------------------------------------------------

/// `GET /dashboard/savings` of one kai-gateway: the fields the console shows. The
/// gateway leaves out what it cannot compute, so every price is optional.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Gateway {
    #[serde(skip)]
    pub port: u16,
    #[serde(skip)]
    pub client: String,
    pub requests: u64,
    pub measured: Measured,
    pub estimated: Estimated,
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Measured {
    pub llm_calls: u64,
    pub tokens: Tokens,
    pub cached_share: Option<f64>,
    pub spend_usd: Option<f64>,
    pub cache_saved_usd: Option<f64>,
    pub unpriced_calls: u64,
    pub kai: Kai,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Tokens {
    pub input: u64,
    pub output: u64,
    pub cached: u64,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Kai {
    pub decisions: u64,
    pub input_tokens: u64,
    pub cost_usd: Option<f64>,
    pub latency_ms: Latency,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Latency {
    pub p50: Option<f64>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Estimated {
    pub avoided_calls: u64,
    pub usd: Option<f64>,
    pub net_usd: Option<f64>,
}

/// What the background readers send the page.
pub enum Update {
    Cloud(Result<Value, String>),
    Gateways(Vec<Gateway>),
}

/// Shared with the readers: the range the page shows, and a nudge to read now.
struct Live {
    range: AtomicUsize,
    wake: Notify,
}

/// The page's state.
#[derive(Default)]
pub struct Board {
    pub range: usize,
    pub cloud: Option<Result<Value, String>>,
    pub gateways: Vec<Gateway>,
    /// A scan of the gateway ports has finished at least once.
    pub scanned: bool,
    rx: Option<Receiver<Update>>,
    live: Option<Arc<Live>>,
}

impl Board {
    /// A board fed by background readers on the current tokio runtime. Without a
    /// runtime (a unit test) it is a board nobody feeds.
    pub fn start() -> Board {
        let (tx, rx) = channel();
        let live = Arc::new(Live { range: AtomicUsize::new(0), wake: Notify::new() });
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            let (tx_cloud, shared) = (tx.clone(), live.clone());
            handle.spawn(async move {
                let mut cfg = Config::load(None).unwrap_or_default();
                loop {
                    let range = RANGES[shared.range.load(Ordering::Relaxed) % RANGES.len()];
                    let query = vec![format!("range={range}")];
                    if tx_cloud.send(Update::Cloud(savings::summary(&mut cfg, crate::iam::paths::DEFAULT_BRAND, &query).await)).is_err() {
                        break;
                    }
                    tokio::select! {
                        _ = tokio::time::sleep(CLOUD) => {}
                        _ = shared.wake.notified() => {}
                    }
                }
            });
            let shared = live.clone();
            handle.spawn(async move {
                let Ok(client) = reqwest::Client::builder().timeout(Duration::from_millis(800)).build() else { return };
                loop {
                    let range = RANGES[shared.range.load(Ordering::Relaxed) % RANGES.len()];
                    if tx.send(Update::Gateways(scan(&client, since(range, Utc::now())).await)).is_err() {
                        break;
                    }
                    tokio::time::sleep(LOCAL).await;
                }
            });
        }
        Board { rx: Some(rx), live: Some(live), ..Default::default() }
    }

    /// Take everything the readers have sent since the last frame.
    pub fn poll(&mut self) {
        let updates: Vec<Update> = match &self.rx {
            Some(rx) => rx.try_iter().collect(),
            None => return,
        };
        for u in updates {
            self.apply(u);
        }
    }

    pub fn apply(&mut self, update: Update) {
        match update {
            Update::Cloud(r) => self.cloud = Some(r),
            Update::Gateways(g) => {
                self.gateways = g;
                self.scanned = true;
            }
        }
    }

    /// Step to the next range and read the cloud for it now.
    pub fn cycle(&mut self) {
        self.range = (self.range + 1) % RANGES.len();
        self.refresh();
    }

    /// Read the cloud now.
    pub fn refresh(&self) {
        if let Some(live) = &self.live {
            live.range.store(self.range, Ordering::Relaxed);
            live.wake.notify_one();
        }
    }

    pub fn plans(&self) -> Vec<Plan> {
        match &self.cloud {
            Some(Ok(doc)) => Plan::all(doc),
            _ => Vec::new(),
        }
    }
}

/// Where a range starts, counted back from `now`.
pub fn since(range: &str, now: DateTime<Utc>) -> String {
    let (n, unit) = range.split_at(range.len() - 1);
    let n: i64 = n.parse().unwrap_or(24);
    let back = if unit == "d" { chrono::Duration::days(n) } else { chrono::Duration::hours(n) };
    (now - back).to_rfc3339_opts(SecondsFormat::Secs, true)
}

/// Ask every default port; the ones that answer a savings summary are gateways.
async fn scan(client: &reqwest::Client, from: String) -> Vec<Gateway> {
    let tasks: Vec<_> = GATEWAYS
        .iter()
        .map(|&(port, name)| {
            let (client, from) = (client.clone(), from.clone());
            tokio::spawn(async move { probe(&client, port, name, &from).await })
        })
        .collect();
    let mut out = Vec::new();
    for t in tasks {
        if let Ok(Some(g)) = t.await {
            out.push(g);
        }
    }
    out
}

async fn probe(client: &reqwest::Client, port: u16, name: &str, from: &str) -> Option<Gateway> {
    let url = format!("http://127.0.0.1:{port}/dashboard/savings");
    let reply = client.get(url).query(&[("format", "json"), ("from", from)]).send().await.ok()?;
    let mut g: Gateway = reply.error_for_status().ok()?.json().await.ok()?;
    g.port = port;
    g.client = name.to_string();
    Some(g)
}

// ---- rendering ------------------------------------------------------------------

const DIM: Color = Color::Rgb(140, 145, 155);
/// Measured values are plain; an estimate wears its own colour wherever it is shown.
const ESTIMATE: Color = Color::Yellow;

fn dim(s: impl Into<String>) -> Span<'static> {
    Span::styled(s.into(), Style::default().fg(DIM))
}

fn bold(s: impl Into<String>, c: Color) -> Span<'static> {
    Span::styled(s.into(), Style::default().fg(c).add_modifier(Modifier::BOLD))
}

fn panel(title: String) -> Block<'static> {
    Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(Color::DarkGray))
        .title(Span::styled(title, Style::default().fg(Color::White).add_modifier(Modifier::BOLD)))
}

pub fn render(f: &mut Frame, area: Rect, b: &Board) {
    let plans = b.plans();
    let below = if plans.is_empty() { 0 } else { plans.len().min(6) as u16 + 4 };
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(8), Constraint::Length(below)])
        .split(area);
    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(45), Constraint::Percentage(55)])
        .split(rows[0]);
    render_cloud(f, cols[0], b);
    render_local(f, cols[1], b);
    if below > 0 {
        render_plans(f, rows[1], &plans);
    }
}

fn render_cloud(f: &mut Frame, area: Rect, b: &Board) {
    let range = match &b.cloud {
        Some(Ok(doc)) => doc.get("range").and_then(Value::as_str).unwrap_or(RANGES[b.range]),
        _ => RANGES[b.range],
    };
    let block = panel(format!(" hanzo cloud · {range} "));
    let mut lines: Vec<Line> = Vec::new();
    match &b.cloud {
        None => lines.push(Line::from(dim("  reading /v1/usage/summary…"))),
        Some(Err(why)) => {
            lines.push(Line::from(bold("  unavailable", Color::Red)));
            lines.push(Line::from(dim(format!("  {why}"))));
            lines.push(Line::from(dim("  nothing here is a zero: the cloud did not answer")));
        }
        Some(Ok(doc)) => {
            let llm = Llm::of(doc);
            if llm.available == Some(false) {
                lines.push(Line::from(dim("  LLM totals: warehouse not connected")));
            }
            for (k, v) in llm.rows() {
                lines.push(Line::from(vec![bold(format!("  {k:<12}"), Color::White), Span::raw(v)]));
            }
            if !llm.caches() {
                lines.push(Line::from(dim("  cache       not reported by this cloud")));
            }
            let spend = doc.get("spend");
            if spend.and_then(|s| s.get("available")).and_then(Value::as_bool) == Some(true) {
                let cents = |k: &str| spend.and_then(|s| s.get(k)).and_then(Value::as_f64).map(|c| usd(c / 100.0));
                let parts: Vec<String> = [("in range", cents("totalCents")), ("month to date", cents("mtdCents"))]
                    .into_iter()
                    .filter_map(|(k, v)| v.map(|v| format!("{v} {k}")))
                    .collect();
                if !parts.is_empty() {
                    lines.push(Line::from(vec![bold("  ledger      ", Color::White), Span::raw(parts.join(" · "))]));
                }
            }
            if let (Some(a), Some(z)) = (doc.get("start").and_then(Value::as_str), doc.get("end").and_then(Value::as_str)) {
                lines.push(Line::from(""));
                lines.push(Line::from(dim(format!("  {a} → {z}"))));
            }
        }
    }
    f.render_widget(Paragraph::new(lines).block(block).wrap(Wrap { trim: false }), area);
}

/// A gateway's measured columns, then its estimate; the two are never summed.
fn render_local(f: &mut Frame, area: Rect, b: &Board) {
    let block = panel(" local kai-gateway ".into());
    let mut lines: Vec<Line> = Vec::new();
    if !b.scanned {
        lines.push(Line::from(dim("  scanning 127.0.0.1:8787-8793…")));
    } else if b.gateways.is_empty() {
        lines.push(Line::from(dim("  no kai-gateway answered on 127.0.0.1:8787-8793")));
        lines.push(Line::from(dim("  kai-claude and kai-codex start one for their client")));
    } else {
        lines.push(Line::from(dim(format!(
            "  {:<14}{:>6}{:>10}{:>8}{:>9}{:>9}  {}",
            "measured", "calls", "input", "cached", "spend", "cache $", "kai"
        ))));
        for g in &b.gateways {
            let m = &g.measured;
            let money = |v: Option<f64>| v.map(usd).unwrap_or_else(|| "—".into());
            let kai = match (m.kai.decisions, m.kai.latency_ms.p50) {
                (0, _) => "—".to_string(),
                (n, Some(ms)) => format!("{n} · {ms:.0} ms"),
                (n, None) => n.to_string(),
            };
            lines.push(Line::from(vec![
                bold(format!("  {:<14}", format!("{}:{}", g.client, g.port)), Color::Cyan),
                Span::raw(format!(
                    "{:>6}{:>10}{:>8}{:>9}{:>9}  {}",
                    int(m.llm_calls),
                    int(m.tokens.input),
                    m.cached_share.map(pct).unwrap_or_else(|| "—".into()),
                    money(m.spend_usd),
                    money(m.cache_saved_usd),
                    kai,
                )),
            ]));
        }
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "  estimated: a counterfactual, never added to the above",
            Style::default().fg(ESTIMATE),
        )));
        let mut any = false;
        for g in &b.gateways {
            let e = &g.estimated;
            if e.avoided_calls == 0 {
                continue;
            }
            any = true;
            let worth = e.usd.map(|v| format!(" ≈ {}", usd(v))).unwrap_or_default();
            let net = e.net_usd.map(|v| format!(", {} net of Kai", usd(v))).unwrap_or_default();
            lines.push(Line::from(Span::styled(
                format!("  {}:{}  {} call{} answered without an LLM{worth}{net}", g.client, g.port, e.avoided_calls, if e.avoided_calls == 1 { "" } else { "s" }),
                Style::default().fg(ESTIMATE),
            )));
        }
        if !any {
            lines.push(Line::from(dim("  no request was answered without an LLM call in this range")));
        }
        let unpriced: u64 = b.gateways.iter().map(|g| g.measured.unpriced_calls).sum();
        if unpriced > 0 {
            lines.push(Line::from(dim(format!("  {unpriced} call(s) went to a model the price list does not name: tokens only"))));
        }
        lines.push(Line::from(dim("  dollars are listed prices (api.hanzo.ai/v1/models), not what a plan charged")));
    }
    f.render_widget(Paragraph::new(lines).block(block).wrap(Wrap { trim: false }), area);
}

fn render_plans(f: &mut Frame, area: Rect, plans: &[Plan]) {
    let block = panel(" linked plans and Hanzo-routed usage · listed apart, never summed ".into());
    let mut lines = vec![Line::from(dim(format!(
        "  {:<9}{:<12}{:<8}{:>7}{:>10}{:>14}{:>9}",
        "source", "provider", "window", "used", "requests", "tokens", "cost"
    )))];
    for p in plans.iter().take(6) {
        let some = |v: Option<u64>| v.map(int).unwrap_or_else(|| "—".into());
        lines.push(Line::from(format!(
            "  {:<9}{:<12}{:<8}{:>7}{:>10}{:>14}{:>9}",
            p.source,
            p.provider,
            p.window,
            p.used.map(|u| format!("{u:.0}%")).unwrap_or_else(|| "—".into()),
            some(p.requests),
            some(p.tokens),
            p.cost_cents.map(|c| usd(c / 100.0)).unwrap_or_else(|| "—".into()),
        )));
    }
    f.render_widget(Paragraph::new(lines).block(block), area);
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{backend::TestBackend, Terminal};
    use serde_json::json;

    /// What a running kai-gateway answered to `POST /dashboard/savings` for four
    /// route events (two Claude Sonnet calls with prompt-cache hits, one `direct`
    /// answer, one call to a model the list does not name), the method text elided.
    const SAMPLE: &str = r#"{"requests":4,"prices":{"source":"https://api.hanzo.ai/v1/models","fetchedAt":"2026-10-12T05:34:51.358Z","models":[{"model":"claude-sonnet-4-5-20250929","id":"anthropic/claude-sonnet-4.5","calls":2,"inputPerMillion":3.165,"outputPerMillion":15.825,"cacheReadPerMillion":0.3165,"cacheWritePerMillion":3.95625}]},"measured":{"llmCalls":3,"tokens":{"input":192000,"output":1650,"cached":150000,"cacheWrite":5000,"reasoning":0},"cachedShare":0.78125,"cacheByMode":[{"mode":"hint","calls":1,"input":100000,"cached":80000,"share":0.8},{"mode":"passthrough","calls":2,"input":92000,"cached":70000,"share":0.7608695652173914}],"spendUsd":0.20335125,"cacheSavedUsd":0.42727500000000007,"cacheSavedTokens":135000,"unpricedCalls":1,"kai":{"decisions":2,"inputTokens":1100,"costUsd":2.31e-05,"latencyMs":{"p50":41,"p95":41}},"modes":{"hint":1,"passthrough":2,"direct":1}},"estimated":{"method":"\u2026","avoidedCalls":1,"unestimated":0,"inputTokens":95000,"outputTokens":800,"usd":0.101675625,"timeMs":3995,"netUsd":0.10165252500000001},"notes":["1 call went to a model the price list does not name; they are counted in tokens and left out of every dollar."]}"#;

    fn answer() -> Value {
        serde_json::from_str(SAMPLE).unwrap()
    }

    fn gateway(port: u16, client: &str) -> Gateway {
        let mut g: Gateway = serde_json::from_value(answer()).unwrap();
        g.port = port;
        g.client = client.into();
        g
    }

    fn screen(b: &Board) -> String {
        let mut term = Terminal::new(TestBackend::new(160, 32)).unwrap();
        term.draw(|f| render(f, f.area(), b)).unwrap();
        let buf = term.backend().buffer().clone();
        (0..buf.area.height)
            .map(|y| (0..buf.area.width).map(|x| buf.cell((x, y)).unwrap().symbol().to_string()).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn a_gateways_answer_reads_into_the_page() {
        let g = gateway(8789, "claude");
        assert_eq!(g.measured.llm_calls, 3);
        assert_eq!(g.measured.tokens.input, 192000);
        assert_eq!(g.measured.spend_usd, Some(0.20335125));
        assert_eq!(g.measured.kai.latency_ms.p50, Some(41.0));
        assert_eq!(g.estimated.avoided_calls, 1);
        // A gateway with no price list leaves its dollars out; they are None, not 0.
        let bare: Gateway = serde_json::from_value(json!({"requests": 1, "measured": {"llmCalls": 1, "tokens": {"input": 5, "output": 1, "cached": 0, "cacheWrite": 0, "reasoning": 0}, "unpricedCalls": 1, "kai": {"decisions": 0, "inputTokens": 0, "latencyMs": {}}, "modes": {}}, "estimated": {"avoidedCalls": 0, "unestimated": 0, "inputTokens": 0, "outputTokens": 0}, "notes": ["No price list could be read"]})).unwrap();
        assert_eq!(bare.measured.spend_usd, None);
        assert_eq!(bare.estimated.usd, None);
    }

    #[test]
    fn measured_and_estimated_are_shown_apart() {
        let mut b = Board::default();
        b.apply(Update::Gateways(vec![gateway(8789, "claude"), gateway(8790, "codex")]));
        b.apply(Update::Cloud(Ok(json!({
            "range": "24h", "start": "2026-10-10T12:00:00Z", "end": "2026-10-11T12:00:00Z",
            "spend": {"available": true, "totalCents": 191, "mtdCents": 5000},
            "llm": {"available": true, "requests": 41, "promptTokens": 412310, "completionTokens": 18204, "costCents": 191.0},
            "accounts": {"rows": [
                {"source": "account", "provider": "anthropic", "window": "5h", "usedPct": 62.5},
                {"source": "hanzo", "provider": "hanzo", "window": "24h", "requests": 41, "tokens": 430514, "costCents": 191}
            ]}
        }))));
        let text = screen(&b);
        for want in [
            "hanzo cloud · 24h", "calls", "41", "in 412,310 · out 18,204", "$1.91 metered",
            "cache       not reported by this cloud",
            "$1.91 in range · $50.00 month to date",
            "local kai-gateway", "claude:8789", "codex:8790", "78.1%", "$0.20", "$0.43", "2 · 41 ms",
            "estimated: a counterfactual, never added to the above",
            "claude:8789  1 call answered without an LLM ≈ $0.10, $0.10 net of Kai",
            "2 call(s) went to a model the price list does not name",
            "listed prices (api.hanzo.ai/v1/models), not what a plan charged",
            "linked plans and Hanzo-routed usage", "anthropic", "62%", "430,514",
        ] {
            assert!(text.contains(want), "missing {want:?} in:\n{text}");
        }
    }

    #[test]
    fn the_cloud_fields_show_once_it_sends_them() {
        let mut b = Board::default();
        b.apply(Update::Cloud(Ok(json!({"range": "7d", "llm": {
            "requests": 9, "cacheReadTokens": 354000, "cacheWriteTokens": 12000, "cachedShare": 0.86, "cacheSavedCents": 40.0,
            "kai": {"decisions": 12, "costCents": 3.5}}}))));
        let text = screen(&b);
        for want in ["hanzo cloud · 7d", "86.0% of input · read 354,000 · written 12,000", "cache saved", "$0.40", "cost $0.04 · decisions 12"] {
            assert!(text.contains(want), "missing {want:?} in:\n{text}");
        }
        assert!(!text.contains("not reported by this cloud"), "{text}");
    }

    #[test]
    fn what_did_not_answer_is_unavailable_never_zero() {
        let mut b = Board::default();
        let text = screen(&b);
        assert!(text.contains("reading /v1/usage/summary"), "{text}");
        assert!(text.contains("scanning 127.0.0.1:8787-8793"), "{text}");

        b.apply(Update::Cloud(Err("403 not authorized".into())));
        b.apply(Update::Gateways(vec![]));
        let text = screen(&b);
        assert!(text.contains("unavailable"), "{text}");
        assert!(text.contains("403 not authorized"), "{text}");
        assert!(text.contains("nothing here is a zero"), "{text}");
        assert!(text.contains("no kai-gateway answered on 127.0.0.1:8787-8793"), "{text}");
        assert!(!text.contains("linked plans"), "no plans without an answer: {text}");

        b.apply(Update::Cloud(Ok(json!({"llm": {"available": false}}))));
        let text = screen(&b);
        assert!(text.contains("LLM totals: warehouse not connected"), "{text}");
    }

    #[test]
    fn a_range_is_counted_back_from_now() {
        let now = DateTime::parse_from_rfc3339("2026-10-11T12:00:00Z").unwrap().with_timezone(&Utc);
        assert_eq!(since("24h", now), "2026-10-10T12:00:00Z");
        assert_eq!(since("7d", now), "2026-10-04T12:00:00Z");
        assert_eq!(since("30d", now), "2026-09-11T12:00:00Z");
        let mut b = Board::default();
        b.cycle();
        b.cycle();
        assert_eq!(RANGES[b.range], "30d");
        b.cycle();
        assert_eq!(RANGES[b.range], "24h", "the ranges wrap");
    }

    /// The reader against a gateway that answers as the real one does.
    #[tokio::test]
    async fn a_listening_gateway_is_found_and_a_silent_port_is_not() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let body = answer().to_string();
        tokio::spawn(async move {
            while let Ok((mut s, _)) = listener.accept().await {
                let body = body.clone();
                tokio::spawn(async move {
                    let mut buf = [0u8; 2048];
                    let n = s.read(&mut buf).await.unwrap_or(0);
                    let req = String::from_utf8_lossy(&buf[..n]).to_string();
                    let ok = req.starts_with("GET /dashboard/savings?") && req.contains("format=json") && req.contains("from=2026-10-10T12%3A00%3A00Z");
                    let (status, payload) = if ok { ("200 OK", body) } else { ("404 Not Found", "{}".into()) };
                    let reply = format!("HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{payload}", payload.len());
                    let _ = s.write_all(reply.as_bytes()).await;
                });
            }
        });
        let client = reqwest::Client::builder().timeout(Duration::from_millis(800)).build().unwrap();
        let found = probe(&client, port, "claude", "2026-10-10T12:00:00Z").await.expect("the gateway answers");
        assert_eq!((found.port, found.client.as_str(), found.measured.llm_calls), (port, "claude", 3));
        // Port 1 has nothing listening.
        assert!(probe(&client, 1, "default", "2026-10-10T12:00:00Z").await.is_none());
    }
}
