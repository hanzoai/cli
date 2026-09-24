//! The console's home: compute, the way sparkDash shows a DGX Spark.
//!
//! One data model for every machine on the page. This machine is read locally
//! every two seconds by a [`Sampler`]; its siblings are the org's run-targets
//! (`GET /v1/agent/targets`), each carrying the SAME `spec` + `metrics` its own
//! heartbeat sent — the rows platform.hanzo.ai draws. Networks are the org's
//! zero-trust overlay (`GET /v1/network`); accounts are the identities this CLI
//! holds. While the console is open the machine beats, so it is on the platform
//! too.
//!
//! Graphs follow sparkDash: the last 30 samples, GPU util and temperature, CPU
//! temperature, memory, decode and prefill tokens/s. A reading nobody gave is
//! `—`, never 0.

use std::collections::{HashMap, VecDeque};
use std::sync::mpsc::{channel, Receiver};
use std::time::Duration;

use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Paragraph, Sparkline},
    Frame,
};
use serde::Deserialize;

use crate::commands::code::context::{self, Machine, Metrics, Spec};
use crate::commands::code::sample::Sampler;
use crate::commands::code::target;
use crate::commands::product::Seam;
use crate::commands::{network, status};
use crate::config::Config;
use crate::iam::{paths, store};

/// How often this machine is read.
const TICK: Duration = Duration::from_secs(2);
/// How often the siblings are read; the networks every third time.
const FLEET: Duration = Duration::from_secs(10);
const NET_EVERY: u64 = 3;
/// How long one cloud read may take before the page says so.
const PATIENCE: Duration = Duration::from_secs(10);
/// Points kept per series: twenty minutes of this machine at one every two seconds.
const POINTS: usize = 600;
/// Points an inline sparkline draws.
const TAIL: usize = 16;

/// A run-target as `GET /v1/agent/targets` answers it.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Target {
    pub id: String,
    pub label: String,
    pub kind: String,
    pub status: String,
    pub host: String,
    pub spec: Spec,
    pub metrics: Metrics,
    pub metrics_at: Option<String>,
}

/// One zero-trust network as `GET /v1/network` answers it.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Network {
    pub id: String,
    pub name: String,
    pub status: String,
    pub nodes: i64,
}

/// What the background reader sends the page.
pub enum Update {
    Local(Box<Machine>),
    Fleet(Result<Vec<Target>, String>),
    Networks(Result<Vec<Network>, String>),
    Accounts { active: Option<String>, all: Vec<String>, api: String },
}

/// One machine on the page: this one, or a sibling.
#[derive(Debug, Clone, Default)]
pub struct Unit {
    pub key: String,
    pub label: String,
    pub local: bool,
    pub online: bool,
    /// Unix seconds of the reading; `None` for this machine (it is now).
    pub at: Option<i64>,
    pub spec: Spec,
    pub metrics: Metrics,
}

/// The series sparkDash draws, newest last.
#[derive(Debug, Clone, Default)]
pub struct History {
    last: Option<i64>,
    pub gpu: VecDeque<f64>,
    pub cpu: VecDeque<f64>,
    pub gpu_temp: VecDeque<f64>,
    pub cpu_temp: VecDeque<f64>,
    pub mem: VecDeque<f64>,
    pub decode: VecDeque<f64>,
    pub prefill: VecDeque<f64>,
}

impl History {
    fn push(&mut self, spec: &Spec, m: &Metrics) {
        let put = |q: &mut VecDeque<f64>, v: Option<f64>| {
            if let Some(v) = v.filter(|v| v.is_finite()) {
                q.push_back(v);
                while q.len() > POINTS {
                    q.pop_front();
                }
            }
        };
        put(&mut self.gpu, (!spec.gpus.is_empty()).then_some(m.gpu_util * 100.0));
        put(&mut self.cpu, m.cpu_util.map(|c| c * 100.0));
        put(&mut self.gpu_temp, m.gpu_temp);
        put(&mut self.cpu_temp, m.cpu_temp);
        put(&mut self.mem, mem_pct(spec, m));
        put(&mut self.decode, m.decode);
        put(&mut self.prefill, m.prefill);
    }
}

/// Where a cloud read stands.
#[derive(Debug, Clone, Default, PartialEq)]
pub enum Reading {
    #[default]
    Pending,
    Ok,
    Failed(String),
}

/// The compute page's state.
#[derive(Default)]
pub struct Board {
    pub host: String,
    pub local: Option<Machine>,
    pub targets: Vec<Target>,
    pub fleet: Reading,
    pub networks: Vec<Network>,
    pub net: Reading,
    pub account: Option<String>,
    pub accounts: Vec<String>,
    pub api: String,
    pub history: HashMap<String, History>,
    pub selected: usize,
    rx: Option<Receiver<Update>>,
}

impl Board {
    /// A board fed by a background reader on the current tokio runtime. Without a
    /// runtime (a unit test) it is a board nobody feeds.
    pub fn start() -> Board {
        let (tx, rx) = channel();
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            let local = tx.clone();
            handle.spawn(async move {
                let mut sampler = Sampler::new();
                while local.send(Update::Local(Box::new(sampler.machine().await))).is_ok() {
                    tokio::time::sleep(TICK).await;
                }
            });
            handle.spawn(async move {
                let mut cfg = Config::load(None).unwrap_or_default();
                let held = store::list(&cfg, paths::DEFAULT_BRAND);
                let active = store::active(&cfg, paths::DEFAULT_BRAND);
                let api = network::active(&cfg).api.trim_end_matches('/').to_string();
                let _ = tx.send(Update::Accounts {
                    active: active.as_ref().map(|i| format!("{}/{}", i.owner, i.name)),
                    all: held.iter().map(|i| format!("{}/{}", i.owner, i.name)).collect(),
                    api: api.clone(),
                });
                let _beat = active
                    .is_some()
                    .then(|| target::beat(&cfg, &api, &context::machine_id(), &context::hostname()));
                for tick in 0u64.. {
                    if tick % NET_EVERY == 0 && tx.send(Update::Networks(networks(&mut cfg).await)).is_err() {
                        break;
                    }
                    if tx.send(Update::Fleet(fleet(&mut cfg).await)).is_err() {
                        break;
                    }
                    tokio::time::sleep(FLEET).await;
                }
            });
        }
        Board { host: context::hostname(), rx: Some(rx), ..Default::default() }
    }

    /// Take everything the reader has sent since the last frame.
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
            Update::Local(m) => {
                self.history.entry("local".into()).or_default().push(&m.spec, &m.metrics);
                self.local = Some(*m);
            }
            Update::Fleet(Ok(targets)) => {
                for t in &targets {
                    let at = seen(t);
                    let h = self.history.entry(t.id.clone()).or_default();
                    if at.is_some() && at != h.last {
                        h.last = at;
                        h.push(&t.spec, &t.metrics);
                    }
                }
                self.targets = targets;
                self.fleet = Reading::Ok;
            }
            Update::Fleet(Err(e)) => self.fleet = Reading::Failed(e),
            Update::Networks(Ok(n)) => {
                self.networks = n;
                self.net = Reading::Ok;
            }
            Update::Networks(Err(e)) => self.net = Reading::Failed(e),
            Update::Accounts { active, all, api } => {
                self.account = active;
                self.accounts = all;
                self.api = api;
            }
        }
        self.selected = self.selected.min(self.units().len().saturating_sub(1));
    }

    /// Every machine, this one first: its own live reading stands in for its
    /// target row, then the online siblings, GPU machines before the rest.
    pub fn units(&self) -> Vec<Unit> {
        let mut out = Vec::new();
        let mine = self.targets.iter().find(|t| t.host == self.host);
        if let Some(m) = &self.local {
            out.push(Unit {
                key: "local".into(),
                label: self.host.clone(),
                local: true,
                online: true,
                at: None,
                spec: m.spec.clone(),
                metrics: m.metrics.clone(),
            });
        }
        // One row per machine: a host re-registered under a second target keeps
        // only its newest reading.
        let mut newest: HashMap<&str, &Target> = HashMap::new();
        for t in &self.targets {
            let key = if t.host.is_empty() { t.id.as_str() } else { t.host.as_str() };
            if newest.get(key).is_none_or(|n| seen(n) < seen(t)) {
                newest.insert(key, t);
            }
        }
        let mut rest: Vec<Unit> = newest
            .into_values()
            .filter(|t| self.local.is_none() || mine.map(|m| m.host.as_str()) != Some(t.host.as_str()))
            .map(|t| Unit {
                key: t.id.clone(),
                label: if t.label.is_empty() { t.host.clone() } else { t.label.clone() },
                local: false,
                online: t.status == "online",
                at: seen(t),
                spec: t.spec.clone(),
                metrics: t.metrics.clone(),
            })
            .collect();
        rest.sort_by(|a, b| {
            (!a.online, a.spec.gpus.is_empty(), a.label.as_str(), std::cmp::Reverse(a.at)).cmp(&(
                !b.online,
                b.spec.gpus.is_empty(),
                b.label.as_str(),
                std::cmp::Reverse(b.at),
            ))
        });
        out.extend(rest);
        out
    }

    pub fn next(&mut self) {
        let n = self.units().len();
        if n > 0 {
            self.selected = (self.selected + 1) % n;
        }
    }

    pub fn previous(&mut self) {
        let n = self.units().len();
        if n > 0 {
            self.selected = (self.selected + n - 1) % n;
        }
    }
}

/// When a target's reading was taken, from the server-stamped `metricsAt`.
fn seen(t: &Target) -> Option<i64> {
    t.metrics_at
        .as_deref()
        .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
        .map(|d| d.timestamp())
}

async fn fleet(cfg: &mut Config) -> Result<Vec<Target>, String> {
    let rows = read(cfg, "/v1/agent/targets", "targets").await?;
    Ok(rows.into_iter().filter_map(|v| serde_json::from_value(v).ok()).collect())
}

async fn networks(cfg: &mut Config) -> Result<Vec<Network>, String> {
    let rows = read(cfg, "/v1/network", "networks").await?;
    Ok(rows.into_iter().filter_map(|v| serde_json::from_value(v).ok()).collect())
}

/// One list off the cloud through the one authenticated seam `hanzo status` reads by.
async fn read(cfg: &mut Config, path: &str, key: &str) -> Result<Vec<serde_json::Value>, String> {
    let call = async {
        let seam = Seam::open(cfg).await.map_err(|e| format!("{e:#}"))?;
        status::read(&seam, path, key).await
    };
    tokio::time::timeout(PATIENCE, call)
        .await
        .unwrap_or_else(|_| Err(format!("no answer in {}s", PATIENCE.as_secs())))
}

// ---- values -----------------------------------------------------------------

/// Memory in use as a percentage of the machine's RAM.
fn mem_pct(spec: &Spec, m: &Metrics) -> Option<f64> {
    (spec.memory > 0 && m.mem_used > 0).then(|| m.mem_used as f64 * 100.0 / spec.memory as f64)
}

/// Whether the GPU shares system memory: it has GPUs and no dedicated VRAM.
pub fn unified(spec: &Spec, m: &Metrics) -> bool {
    !spec.gpus.is_empty() && m.gpu_mem_total.is_none()
}

pub fn gib(bytes: i64) -> String {
    let g = bytes as f64 / (1u64 << 30) as f64;
    if g >= 1024.0 {
        format!("{:.1} TiB", g / 1024.0)
    } else {
        format!("{g:.1} GiB")
    }
}

/// GiB as a bare number, for a list whose last entry names the unit.
fn num(bytes: i64) -> String {
    format!("{:.1}", bytes as f64 / (1u64 << 30) as f64)
}

pub fn rate(bps: f64) -> String {
    const UNITS: [&str; 4] = ["B/s", "KiB/s", "MiB/s", "GiB/s"];
    let mut v = bps;
    let mut i = 0;
    while v >= 1024.0 && i < UNITS.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{v:.0} {}", UNITS[i])
    } else {
        format!("{v:.1} {}", UNITS[i])
    }
}

pub fn tokens(t: f64) -> String {
    if t >= 1000.0 {
        format!("{:.1}k", t / 1000.0)
    } else {
        format!("{t:.1}")
    }
}

pub fn seconds(s: f64) -> String {
    if s < 1.0 {
        format!("{:.0} ms", s * 1000.0)
    } else {
        format!("{s:.2} s")
    }
}

pub fn age(secs: i64) -> String {
    match secs.max(0) {
        s if s < 90 => "now".into(),
        s if s < 3600 => format!("{}m", s / 60),
        s if s < 172_800 => format!("{}h", s / 3600),
        s => format!("{}d", s / 86_400),
    }
}

/// The accelerator a row is named by: "GB10", "2× RTX 4090".
fn gpu_name(spec: &Spec) -> String {
    match spec.gpus.first() {
        None => "—".into(),
        Some(g) => {
            let model = product(&g.model);
            let name = if model.is_empty() { g.vendor.clone() } else { model };
            if spec.gpus.len() > 1 {
                format!("{}× {name}", spec.gpus.len())
            } else {
                name
            }
        }
    }
}

/// The product in an lspci name: "Advanced Micro Devices, Inc. [AMD/ATI] Strix
/// [Radeon 8060S]" is a "Radeon 8060S", and "… [AMD/ATI] Device 1586" an
/// "AMD/ATI Device 1586". A plain name ("GB10") is itself.
fn product(model: &str) -> String {
    let m = model.trim();
    if let Some(inner) = m.strip_suffix(']').and_then(|h| h.rsplit_once('[')).map(|(_, p)| p) {
        return inner.trim().to_string();
    }
    match m.split_once('[') {
        Some((_, rest)) => rest.replace(']', "").split_whitespace().collect::<Vec<_>>().join(" "),
        None => m.to_string(),
    }
}

/// A sparkline of the last `width` values, scaled to `max` (or the series' own
/// peak), right-aligned. Under two points there is no line yet.
pub fn spark(values: &VecDeque<f64>, width: usize, max: Option<f64>) -> String {
    const TICKS: [char; 8] = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
    if values.len() < 2 {
        return " ".repeat(width);
    }
    let tail: Vec<f64> = values.iter().skip(values.len().saturating_sub(width)).copied().collect();
    let top = max.unwrap_or_else(|| tail.iter().copied().fold(0.0, f64::max)).max(f64::EPSILON);
    let line: String = tail
        .iter()
        .map(|v| TICKS[((v / top).clamp(0.0, 1.0) * 7.0).round() as usize])
        .collect();
    format!("{}{line}", " ".repeat(width - tail.len()))
}

pub fn bar(frac: f64, width: usize) -> String {
    let filled = ((frac.clamp(0.0, 1.0)) * width as f64).round() as usize;
    format!("{}{}", "█".repeat(filled), "░".repeat(width - filled))
}

// ---- colors (sparkDash's bands) ---------------------------------------------

const DIM: Color = Color::Rgb(140, 145, 155);
const ACCENT: Color = Color::Cyan;

/// Bars: warning above 60%, danger above 85%.
pub fn load_color(pct: f64) -> Color {
    if pct > 85.0 {
        Color::Red
    } else if pct > 60.0 {
        Color::Yellow
    } else {
        Color::Green
    }
}

/// GPU temperature: warning above 65 °C, danger above 85 °C.
pub fn gpu_temp_color(t: f64) -> Color {
    if t > 85.0 {
        Color::Red
    } else if t > 65.0 {
        Color::Yellow
    } else {
        ACCENT
    }
}

/// CPU temperature: warning above 85 °C, danger above 95 °C.
pub fn cpu_temp_color(t: f64) -> Color {
    if t > 95.0 {
        Color::Red
    } else if t > 85.0 {
        Color::Yellow
    } else {
        ACCENT
    }
}

fn label(s: &str) -> Span<'static> {
    Span::styled(format!("{s:<7}"), Style::default().fg(Color::White).add_modifier(Modifier::BOLD))
}

fn dim(s: impl Into<String>) -> Span<'static> {
    Span::styled(s.into(), Style::default().fg(DIM))
}

fn val(s: impl Into<String>, c: Color) -> Span<'static> {
    Span::styled(s.into(), Style::default().fg(c).add_modifier(Modifier::BOLD))
}

fn none() -> Span<'static> {
    dim("—")
}

// ---- rendering --------------------------------------------------------------

pub fn render(f: &mut Frame, area: Rect, board: &Board) {
    let units = board.units();
    // The table keeps a row per machine; the charts take what is left, 7 to 16 rows.
    let table = units.len() as u16 + 4;
    let charts = match area.height.saturating_sub(9 + table.max(6)) {
        h if h < 7 => 0,
        h => h.min(16),
    };
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(9), Constraint::Length(charts), Constraint::Min(5)])
        .split(area);
    match units.get(board.selected) {
        Some(u) => {
            let empty = History::default();
            let h = board.history.get(&u.key).unwrap_or(&empty);
            render_unit(f, rows[0], u, h);
            if charts > 0 {
                render_charts(f, rows[1], u, h);
            }
        }
        None => {
            let block = panel(" this machine ".into());
            f.render_widget(Paragraph::new(dim("  reading this machine…")).block(block), rows[0]);
        }
    }
    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(68), Constraint::Percentage(32)])
        .split(rows[2]);
    render_machines(f, cols[0], board, &units);
    render_side(f, cols[1], board);
}

fn panel(title: String) -> Block<'static> {
    Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(Color::DarkGray))
        .title(Span::styled(title, Style::default().fg(Color::White).add_modifier(Modifier::BOLD)))
}

fn render_unit(f: &mut Frame, area: Rect, u: &Unit, h: &History) {
    let s = &u.spec;
    let m = &u.metrics;
    let mut title = format!(" {} · {}", u.label, gpu_name(s));
    if !s.os.is_empty() {
        title.push_str(&format!(" · {}/{}", s.os, s.arch));
    }
    if s.cpus > 0 {
        title.push_str(&format!(" · {} cpu", s.cpus));
    }
    if s.memory > 0 {
        title.push_str(&format!(" · {}", gib(s.memory)));
    }
    title.push(' ');
    let state = match (u.local, u.online, u.at) {
        (true, _, _) => Span::styled(" ● this machine · live ", Style::default().fg(Color::Green)),
        (false, true, _) => Span::styled(" ● online ", Style::default().fg(Color::Green)),
        (false, false, Some(at)) => dim(format!(" ○ offline · seen {} ago ", age(now() - at))),
        (false, false, None) => dim(" ○ offline "),
    };
    let block = panel(title).title(Line::from(state).right_aligned());
    let inner = block.inner(area);
    f.render_widget(block, area);

    let halves = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(Rect { height: inner.height.min(5), ..inner });
    let w = TAIL;

    let mut left = Vec::new();
    if s.gpus.is_empty() {
        left.push(Line::from(vec![label("GPU"), none()]));
        left.push(Line::from(""));
    } else {
        let util = m.gpu_util * 100.0;
        left.push(Line::from(vec![
            label("GPU"),
            dim("util "),
            Span::styled(spark(&h.gpu, w, Some(100.0)), Style::default().fg(load_color(util))),
            val(format!(" {util:>3.0}%"), load_color(util)),
        ]));
        let mut temp = vec![label(""), dim("temp ")];
        match m.gpu_temp {
            Some(t) => {
                temp.push(Span::styled(spark(&h.gpu_temp, w, Some(100.0)), Style::default().fg(gpu_temp_color(t))));
                temp.push(val(format!(" {t:>3.0}°C"), gpu_temp_color(t)));
            }
            None => temp.push(none()),
        }
        if let Some(p) = m.gpu_power {
            temp.push(dim("  power "));
            temp.push(val(format!("{p:.1} W"), Color::White));
            temp.push(dim(m.gpu_power_limit.map(|l| format!(" / {l:.0} W")).unwrap_or_default()));
        }
        left.push(Line::from(temp));
    }
    let mut cpu = vec![label("CPU")];
    match m.cpu_util {
        Some(c) => {
            let pct = c * 100.0;
            cpu.push(dim("util "));
            cpu.push(val(format!("{pct:>3.0}%"), load_color(pct)));
        }
        None => cpu.push(none()),
    }
    cpu.push(dim(format!("   load {:.1}", m.load1)));
    if s.cpus > 0 {
        cpu.push(dim(format!(" / {}", s.cpus)));
    }
    left.push(Line::from(cpu));
    let mut ct = vec![label(""), dim("temp ")];
    match m.cpu_temp {
        Some(t) => {
            ct.push(Span::styled(spark(&h.cpu_temp, w, Some(100.0)), Style::default().fg(cpu_temp_color(t))));
            ct.push(val(format!(" {t:>3.0}°C"), cpu_temp_color(t)));
        }
        None => ct.push(none()),
    }
    left.push(Line::from(ct));
    left.push(match (m.net_rx, m.net_tx) {
        (Some(rx), Some(tx)) => Line::from(vec![
            label("NET"),
            dim("↓ "),
            val(rate(rx), Color::White),
            dim("   ↑ "),
            val(rate(tx), Color::White),
        ]),
        _ => Line::from(vec![label("NET"), none()]),
    });
    f.render_widget(Paragraph::new(left), halves[0]);

    let mut right = Vec::new();
    let used_frac = |used: i64, total: i64| if total > 0 { used as f64 / total as f64 } else { 0.0 };
    if s.memory > 0 && m.mem_used > 0 {
        let pct = used_frac(m.mem_used, s.memory) * 100.0;
        if unified(s, m) {
            right.push(Line::from(vec![
                label("MEMORY"),
                dim("unified "),
                Span::styled(bar(pct / 100.0, 14), Style::default().fg(load_color(pct))),
                val(format!(" {pct:.0}%"), load_color(pct)),
                dim("  "),
                Span::styled(spark(&h.mem, 10, Some(100.0)), Style::default().fg(DIM)),
            ]));
            let gpu = m.gpu_mem_used.unwrap_or(0);
            right.push(Line::from(vec![
                label(""),
                val(format!("{} / {}", gib(m.mem_used), gib(s.memory)), Color::White),
                dim(format!("  gpu {} · cpu {} · avail {}", num(gpu), num((m.mem_used - gpu).max(0)), gib(m.mem_free))),
            ]));
        } else {
            right.push(Line::from(vec![
                label("RAM"),
                Span::styled(bar(pct / 100.0, 14), Style::default().fg(load_color(pct))),
                val(format!(" {pct:.0}%"), load_color(pct)),
                dim(format!("  {} / {}", gib(m.mem_used), gib(s.memory))),
            ]));
            right.push(match (m.gpu_mem_used, m.gpu_mem_total) {
                (Some(used), Some(total)) => {
                    let vp = used_frac(used, total) * 100.0;
                    Line::from(vec![
                        label("VRAM"),
                        Span::styled(bar(vp / 100.0, 14), Style::default().fg(load_color(vp))),
                        val(format!(" {vp:.0}%"), load_color(vp)),
                        dim(format!("  {} / {}", gib(used), gib(total))),
                    ])
                }
                _ => Line::from(vec![label("VRAM"), none()]),
            });
        }
    } else {
        right.push(Line::from(vec![label("MEMORY"), none()]));
        right.push(Line::from(""));
    }
    match (m.disk_used, m.disk_total) {
        (Some(used), Some(total)) if total > 0 => {
            let pct = used_frac(used, total) * 100.0;
            right.push(Line::from(vec![
                label("DISK"),
                Span::styled(bar(pct / 100.0, 14), Style::default().fg(load_color(pct))),
                val(format!(" {pct:.0}%"), load_color(pct)),
                dim(format!("  {} / {}", gib(used), gib(total))),
            ]));
        }
        _ => right.push(Line::from(vec![label("DISK"), none()])),
    }
    right.push(match (m.disk_read, m.disk_write) {
        (Some(r), Some(w)) => Line::from(vec![label(""), dim("read "), val(rate(r), Color::White), dim("   write "), val(rate(w), Color::White)]),
        _ => Line::from(""),
    });
    f.render_widget(Paragraph::new(right), halves[1]);

    let serve_area = Rect { y: inner.y + 5, height: inner.height.saturating_sub(5), ..inner };
    let serve = match &m.model {
        Some(model) => {
            let rate = |v: Option<f64>| v.map(|v| format!("{} tok/s", tokens(v))).unwrap_or_else(|| "—".into());
            vec![
                Line::from(vec![
                    label("SERVE"),
                    val(model.clone(), ACCENT),
                    dim("   decode "),
                    Span::styled(spark(&h.decode, w, None), Style::default().fg(ACCENT)),
                    val(rate(m.decode), Color::White),
                    dim("   prefill "),
                    Span::styled(spark(&h.prefill, 10, None), Style::default().fg(DIM)),
                    val(rate(m.prefill), Color::White),
                ]),
                Line::from(vec![
                    label(""),
                    dim("ttft "),
                    m.ttft.map(|t| val(seconds(t), Color::White)).unwrap_or_else(none),
                    dim("   run "),
                    m.running.map(|r| val(r.to_string(), Color::White)).unwrap_or_else(none),
                    dim(" · wait "),
                    m.waiting.map(|r| val(r.to_string(), Color::White)).unwrap_or_else(none),
                    dim(" · kv "),
                    m.kv_cache
                        .map(|k| val(format!("{:.1}%", k * 100.0), if k >= 0.8 { Color::Red } else if k >= 0.5 { Color::Yellow } else { Color::Green }))
                        .unwrap_or_else(none),
                ]),
            ]
        }
        None => vec![Line::from(vec![label("SERVE"), dim("no model server answering /metrics")])],
    };
    f.render_widget(Paragraph::new(serve), serve_area);
}

/// The history row: sparkDash's area charts over everything kept, newest at the
/// right edge. A machine without a GPU charts its CPU instead.
fn render_charts(f: &mut Frame, area: Rect, u: &Unit, h: &History) {
    let m = &u.metrics;
    let pct = |v: Option<f64>| v.map(|v| format!("{v:.0}%")).unwrap_or_else(|| "—".into());
    let deg = |v: Option<f64>| v.map(|v| format!("{v:.0}°C")).unwrap_or_else(|| "—".into());
    let gpu = !u.spec.gpus.is_empty();
    let cells: [(String, &VecDeque<f64>, Option<f64>, Color); 4] = [
        if gpu {
            (format!(" GPU util  {} ", pct(Some(m.gpu_util * 100.0))), &h.gpu, Some(100.0), load_color(m.gpu_util * 100.0))
        } else {
            (format!(" CPU util  {} ", pct(m.cpu_util.map(|c| c * 100.0))), &h.cpu, Some(100.0), ACCENT)
        },
        if gpu {
            (format!(" GPU temp  {} ", deg(m.gpu_temp)), &h.gpu_temp, Some(100.0), m.gpu_temp.map(gpu_temp_color).unwrap_or(DIM))
        } else {
            (format!(" CPU temp  {} ", deg(m.cpu_temp)), &h.cpu_temp, Some(100.0), m.cpu_temp.map(cpu_temp_color).unwrap_or(DIM))
        },
        (format!(" memory  {} ", pct(mem_pct(&u.spec, m))), &h.mem, Some(100.0), mem_pct(&u.spec, m).map(load_color).unwrap_or(DIM)),
        (
            format!(" decode  {} ", m.decode.map(|d| format!("{} tok/s", tokens(d))).unwrap_or_else(|| "—".into())),
            &h.decode,
            None,
            ACCENT,
        ),
    ];
    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Ratio(1, 4); 4])
        .split(area);
    for (i, (title, series, max, color)) in cells.into_iter().enumerate() {
        let block = panel(title);
        let inner = block.inner(cols[i]);
        f.render_widget(block, cols[i]);
        let width = inner.width as usize;
        let tail: Vec<f64> = series.iter().skip(series.len().saturating_sub(width)).copied().collect();
        let top = max.unwrap_or_else(|| tail.iter().copied().fold(0.0, f64::max)).max(1.0);
        let mut data = vec![0u64; width - tail.len()];
        data.extend(tail.iter().map(|v| (v.clamp(0.0, top) * 100.0 / top).round() as u64));
        f.render_widget(Sparkline::default().data(&data).max(100).style(Style::default().fg(color)), inner);
    }
}

fn render_machines(f: &mut Frame, area: Rect, board: &Board, units: &[Unit]) {
    let title = match &board.account {
        Some(a) => format!(" machines · {a} "),
        None => " machines ".into(),
    };
    let block = panel(title);
    // The model column goes first when the panel is narrow, then the GPU name shrinks.
    let inner = area.width.saturating_sub(2) as usize;
    let wide = inner >= 93;
    let gw = if inner >= 71 || wide { 18 } else { 12 };
    let row = |name: &str, gpu: &str, util: &str, temp: &str, mem: &str, tps: &str, model: &str, when: &str| {
        let mut s = format!("{name:<14}{gpu:<gw$}{util:>6}{temp:>7}{mem:>6}{tps:>9}");
        if wide {
            s.push_str(&format!("  {model:<22}"));
        }
        s.push_str(&format!("{when:>5}"));
        s
    };
    let head = Line::from(Span::styled(
        format!("  {}", row("MACHINE", "GPU", "UTIL", "TEMP", "MEM", "TOK/S", "MODEL", "SEEN")),
        Style::default().fg(Color::DarkGray).add_modifier(Modifier::BOLD),
    ));
    let mut lines = vec![head];
    for (i, u) in units.iter().enumerate() {
        let m = &u.metrics;
        let fresh = u.local || u.online;
        let dot = if fresh { Span::styled("● ", Style::default().fg(Color::Green)) } else { dim("○ ") };
        let util = if u.spec.gpus.is_empty() || !fresh { "—".into() } else { format!("{:.0}%", m.gpu_util * 100.0) };
        let temp = m.gpu_temp.filter(|_| fresh).map(|t| format!("{t:.0}°C")).unwrap_or_else(|| "—".into());
        let mem = mem_pct(&u.spec, m).filter(|_| fresh).map(|p| format!("{p:.0}%")).unwrap_or_else(|| "—".into());
        let tps = m.decode.filter(|_| fresh).map(tokens).unwrap_or_else(|| "—".into());
        let model: String = m.model.clone().unwrap_or_else(|| "—".into()).chars().take(21).collect();
        let when = if u.local { "live".into() } else { u.at.map(|a| age(now() - a)).unwrap_or_else(|| "—".into()) };
        let name: String = u.label.chars().take(13).collect();
        let gpu: String = gpu_name(&u.spec).chars().take(gw - 1).collect();
        let text = row(&name, &gpu, &util, &temp, &mem, &tps, &model, &when);
        let mut style = Style::default().fg(if fresh { Color::White } else { Color::DarkGray });
        if i == board.selected {
            style = style.bg(Color::Rgb(40, 44, 58)).add_modifier(Modifier::BOLD);
        }
        lines.push(Line::from(vec![dot, Span::styled(text, style)]));
    }
    match &board.fleet {
        Reading::Pending => lines.push(Line::from(dim("  reading the org's machines…"))),
        Reading::Failed(e) => lines.push(Line::from(vec![Span::styled("  cloud: ", Style::default().fg(Color::Yellow)), dim(e.clone())])),
        Reading::Ok if board.targets.is_empty() => lines.push(Line::from(dim("  no other machines linked — `hanzo link` on one"))),
        Reading::Ok => {}
    }
    f.render_widget(Paragraph::new(lines).block(block), area);
}

fn render_side(f: &mut Frame, area: Rect, board: &Board) {
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(3), Constraint::Length(board.accounts.len().max(1) as u16 + 2)])
        .split(area);

    let mut nets = Vec::new();
    match &board.net {
        Reading::Pending => nets.push(Line::from(dim(" reading…"))),
        Reading::Failed(e) => nets.push(Line::from(dim(format!(" {e}")))),
        Reading::Ok if board.networks.is_empty() => nets.push(Line::from(dim(" none — `hanzo link`"))),
        Reading::Ok => {
            for n in &board.networks {
                let up = matches!(n.status.as_str(), "active" | "online" | "up" | "ready");
                nets.push(Line::from(vec![
                    if up { Span::styled(" ● ", Style::default().fg(Color::Green)) } else { dim(" ○ ") },
                    val(if n.name.is_empty() { n.id.clone() } else { n.name.clone() }, Color::White),
                    dim(format!("  {} · {} nodes", n.status, n.nodes)),
                ]));
            }
        }
    }
    f.render_widget(Paragraph::new(nets).block(panel(" networks · zero trust ".into())), rows[0]);

    let mut accts = Vec::new();
    if board.accounts.is_empty() {
        accts.push(Line::from(dim(" not signed in — `hanzo auth login`")));
    }
    for a in &board.accounts {
        let active = board.account.as_deref() == Some(a.as_str());
        accts.push(Line::from(vec![
            if active { Span::styled(" * ", Style::default().fg(ACCENT)) } else { dim("   ") },
            val(a.clone(), if active { Color::White } else { DIM }),
        ]));
    }
    f.render_widget(Paragraph::new(accts).block(panel(" accounts ".into())), rows[1]);
}

fn now() -> i64 {
    chrono::Utc::now().timestamp()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::code::context::Gpu;
    use ratatui::{backend::TestBackend, Terminal};

    fn spark_machine() -> Machine {
        Machine {
            spec: Spec {
                os: "linux".into(),
                arch: "aarch64".into(),
                cpus: 20,
                memory: 124610 * 1048576,
                gpus: vec![Gpu { vendor: "nvidia".into(), model: "GB10".into(), memory: 0 }],
            },
            metrics: Metrics {
                load1: 3.2,
                mem_used: 92096 * 1048576,
                mem_free: 32514 * 1048576,
                gpu_util: 0.15,
                cpu_util: Some(0.56),
                cpu_temp: Some(80.3),
                gpu_temp: Some(54.0),
                gpu_power: Some(16.5),
                gpu_mem_used: Some(73085 * 1048576),
                disk_used: Some(3033975 * 1048576),
                disk_total: Some(3649741 * 1048576),
                disk_read: Some(149_755_753.0),
                disk_write: Some(70_211_646.0),
                net_rx: Some(773_675.0),
                net_tx: Some(143_806.0),
                model: Some("qwen3.8-flash-next".into()),
                decode: Some(41.2),
                prefill: Some(1200.0),
                ttft: Some(0.21),
                running: Some(1),
                waiting: Some(0),
                kv_cache: Some(0.12),
                ..Default::default()
            },
        }
    }

    fn target(id: &str, host: &str, status: &str, at: &str) -> Target {
        let mut t: Target = serde_json::from_value(serde_json::json!({
            "id": id, "label": host, "kind": "gpu", "status": status, "host": host,
            "spec": {"os": "linux", "arch": "amd64", "cpus": 32, "memory": 68719476736i64,
                     "gpus": [{"vendor": "nvidia", "model": "RTX 4090", "memory": 25757220864i64}]},
            "metrics": {"load1": 1.0, "memUsed": 34359738368i64, "gpuUtil": 0.5, "gpuTemp": 61.0,
                        "gpuMemUsed": 12884901888i64, "gpuMemTotal": 25757220864i64, "at": 1},
            "metricsAt": at, "sessions": 0, "running": 0
        }))
        .unwrap();
        t.label = host.into();
        t
    }

    #[test]
    fn this_machine_leads_and_replaces_its_own_target() {
        let mut b = Board { host: "dgx".into(), ..Default::default() };
        b.apply(Update::Local(Box::new(spark_machine())));
        b.apply(Update::Fleet(Ok(vec![
            target("tgt_old", "evo", "offline", "2026-09-08T04:51:59Z"),
            target("tgt_older", "evo", "offline", "2026-08-01T00:00:00Z"),
            target("tgt_me", "dgx", "online", "2026-09-23T07:00:00Z"),
            target("tgt_live", "rig", "online", "2026-09-23T07:00:00Z"),
        ])));
        let units = b.units();
        let names: Vec<&str> = units.iter().map(|u| u.label.as_str()).collect();
        assert_eq!(names, ["dgx", "rig", "evo"], "local first, online before offline, own target folded in");
        assert!(units[0].local && units[0].metrics.model.is_some());
        assert!(!units[2].online);
        assert_eq!(units[2].key, "tgt_old", "a re-registered host shows its newest target");
    }

    #[test]
    fn history_takes_a_sibling_reading_once() {
        let mut b = Board::default();
        let t = target("tgt_1", "rig", "online", "2026-09-23T07:00:00Z");
        b.apply(Update::Fleet(Ok(vec![t.clone()])));
        b.apply(Update::Fleet(Ok(vec![t.clone()])));
        assert_eq!(b.history["tgt_1"].gpu.len(), 1, "the same reading is not a new point");
        let mut later = t;
        later.metrics_at = Some("2026-09-23T07:00:30Z".into());
        b.apply(Update::Fleet(Ok(vec![later])));
        assert_eq!(b.history["tgt_1"].gpu.len(), 2);
        assert_eq!(b.history["tgt_1"].cpu_temp.len(), 0, "an absent reading is no point, not a zero");
    }

    #[test]
    fn memory_mode_follows_dedicated_vram() {
        let m = spark_machine();
        assert!(unified(&m.spec, &m.metrics));
        let t = target("t", "rig", "online", "2026-09-23T07:00:00Z");
        assert!(!unified(&t.spec, &t.metrics));
        assert!(!unified(&Spec::default(), &Metrics::default()), "no GPU is not unified memory");
    }

    #[test]
    fn values_read_like_sparkdash() {
        assert_eq!(gib(92096 * 1048576), "89.9 GiB");
        assert_eq!(gib(3649741 * 1048576), "3.5 TiB");
        assert_eq!(rate(773_675.0), "755.5 KiB/s");
        assert_eq!(rate(12.0), "12 B/s");
        assert_eq!(tokens(41.23), "41.2");
        assert_eq!(tokens(1234.0), "1.2k");
        assert_eq!(seconds(0.21), "210 ms");
        assert_eq!(seconds(2.5), "2.50 s");
        assert_eq!(age(30), "now");
        assert_eq!(age(15 * 86_400), "15d");
        let q: VecDeque<f64> = [0.0, 50.0, 100.0].into_iter().collect();
        assert_eq!(spark(&q, 5, Some(100.0)), "  ▁▅█");
        assert_eq!(spark(&VecDeque::from([7.0]), 3, None), "   ", "one point is no line");
        assert_eq!(bar(0.5, 4), "██░░");
        assert_eq!((gpu_temp_color(70.0), cpu_temp_color(70.0)), (Color::Yellow, ACCENT));
        assert_eq!(load_color(90.0), Color::Red);
        assert_eq!(product("Advanced Micro Devices, Inc. [AMD/ATI] Strix [Radeon 8060S]"), "Radeon 8060S");
        assert_eq!(product("Advanced Micro Devices, Inc. [AMD/ATI] Device 1586"), "AMD/ATI Device 1586");
        assert_eq!(product("GB10"), "GB10");
    }

    #[test]
    fn renders_the_home_page() {
        let mut b = Board { host: "dgx".into(), ..Default::default() };
        for _ in 0..3 {
            b.apply(Update::Local(Box::new(spark_machine())));
        }
        b.apply(Update::Fleet(Ok(vec![target("tgt_old", "evo", "offline", "2026-09-08T04:51:59Z")])));
        b.apply(Update::Networks(Ok(vec![])));
        b.apply(Update::Accounts { active: Some("hanzo/z".into()), all: vec!["hanzo/z".into()], api: "https://api.hanzo.ai".into() });
        let mut term = Terminal::new(TestBackend::new(136, 32)).unwrap();
        term.draw(|f| render(f, f.area(), &b)).unwrap();
        let text: String = term.backend().buffer().content().iter().map(|c| c.symbol()).collect();
        for want in ["dgx · GB10", "this machine", "unified", "gpu 71.4 · cpu", "qwen3.8-flash-next", "41.2 tok/s", "210 ms", "kv 12.0%", "evo", "RTX 4090", "machines · hanzo/z", "zero trust", "hanzo/z", "GPU util", "decode  41.2 tok/s"] {
            assert!(text.contains(want), "missing {want:?}");
        }
    }
}
