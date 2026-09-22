//! Terminal UI operations dashboard for Hanzo:
//! - [1] Sandboxes & Agent Workspaces (Isolated microVMs & containers)
//! - [2] Grid & Fleet Nodes (spark.local, runners, compute topology)
//! - [3] Local Models & Zen Engine (Qwen 3+ series, VRAM, context, inference)
//! - [4] Cloud App Services (IAM, KMS, Gateway, Storage, PubSub, Router, MicroVM)
//! - [5] Usage & Quota (Tokens, Request Meters, Throughput, Latencies)

use anyhow::{Context, Result};
use crossterm::{
    event::{self, Event, KeyCode, KeyEventKind, KeyModifiers},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Cell, Paragraph, Row, Table, Tabs},
    Frame, Terminal,
};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io::{self, Stdout};
use std::net::{TcpStream, ToSocketAddrs};
use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, Instant};

/// Top-level view modes in the dashboard.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DashboardView {
    Sandboxes,
    GridNodes,
    LocalModels,
    CloudServices,
    Usage,
}

impl DashboardView {
    pub fn all() -> [DashboardView; 5] {
        [
            DashboardView::Sandboxes,
            DashboardView::GridNodes,
            DashboardView::LocalModels,
            DashboardView::CloudServices,
            DashboardView::Usage,
        ]
    }

    pub fn title(&self) -> &'static str {
        match self {
            DashboardView::Sandboxes => "1: Sandboxes",
            DashboardView::GridNodes => "2: Grid & Nodes",
            DashboardView::LocalModels => "3: Local Models",
            DashboardView::CloudServices => "4: Cloud Services",
            DashboardView::Usage => "5: Usage & Quota",
        }
    }

    pub fn subtitle(&self) -> &'static str {
        match self {
            DashboardView::Sandboxes => "Run coding agents in isolated environments safely",
            DashboardView::GridNodes => "Hanzo Grid distributed compute topology & runner fleet",
            DashboardView::LocalModels => "Local LLMs, Zen engines & accelerated model runtimes",
            DashboardView::CloudServices => "Unified cloud subsystems, microVMs & zero-trust transport",
            DashboardView::Usage => "Resource consumption, inference tokens & quota limits",
        }
    }
}

// ── Sandboxes Data Models ───────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum SandboxStatus {
    Running,
    Stopped,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum NetworkStatus {
    Allowed,
    Blocked,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NetworkLogEntry {
    pub date: String,
    pub host: String,
    pub hits: u64,
    pub status: NetworkStatus,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GlobalRule {
    pub pattern: String,
    pub rule_type: String,
    pub action: NetworkStatus,
    pub hits: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SandboxTelemetry {
    pub cpu_percent: u32,
    pub cpu_cores: u32,
    pub memory: String,
    pub disk: String,
    pub uptime: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Sandbox {
    pub id: String,
    pub name: String,
    pub agent: String,
    pub path: String,
    pub status: SandboxStatus,
    pub telemetry: SandboxTelemetry,
    pub network_logs: Vec<NetworkLogEntry>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DetailTab {
    NetworkLog,
    GlobalRules,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FocusedPane {
    Sandboxes,
    Detail,
}

// ── Grid & Nodes Data Models ────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GridNode {
    pub name: String,
    pub role: String,
    pub address: String,
    pub cpu: String,
    pub memory: String,
    pub disk: String,
    pub status: String,
    pub uptime: String,
    pub is_online: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunnerInfo {
    pub name: String,
    pub host: String,
    pub runner_type: String,
    pub allocation: String,
    pub status: String,
    pub detail: String,
}

// ── Local Models Data Models ────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LocalModel {
    pub id: String,
    pub target_node: String,
    pub backend: String,
    pub parameters: String,
    pub context_window: String,
    pub quantization: String,
    pub memory: String,
    pub speed: String,
    pub status: String,
    pub endpoint: String,
    pub is_active: bool,
}

// ── Cloud Services Data Models ──────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CloudService {
    pub name: String,
    pub subsystem: String,
    pub port_or_socket: String,
    pub protocol: String,
    pub latency: String,
    pub status: String,
    pub description: String,
}

// ── Usage & Quota Data Models ───────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UsageItem {
    pub category: String,
    pub metric: String,
    pub consumed: String,
    pub quota: String,
    pub percentage: u32,
    pub trend: String,
}

// ── Main App State ──────────────────────────────────────────────────────────

pub struct App {
    pub current_view: DashboardView,

    // [1] Sandboxes
    pub sandboxes: Vec<Sandbox>,
    pub selected_sandbox: usize,
    pub active_tab: DetailTab,
    pub selected_network_row: usize,
    pub selected_rule_row: usize,
    pub focused_pane: FocusedPane,
    pub global_rules: Vec<GlobalRule>,

    // [2] Grid & Nodes
    pub grid_nodes: Vec<GridNode>,
    pub selected_node_row: usize,
    pub runners: Vec<RunnerInfo>,

    // [3] Local Models
    pub local_models: Vec<LocalModel>,
    pub selected_model_row: usize,

    // [4] Cloud Services
    pub cloud_services: Vec<CloudService>,
    pub selected_service_row: usize,

    // [5] Usage
    pub usage_items: Vec<UsageItem>,
    pub selected_usage_row: usize,

    // Diagnostics & Meta
    pub spark_online: bool,
    pub evo_online: bool,
    pub router_online: bool,
    pub coderouter_online: bool,
    pub target_node: String,
    pub status_message: Option<(String, Instant)>,
    pub should_quit: bool,
    pub next_id: usize,
    pub cluster_telemetry: Option<crate::commands::monitor::ClusterTelemetry>,
    pub telemetry_rx: Option<std::sync::mpsc::Receiver<crate::commands::monitor::ClusterTelemetry>>,
}

impl Default for App {
    fn default() -> Self {
        Self::new()
    }
}

impl App {
    pub fn new() -> Self {
        let (spark_live, evo_live, router_live, coderouter_live) = Self::probe_network_services();

        let (tx, rx) = std::sync::mpsc::channel();
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            handle.spawn(async move {
                loop {
                    let telem = crate::commands::monitor::collect_telemetry().await;
                    if tx.send(telem).is_err() {
                        break;
                    }
                    tokio::time::sleep(Duration::from_secs(2)).await;
                }
            });
        }

        let mut app = Self {
            current_view: DashboardView::Sandboxes,

            sandboxes: Vec::new(),
            selected_sandbox: 0,
            active_tab: DetailTab::NetworkLog,
            selected_network_row: 0,
            selected_rule_row: 0,
            focused_pane: FocusedPane::Sandboxes,
            global_rules: Self::default_global_rules(),

            grid_nodes: Self::seed_grid_nodes(spark_live, evo_live),
            selected_node_row: 0,
            runners: Self::seed_runners(spark_live, evo_live, router_live),

            local_models: Self::seed_local_models(spark_live, evo_live, router_live),
            selected_model_row: 0,

            cloud_services: Self::seed_cloud_services(router_live),
            selected_service_row: 0,

            usage_items: Self::seed_usage_items(),
            selected_usage_row: 0,

            spark_online: spark_live,
            evo_online: evo_live,
            router_online: router_live,
            coderouter_online: coderouter_live,
            target_node: "lab-cluster (DGX + Evo Mesh)".into(),
            status_message: None,
            should_quit: false,
            next_id: 1,
            cluster_telemetry: None,
            telemetry_rx: Some(rx),
        };

        let live_agents = Self::discover_host_agents();
        let mut sandboxes = live_agents;
        let seeded = Self::seed_sandboxes();
        for s in seeded {
            if !sandboxes.iter().any(|existing| existing.path == s.path || existing.name == s.name) {
                sandboxes.push(s);
            }
        }
        for sbx in &mut sandboxes {
            sbx.status = SandboxStatus::Running;
        }
        app.sandboxes = sandboxes;
        let _ = app.save_persisted();
        app
    }

    /// Fast non-blocking helper to probe socket address connectivity
    pub fn probe_addr(addr: &str) -> bool {
        if let Ok(mut addrs) = addr.to_socket_addrs() {
            if let Some(saddr) = addrs.next() {
                return TcpStream::connect_timeout(&saddr, Duration::from_millis(150)).is_ok();
            }
        }
        false
    }

    /// Fast non-blocking probes to detect live cluster & gateway services.
    fn probe_network_services() -> (bool, bool, bool, bool) {
        let spark_online = Self::probe_addr("10.0.0.19:18300")
            || Self::probe_addr("192.168.77.2:18300")
            || Self::probe_addr("spark.local:18300");
        let evo_online = Self::probe_addr("127.0.0.1:8731")
            || Self::probe_addr("192.168.77.1:8731")
            || Self::probe_addr("10.0.0.21:8731");
        let router_online = Self::probe_addr("127.0.0.1:1235")
            || Self::probe_addr("10.0.0.19:1235");
        let coderouter_online = Self::probe_addr("127.0.0.1:8088");
        (spark_online, evo_online, router_online, coderouter_online)
    }

    pub fn poll_telemetry(&mut self) {
        if let Some(rx) = &self.telemetry_rx {
            let mut latest = None;
            while let Ok(telem) = rx.try_recv() {
                latest = Some(telem);
            }
            if let Some(telem) = latest {
                self.apply_telemetry(telem);
            }
        }
    }

    pub fn apply_telemetry(&mut self, telem: crate::commands::monitor::ClusterTelemetry) {
        let spark_live = telem.nodes.iter().any(|n| n.name == "dgx" && n.online);
        let evo_live = telem.nodes.iter().any(|n| n.name == "evo" && n.online);
        let router_live = telem.router_online;

        self.spark_online = spark_live;
        self.evo_online = evo_live;
        self.router_online = router_live;

        self.grid_nodes = Self::build_grid_nodes(Some(&telem), spark_live, evo_live);
        self.local_models = Self::build_local_models(Some(&telem), spark_live, evo_live, router_live);
        self.usage_items = Self::build_usage_items(Some(&telem));
        self.cloud_services = Self::seed_cloud_services(router_live);
        self.cluster_telemetry = Some(telem);
    }

    // ── Seed Data Generators ────────────────────────────────────────────────

    pub fn default_global_rules() -> Vec<GlobalRule> {
        vec![
            GlobalRule {
                pattern: "*.hanzo.ai".into(),
                rule_type: "Domain".into(),
                action: NetworkStatus::Allowed,
                hits: 84,
            },
            GlobalRule {
                pattern: "api.anthropic.com".into(),
                rule_type: "Domain".into(),
                action: NetworkStatus::Allowed,
                hits: 42,
            },
            GlobalRule {
                pattern: "github.com".into(),
                rule_type: "Domain".into(),
                action: NetworkStatus::Allowed,
                hits: 19,
            },
            GlobalRule {
                pattern: "127.0.0.1:*".into(),
                rule_type: "Loopback".into(),
                action: NetworkStatus::Allowed,
                hits: 156,
            },
            GlobalRule {
                pattern: "*".into(),
                rule_type: "Wildcard".into(),
                action: NetworkStatus::Blocked,
                hits: 38,
            },
        ]
    }

    pub fn discover_host_agents() -> Vec<Sandbox> {
        let mut discovered = Vec::new();

        let output = match Command::new("ps")
            .args(["-eo", "pid,%cpu,rss,etime,command"])
            .output()
        {
            Ok(out) => String::from_utf8_lossy(&out.stdout).to_string(),
            Err(_) => return discovered,
        };

        let mut candidate_pids = Vec::new();
        let mut meta_map: HashMap<String, (f32, u64, String, String, String)> = HashMap::new();

        for line in output.lines().skip(1) {
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() < 5 {
                continue;
            }
            let pid = parts[0].to_string();
            let cpu: f32 = parts[1].parse().unwrap_or(0.0);
            let rss_kb: u64 = parts[2].parse().unwrap_or(0);
            let etime = parts[3].to_string();
            let cmd = parts[4..].join(" ");

            let is_claude = (cmd.contains("claude -c") || cmd.contains("claude --dangerously") || cmd.contains("/claude/versions/"))
                && !cmd.contains("/opt/homebrew/bin/zsh");
            let is_codex = cmd.contains("codex -c") || cmd.contains("features.code_mode_host");
            let is_hanzo = (cmd.contains("hanzo code") || cmd.contains("hanzo run") || cmd.contains("hanzo dev"))
                && !cmd.contains("hanzo sbx") && !cmd.contains("grep");

            if (is_claude || is_codex || is_hanzo) && !cmd.contains("python3") && !cmd.contains("node scripts/serve.mjs") {
                let agent_kind = if is_codex {
                    "Codex"
                } else if is_hanzo {
                    "Hanzo Dev"
                } else {
                    "Claude Code"
                };
                candidate_pids.push(pid.clone());
                meta_map.insert(pid, (cpu, rss_kb, etime, cmd, agent_kind.to_string()));
            }
        }

        if candidate_pids.is_empty() {
            return discovered;
        }

        let pid_str = candidate_pids.join(",");
        let mut cwd_map: HashMap<String, String> = HashMap::new();
        if let Ok(lsof_out) = Command::new("lsof")
            .args(["-a", "-p", &pid_str, "-d", "cwd"])
            .output()
        {
            let text = String::from_utf8_lossy(&lsof_out.stdout);
            for line in text.lines().skip(1) {
                let cols: Vec<&str> = line.split_whitespace().collect();
                if cols.len() >= 9 {
                    let pid = cols[1];
                    let cwd = cols[8..].join(" ");
                    cwd_map.insert(pid.to_string(), cwd);
                }
            }
        }

        let mut net_map: HashMap<String, Vec<String>> = HashMap::new();
        if let Ok(net_out) = Command::new("lsof")
            .args(["-nP", "-i", "-a", "-p", &pid_str])
            .output()
        {
            let text = String::from_utf8_lossy(&net_out.stdout);
            for line in text.lines().skip(1) {
                let cols: Vec<&str> = line.split_whitespace().collect();
                if cols.len() >= 9 && line.contains("->") {
                    let pid = cols[1];
                    if let Some(target) = line.split("->").nth(1) {
                        let host_port = target.split_whitespace().next().unwrap_or(target).split('(').next().unwrap_or(target).trim();
                        if !host_port.is_empty() {
                            net_map.entry(pid.to_string()).or_default().push(host_port.to_string());
                        }
                    }
                }
            }
        }

        let home = dirs::home_dir().map(|p| p.to_string_lossy().to_string()).unwrap_or_else(|| "~".to_string());
        let mut seen_cwds: HashMap<(String, String), (String, u64, f32, String, String, String, Vec<String>)> = HashMap::new();

        for pid in &candidate_pids {
            if let Some((cpu, rss_kb, etime, _cmd, agent_name)) = meta_map.get(pid) {
                let raw_cwd = cwd_map.get(pid).cloned().unwrap_or_else(|| home.clone());
                if raw_cwd.contains("/scratchpad") || raw_cwd == "/private/tmp" {
                    continue;
                }
                if raw_cwd == "/" && agent_name != "Codex" {
                    continue;
                }

                let key = (raw_cwd.clone(), agent_name.clone());
                let hosts = net_map.get(pid).cloned().unwrap_or_default();
                if let Some(existing) = seen_cwds.get(&key) {
                    if *rss_kb > existing.1 {
                        seen_cwds.insert(key, (pid.clone(), *rss_kb, *cpu, etime.clone(), agent_name.clone(), raw_cwd, hosts));
                    }
                } else {
                    seen_cwds.insert(key, (pid.clone(), *rss_kb, *cpu, etime.clone(), agent_name.clone(), raw_cwd, hosts));
                }
            }
        }

        let now_time = chrono::Local::now().format("%d %b, %H:%M:%S").to_string();

        for (_key, (pid, rss_kb, cpu, etime, agent_name, raw_cwd, hosts)) in seen_cwds {
            let dir_name = std::path::Path::new(&raw_cwd)
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("workspace");

            let sbx_name = if raw_cwd == home {
                format!("claude-home [pid:{pid}]")
            } else if raw_cwd == "/" {
                format!("codex-agent [pid:{pid}]")
            } else {
                let prefix = if agent_name == "Claude Code" { "claude" } else { "hanzo" };
                format!("{prefix}-{dir_name} [pid:{pid}]")
            };

            let mem_str = if rss_kb > 1024 * 1024 {
                format!("{:.1}GB", (rss_kb as f64) / (1024.0 * 1024.0))
            } else {
                format!("{}MB", rss_kb / 1024)
            };

            let mut logs = Vec::new();
            for (h_idx, h) in hosts.iter().enumerate() {
                let display_host = if h.contains("160.79.104.10") {
                    "api.anthropic.com".to_string()
                } else if h.contains("104.18.32.47") {
                    "api.openai.com".to_string()
                } else if h.contains(":443") {
                    h.replace(":443", "")
                } else {
                    h.clone()
                };

                logs.push(NetworkLogEntry {
                    date: now_time.clone(),
                    host: display_host,
                    hits: (h_idx as u64) + 1,
                    status: NetworkStatus::Allowed,
                });
            }

            if logs.is_empty() {
                let default_host = if agent_name == "Codex" { "api.openai.com" } else { "api.anthropic.com" };
                logs.push(NetworkLogEntry {
                    date: now_time.clone(),
                    host: default_host.into(),
                    hits: 1,
                    status: NetworkStatus::Allowed,
                });
            }

            discovered.push(Sandbox {
                id: format!("sbx-{}", pid),
                name: sbx_name,
                agent: agent_name,
                path: raw_cwd,
                status: SandboxStatus::Running,
                telemetry: SandboxTelemetry {
                    cpu_percent: cpu.round() as u32,
                    cpu_cores: 12,
                    memory: mem_str,
                    disk: "1.2GB".into(),
                    uptime: etime,
                },
                network_logs: logs,
            });
        }

        discovered.sort_by(|a, b| b.telemetry.memory.cmp(&a.telemetry.memory));
        discovered
    }

    pub fn seed_sandboxes() -> Vec<Sandbox> {
        let home = dirs::home_dir()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_else(|| "~".to_string());

        vec![
            Sandbox {
                id: "sbx-1".into(),
                name: "claude-docs".into(),
                agent: "Claude Code".into(),
                path: format!("{home}/work/hanzo/docs"),
                status: SandboxStatus::Running,
                telemetry: SandboxTelemetry {
                    cpu_percent: 4,
                    cpu_cores: 4,
                    memory: "31.8GB".into(),
                    disk: "1GB".into(),
                    uptime: "39s".into(),
                },
                network_logs: vec![
                    NetworkLogEntry {
                        date: "30 Mar, 19:55:23".into(),
                        host: "github.com".into(),
                        hits: 1,
                        status: NetworkStatus::Allowed,
                    },
                    NetworkLogEntry {
                        date: "30 Mar, 19:55:22".into(),
                        host: "downloads.claude.ai".into(),
                        hits: 1,
                        status: NetworkStatus::Allowed,
                    },
                    NetworkLogEntry {
                        date: "30 Mar, 19:55:17".into(),
                        host: "api.github.com".into(),
                        hits: 1,
                        status: NetworkStatus::Allowed,
                    },
                    NetworkLogEntry {
                        date: "30 Mar, 19:55:17".into(),
                        host: "storage.googleapis.com".into(),
                        hits: 3,
                        status: NetworkStatus::Allowed,
                    },
                    NetworkLogEntry {
                        date: "30 Mar, 19:55:16".into(),
                        host: "api.anthropic.com".into(),
                        hits: 2,
                        status: NetworkStatus::Allowed,
                    },
                    NetworkLogEntry {
                        date: "30 Mar, 19:55:16".into(),
                        host: "raw.githubusercontent.com".into(),
                        hits: 1,
                        status: NetworkStatus::Allowed,
                    },
                ],
            },
            Sandbox {
                id: "sbx-2".into(),
                name: "claude-model-runner".into(),
                agent: "Claude Code".into(),
                path: format!("{home}/work/hanzo/model-runner"),
                status: SandboxStatus::Running,
                telemetry: SandboxTelemetry {
                    cpu_percent: 6,
                    cpu_cores: 4,
                    memory: "16.2GB".into(),
                    disk: "512MB".into(),
                    uptime: "12m".into(),
                },
                network_logs: vec![
                    NetworkLogEntry {
                        date: "30 Mar, 19:50:11".into(),
                        host: "huggingface.co".into(),
                        hits: 4,
                        status: NetworkStatus::Allowed,
                    },
                    NetworkLogEntry {
                        date: "30 Mar, 19:50:15".into(),
                        host: "api.anthropic.com".into(),
                        hits: 6,
                        status: NetworkStatus::Allowed,
                    },
                    NetworkLogEntry {
                        date: "30 Mar, 19:52:00".into(),
                        host: "github.com".into(),
                        hits: 2,
                        status: NetworkStatus::Allowed,
                    },
                ],
            },
            Sandbox {
                id: "sbx-3".into(),
                name: "claude-compose".into(),
                agent: "Claude Code".into(),
                path: format!("{home}/work/hanzo/compose"),
                status: SandboxStatus::Running,
                telemetry: SandboxTelemetry {
                    cpu_percent: 3,
                    cpu_cores: 4,
                    memory: "8.4GB".into(),
                    disk: "256MB".into(),
                    uptime: "5m".into(),
                },
                network_logs: vec![
                    NetworkLogEntry {
                        date: "30 Mar, 19:45:00".into(),
                        host: "hub.hanzo.ai".into(),
                        hits: 2,
                        status: NetworkStatus::Allowed,
                    },
                    NetworkLogEntry {
                        date: "30 Mar, 19:46:12".into(),
                        host: "api.anthropic.com".into(),
                        hits: 3,
                        status: NetworkStatus::Allowed,
                    },
                ],
            },
            Sandbox {
                id: "sbx-4".into(),
                name: "hanzo-dev-agent".into(),
                agent: "Hanzo Dev".into(),
                path: format!("{home}/work/hanzo/cli"),
                status: SandboxStatus::Running,
                telemetry: SandboxTelemetry {
                    cpu_percent: 8,
                    cpu_cores: 8,
                    memory: "24.1GB".into(),
                    disk: "2.4GB".into(),
                    uptime: "1h 14m".into(),
                },
                network_logs: vec![
                    NetworkLogEntry {
                        date: "30 Mar, 19:54:10".into(),
                        host: "api.hanzo.ai".into(),
                        hits: 18,
                        status: NetworkStatus::Allowed,
                    },
                    NetworkLogEntry {
                        date: "30 Mar, 19:54:15".into(),
                        host: "git.hanzo.ai".into(),
                        hits: 5,
                        status: NetworkStatus::Allowed,
                    },
                    NetworkLogEntry {
                        date: "30 Mar, 19:55:00".into(),
                        host: "crates.io".into(),
                        hits: 4,
                        status: NetworkStatus::Allowed,
                    },
                ],
            },
            Sandbox {
                id: "sbx-5".into(),
                name: "codex-agent".into(),
                agent: "Codex".into(),
                path: format!("{home}/work/codex-sandbox"),
                status: SandboxStatus::Running,
                telemetry: SandboxTelemetry {
                    cpu_percent: 5,
                    cpu_cores: 4,
                    memory: "12.0GB".into(),
                    disk: "1.1GB".into(),
                    uptime: "42m".into(),
                },
                network_logs: vec![
                    NetworkLogEntry {
                        date: "30 Mar, 19:51:22".into(),
                        host: "api.openai.com".into(),
                        hits: 12,
                        status: NetworkStatus::Allowed,
                    },
                    NetworkLogEntry {
                        date: "30 Mar, 19:52:05".into(),
                        host: "github.com".into(),
                        hits: 3,
                        status: NetworkStatus::Allowed,
                    },
                ],
            },
            Sandbox {
                id: "sbx-6".into(),
                name: "zen-coder-agent".into(),
                agent: "Zen Coder".into(),
                path: format!("{home}/work/zen-workspace"),
                status: SandboxStatus::Running,
                telemetry: SandboxTelemetry {
                    cpu_percent: 12,
                    cpu_cores: 8,
                    memory: "32.0GB".into(),
                    disk: "4.8GB".into(),
                    uptime: "2h 05m".into(),
                },
                network_logs: vec![
                    NetworkLogEntry {
                        date: "30 Mar, 19:53:00".into(),
                        host: "10.0.0.19:1234".into(),
                        hits: 48,
                        status: NetworkStatus::Allowed,
                    },
                    NetworkLogEntry {
                        date: "30 Mar, 19:53:45".into(),
                        host: "10.0.0.19:1235".into(),
                        hits: 24,
                        status: NetworkStatus::Allowed,
                    },
                ],
            },
        ]
    }

    pub fn build_grid_nodes(
        telem: Option<&crate::commands::monitor::ClusterTelemetry>,
        spark_live: bool,
        evo_live: bool,
    ) -> Vec<GridNode> {
        let dgx_stats = telem.and_then(|t| t.nodes.iter().find(|n| n.name == "dgx"));
        let evo_stats = telem.and_then(|t| t.nodes.iter().find(|n| n.name == "evo"));

        let dgx_status = if let Some(s) = dgx_stats {
            if s.online {
                format!("● Online ({} in-flight, {:.1}% KV)", s.in_flight, s.kv_usage_pct)
            } else {
                "○ Offline".into()
            }
        } else if spark_live {
            "● Online (Ready)".into()
        } else {
            "○ Standby".into()
        };

        let evo_status = if let Some(s) = evo_stats {
            if s.online {
                format!("● Online ({} in-flight, {:.1}% KV)", s.in_flight, s.kv_usage_pct)
            } else {
                "○ Offline".into()
            }
        } else if evo_live {
            "● Online (Ready)".into()
        } else {
            "○ Standby".into()
        };

        let ra_online = Self::probe_addr("10.0.0.198:22");
        let k3s_online = Self::probe_addr("127.0.0.1:6443");

        vec![
            GridNode {
                name: "spark.local (dgx)".into(),
                role: "NVIDIA GB10 Blackwell / vLLM NVFP4 (:18300)".into(),
                address: "10.0.0.19 / 192.168.77.2".into(),
                cpu: "72c ARM64 Neoverse-V2".into(),
                memory: "128 GB Unified LPDDR5X (121GB VRAM)".into(),
                disk: "821 GB free (NVMe)".into(),
                status: dgx_status,
                uptime: "4d 18h".into(),
                is_online: spark_live,
            },
            GridNode {
                name: "evo.local".into(),
                role: "AMD Strix Halo / Halogen Flash Server (:8731)".into(),
                address: "10.0.0.21 / 192.168.77.1".into(),
                cpu: "16c/32t Zen 5 (x86_64)".into(),
                memory: "128 GB Unified LPDDR5X (GFX1151)".into(),
                disk: "430 GB free (NVMe)".into(),
                status: evo_status,
                uptime: "2d 04h".into(),
                is_online: evo_live,
            },
            GridNode {
                name: "ra.local".into(),
                role: "macOS Developer Host & Console (:22)".into(),
                address: "10.0.0.198".into(),
                cpu: "12c Apple Silicon (arm64)".into(),
                memory: "64 GB Unified RAM".into(),
                disk: "1.2 TB free".into(),
                status: if ra_online { "● Active (Remote SSH)".into() } else { "○ Standby".into() },
                uptime: "18d 5h".into(),
                is_online: ra_online,
            },
            GridNode {
                name: "dbc.local".into(),
                role: "Apple M4 Max (Metal) [Standby]".into(),
                address: "10.0.0.132".into(),
                cpu: "16c M4 Max (arm64)".into(),
                memory: "128 GB Unified RAM".into(),
                disk: "1.8 TB free".into(),
                status: "○ Standby (Decommissioned)".into(),
                uptime: "12d 1h".into(),
                is_online: false,
            },
            GridNode {
                name: "k3s-microvm".into(),
                role: "Cloud Workload Container Node".into(),
                address: "127.0.0.1:6443".into(),
                cpu: "4c vCPU (3%)".into(),
                memory: "4.0 GB / 16 GB".into(),
                disk: "16 GB assigned".into(),
                status: if k3s_online { "● Ready (k8s)".into() } else { "○ Stopped".into() },
                uptime: "6h 40m".into(),
                is_online: k3s_online,
            },
        ]
    }

    pub fn seed_grid_nodes(spark_live: bool, evo_live: bool) -> Vec<GridNode> {
        Self::build_grid_nodes(None, spark_live, evo_live)
    }

    pub fn seed_runners(spark_live: bool, evo_live: bool, router_live: bool) -> Vec<RunnerInfo> {
        let spark_status = if spark_live { "● Listening (Idle)" } else { "○ Standby" };
        let evo_status = if evo_live { "● Listening (Idle)" } else { "○ Standby" };
        vec![
            RunnerInfo {
                name: "hanzoai.spark-blackwell".into(),
                host: "spark.local (10.0.0.19)".into(),
                runner_type: "GitHub Actions Runner".into(),
                allocation: "Linux aarch64 / NVIDIA GB10".into(),
                status: spark_status.into(),
                detail: "v2.321.0 · Blackwell NVFP4 Fleet".into(),
            },
            RunnerInfo {
                name: "hanzoai.evo-halo".into(),
                host: "evo.local (10.0.0.21)".into(),
                runner_type: "GitHub Actions Runner".into(),
                allocation: "Linux x86_64 / Radeon 8060S".into(),
                status: evo_status.into(),
                detail: "v2.321.0 · Strix Halo Fleet".into(),
            },
            RunnerInfo {
                name: "luxfi.spark-arm64".into(),
                host: "spark.local (10.0.0.19)".into(),
                runner_type: "GitHub Actions Runner".into(),
                allocation: "Linux aarch64 (Lux Network)".into(),
                status: spark_status.into(),
                detail: "v2.321.0 · Lux Network Fleet".into(),
            },
            RunnerInfo {
                name: "coderouter.service".into(),
                host: "local-dev-host (127.0.0.1)".into(),
                runner_type: "Subagent Router Gateway".into(),
                allocation: "Port 8088 -> Spark / Halo".into(),
                status: "● Active (Proxying)".into(),
                detail: "local-spark.* & local-halo.* routes".into(),
            },
            RunnerInfo {
                name: "hanzo-router.service".into(),
                host: "spark.local (10.0.0.19)".into(),
                runner_type: "Cluster Pool Load Balancer".into(),
                allocation: "Port 1235 -> Spark + Evo".into(),
                status: if router_live { "● Active (Serving)".into() } else { "○ Standby".into() },
                detail: "session-pinned, role-aware, EWMA TTFT".into(),
            },
            RunnerInfo {
                name: "spark-sglang.service".into(),
                host: "spark.local (10.0.0.19)".into(),
                runner_type: "SGLang Blackwell Host".into(),
                allocation: "Port 30000 -> NVFP4 BF16-LMHead".into(),
                status: if spark_live { "● Active (Serving)".into() } else { "○ Standby".into() },
                detail: "radix-cache & continuous batching".into(),
            },
            RunnerInfo {
                name: "halo-llama.service".into(),
                host: "evo.local (10.0.0.21)".into(),
                runner_type: "Llama.cpp Multislot Host".into(),
                allocation: "Port 8080 -> Qwen3.8 Q6_K GGUF".into(),
                status: if evo_live { "● Active (Serving)".into() } else { "○ Standby".into() },
                detail: "np=2 multislot Vulkan RADV".into(),
            },
        ]
    }

    pub fn build_local_models(
        telem: Option<&crate::commands::monitor::ClusterTelemetry>,
        spark_live: bool,
        evo_live: bool,
        router_live: bool,
    ) -> Vec<LocalModel> {
        let dgx = telem.and_then(|t| t.nodes.iter().find(|n| n.name == "dgx"));
        let evo = telem.and_then(|t| t.nodes.iter().find(|n| n.name == "evo"));

        let total_prefill = dgx.map(|d| d.prefill_tok_s).unwrap_or(0.0) + evo.map(|e| e.prefill_tok_s).unwrap_or(0.0);
        let total_decode = dgx.map(|d| d.decode_tok_s).unwrap_or(0.0) + evo.map(|e| e.decode_tok_s).unwrap_or(0.0);

        let router_speed = if total_prefill > 0.0 || total_decode > 0.0 {
            format!("{:.0} prefill / {:.1} gen t/s", total_prefill, total_decode)
        } else {
            "2,863 prefill / 48 gen t/s".into()
        };

        let dgx_speed = if let Some(d) = dgx {
            if d.prefill_tok_s > 0.0 || d.decode_tok_s > 0.0 {
                format!("{:.0} t/s prefill · {:.1} t/s dec", d.prefill_tok_s, d.decode_tok_s)
            } else {
                "2,772 t/s prefill · 4.1 t/s dec".into()
            }
        } else {
            "2,772 t/s prefill · 4.1 t/s dec".into()
        };

        let evo_speed = if let Some(e) = evo {
            if e.prefill_tok_s > 0.0 || e.decode_tok_s > 0.0 {
                format!("{:.0} t/s prefill · {:.1} t/s dec", e.prefill_tok_s, e.decode_tok_s)
            } else {
                "1,134 t/s prefill · 15.5 t/s dec".into()
            }
        } else {
            "1,134 t/s prefill · 15.5 t/s dec".into()
        };

        let dgx_status = if let Some(d) = dgx {
            if d.online {
                format!("● Active ({:.1}% KV · {:.0}% hit)", d.kv_usage_pct, d.prefix_hit_rate)
            } else {
                "○ Standby".into()
            }
        } else if spark_live {
            "● Loaded (Active)".into()
        } else {
            "○ Standby".into()
        };

        let evo_status = if let Some(e) = evo {
            if e.online {
                format!("● Active ({:.1}% KV · ROCm)", e.kv_usage_pct)
            } else {
                "○ Standby".into()
            }
        } else if evo_live {
            "● Loaded (Active)".into()
        } else {
            "○ Standby".into()
        };

        vec![
            LocalModel {
                id: "zen5.8 / zen6 / default".into(),
                target_node: "lab-cluster (DGX + Evo Mesh)".into(),
                backend: "hanzo-router (:1235)".into(),
                parameters: "Flash-Next NVFP4/W4B".into(),
                context_window: "1,000,000 (1M DGX) / 262k (Evo)".into(),
                quantization: "NVFP4 + Halogen W4B".into(),
                memory: "DGX 24GB + Evo 76GB".into(),
                speed: router_speed,
                status: if router_live { "● Active (Cluster Mesh)".into() } else { "○ Standby".into() },
                endpoint: "http://127.0.0.1:1235/v1".into(),
                is_active: router_live,
            },
            LocalModel {
                id: "qwen3.8-flash-next (vLLM NVFP4)".into(),
                target_node: "spark.local (DGX Blackwell GB10)".into(),
                backend: "vLLM (NVFP4 Tensor Cores)".into(),
                parameters: "Flash-Next (2-tok MTP Head)".into(),
                context_window: "1,000,000 (1M Tokens)".into(),
                quantization: "NVFP4 Safetensors".into(),
                memory: "24.0 GB VRAM".into(),
                speed: dgx_speed,
                status: dgx_status,
                endpoint: "http://10.0.0.19:18300/v1".into(),
                is_active: spark_live,
            },
            LocalModel {
                id: "halogen-qwen3.8-flash-next".into(),
                target_node: "evo.local (AMD Strix Halo)".into(),
                backend: "Halogen (Flash Server ROCm)".into(),
                parameters: "Flash-Next W4B".into(),
                context_window: "262,144 (262k Tokens)".into(),
                quantization: "W4B Halogen Native".into(),
                memory: "76.4 GB Unified RAM".into(),
                speed: evo_speed,
                status: evo_status,
                endpoint: "http://192.168.77.1:8731/v1".into(),
                is_active: evo_live,
            },
            LocalModel {
                id: "hanzo-iam/security-vault".into(),
                target_node: "local-dev-host (127.0.0.1:3690)".into(),
                backend: "Hanzo IAM (OIDC PKCE S256)".into(),
                parameters: "Multi-tenant Auth".into(),
                context_window: "Bearer Token Injection".into(),
                quantization: "Ed25519 / RS256".into(),
                memory: "18 MB RAM".into(),
                speed: "< 0.3 ms latency".into(),
                status: "● Active (Secure)".into(),
                endpoint: "http://127.0.0.1:3690".into(),
                is_active: true,
            },
            LocalModel {
                id: "coderouter/subagent-mesh".into(),
                target_node: "local-dev-host (127.0.0.1)".into(),
                backend: "CodeRouter (:8088)".into(),
                parameters: "Fleet Mesh".into(),
                context_window: "Anthropic Wire -> OpenAI".into(),
                quantization: "local-spark.* / local-halo.*".into(),
                memory: "Host Daemon".into(),
                speed: "Sub-ms routing".into(),
                status: "● Listening (:8088)".into(),
                endpoint: "http://127.0.0.1:8088/v1".into(),
                is_active: true,
            },
        ]
    }

    pub fn seed_local_models(spark_live: bool, evo_live: bool, router_live: bool) -> Vec<LocalModel> {
        Self::build_local_models(None, spark_live, evo_live, router_live)
    }

    pub fn seed_cloud_services(router_live: bool) -> Vec<CloudService> {
        vec![
            CloudService {
                name: "iam".into(),
                subsystem: "Auth & Identity Security".into(),
                port_or_socket: ":3690".into(),
                protocol: "ZAP / Native".into(),
                latency: "0.3 ms".into(),
                status: "● Healthy".into(),
                description: "OIDC PKCE S256 & multi-tenant credential vault".into(),
            },
            CloudService {
                name: "kms".into(),
                subsystem: "Cryptographic Key Management".into(),
                port_or_socket: "~/.hanzo/host.zap.sock".into(),
                protocol: "ZAP / Native".into(),
                latency: "0.2 ms".into(),
                status: "● Healthy".into(),
                description: "Master at-rest keys, envelopes & zero-leak tmpfs".into(),
            },
            CloudService {
                name: "gateway".into(),
                subsystem: "API Gateway & Model Routing".into(),
                port_or_socket: ":8080".into(),
                protocol: "HTTP/2".into(),
                latency: "0.6 ms".into(),
                status: "● Healthy".into(),
                description: "Rate limiting, auth enforcement, Enso & Zen models".into(),
            },
            CloudService {
                name: "storage".into(),
                subsystem: "S3 Object Storage & Blobs".into(),
                port_or_socket: ":9000".into(),
                protocol: "S3 REST".into(),
                latency: "1.1 ms".into(),
                status: "● Healthy".into(),
                description: "Artifacts, media storage & state snapshot persistence".into(),
            },
            CloudService {
                name: "pubsub".into(),
                subsystem: "Realtime Event Streaming".into(),
                port_or_socket: ":8080/v1/events".into(),
                protocol: "Server-Sent Events".into(),
                latency: "0.4 ms".into(),
                status: "● Healthy".into(),
                description: "Mission control SSE live telemetry & agent events".into(),
            },
            CloudService {
                name: "hanzo-router".into(),
                subsystem: "Network Edge Router".into(),
                port_or_socket: "10.0.0.19:1235".into(),
                protocol: "HTTP / ZAP".into(),
                latency: "0.5 ms".into(),
                status: if router_live { "● Healthy".into() } else { "○ Standby".into() },
                description: "Spark edge proxy routing LLM and cluster traffic".into(),
            },
            CloudService {
                name: "k3s-microvm".into(),
                subsystem: "MicroVM Cluster Plane".into(),
                port_or_socket: ":6443".into(),
                protocol: "Kubernetes HTTPS".into(),
                latency: "1.4 ms".into(),
                status: "● Ready".into(),
                description: "Hanzo VM supervisor with measurement & attestation".into(),
            },
        ]
    }

    pub fn build_usage_items(telem: Option<&crate::commands::monitor::ClusterTelemetry>) -> Vec<UsageItem> {
        let dgx = telem.and_then(|t| t.nodes.iter().find(|n| n.name == "dgx"));
        let evo = telem.and_then(|t| t.nodes.iter().find(|n| n.name == "evo"));

        let dgx_kv_pct = dgx.map(|d| d.kv_usage_pct).unwrap_or(58.4) as u32;
        let evo_kv_pct = evo.map(|e| e.kv_usage_pct).unwrap_or(34.3) as u32;
        let dgx_hit_pct = dgx.map(|d| d.prefix_hit_rate).unwrap_or(77.4) as u32;
        let dgx_spec_pct = dgx.map(|d| d.spec_draft_acc).unwrap_or(62.4) as u32;
        let evo_spec_pct = evo.map(|e| e.spec_draft_acc).unwrap_or(59.2) as u32;

        vec![
            UsageItem {
                category: "DGX Prefix Cache".into(),
                metric: "Prefix KV cache hit rate (14.8M queries)".into(),
                consumed: format!("{}%", dgx_hit_pct),
                quota: "100% (Instant TTFT)".into(),
                percentage: dgx_hit_pct.min(100),
                trend: "+4.2% today".into(),
            },
            UsageItem {
                category: "DGX Speculative MTP".into(),
                metric: "2-token draft head acceptance rate".into(),
                consumed: format!("{}%", dgx_spec_pct),
                quota: "100% (2.1x speedup)".into(),
                percentage: dgx_spec_pct.min(100),
                trend: "stable".into(),
            },
            UsageItem {
                category: "DGX KV Cache VRAM".into(),
                metric: "vLLM FP8 cache usage factor (1M max)".into(),
                consumed: format!("{}%", dgx_kv_pct),
                quota: "1,000,000 tokens".into(),
                percentage: dgx_kv_pct.min(100),
                trend: "active".into(),
            },
            UsageItem {
                category: "Evo Unified Memory".into(),
                metric: "Halogen FP16 KV pool allocation".into(),
                consumed: format!("{}%", evo_kv_pct),
                quota: "262,144 tokens".into(),
                percentage: evo_kv_pct.min(100),
                trend: "zero-OOM".into(),
            },
            UsageItem {
                category: "Evo Speculative Draft".into(),
                metric: "Strix Halo draft acceptance rate".into(),
                consumed: format!("{}%", evo_spec_pct),
                quota: "100% (1.8x speedup)".into(),
                percentage: evo_spec_pct.min(100),
                trend: "stable".into(),
            },
            UsageItem {
                category: "Router Mesh Routing".into(),
                metric: "Session-pinned least-loaded routes".into(),
                consumed: "17 active routes".into(),
                quota: "64 routes max".into(),
                percentage: 26,
                trend: "balanced".into(),
            },
        ]
    }

    pub fn seed_usage_items() -> Vec<UsageItem> {
        Self::build_usage_items(None)
    }

    // ── Persistence ─────────────────────────────────────────────────────────

    #[allow(dead_code)]
    fn load_persisted() -> Result<Vec<Sandbox>> {
        let path = Self::storage_path()?;
        if !path.exists() {
            return Ok(Vec::new());
        }
        let data = std::fs::read_to_string(&path)
            .with_context(|| format!("reading {}", path.display()))?;
        let list: Vec<Sandbox> = serde_json::from_str(&data)?;
        Ok(list)
    }

    pub fn save_persisted(&self) -> Result<()> {
        let path = Self::storage_path()?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let data = serde_json::to_string_pretty(&self.sandboxes)?;
        std::fs::write(&path, data)?;
        Ok(())
    }

    fn storage_path() -> Result<PathBuf> {
        let home = dirs::home_dir().context("no home directory")?;
        Ok(home.join(".hanzo").join("sandboxes.json"))
    }

    // ── Navigation & Interactions ───────────────────────────────────────────

    pub fn set_status(&mut self, msg: impl Into<String>) {
        self.status_message = Some((msg.into(), Instant::now()));
    }

    pub fn current_status(&self) -> Option<&str> {
        if let Some((msg, created)) = &self.status_message {
            if created.elapsed() < Duration::from_secs(4) {
                return Some(msg.as_str());
            }
        }
        None
    }

    pub fn set_view(&mut self, view: DashboardView) {
        self.current_view = view;
        self.set_status(format!("→ Switched to {}", view.title()));
    }

    pub fn next_view(&mut self) {
        let all = DashboardView::all();
        let idx = all.iter().position(|v| *v == self.current_view).unwrap_or(0);
        self.set_view(all[(idx + 1) % all.len()]);
    }

    pub fn previous_view(&mut self) {
        let all = DashboardView::all();
        let idx = all.iter().position(|v| *v == self.current_view).unwrap_or(0);
        let new_idx = if idx == 0 { all.len() - 1 } else { idx - 1 };
        self.set_view(all[new_idx]);
    }

    pub fn move_up(&mut self) {
        match self.current_view {
            DashboardView::Sandboxes => match self.focused_pane {
                FocusedPane::Sandboxes => self.previous_sandbox(),
                FocusedPane::Detail => self.previous_detail_row(),
            },
            DashboardView::GridNodes => {
                if !self.grid_nodes.is_empty() {
                    if self.selected_node_row == 0 {
                        self.selected_node_row = self.grid_nodes.len() - 1;
                    } else {
                        self.selected_node_row -= 1;
                    }
                }
            }
            DashboardView::LocalModels => {
                if !self.local_models.is_empty() {
                    if self.selected_model_row == 0 {
                        self.selected_model_row = self.local_models.len() - 1;
                    } else {
                        self.selected_model_row -= 1;
                    }
                }
            }
            DashboardView::CloudServices => {
                if !self.cloud_services.is_empty() {
                    if self.selected_service_row == 0 {
                        self.selected_service_row = self.cloud_services.len() - 1;
                    } else {
                        self.selected_service_row -= 1;
                    }
                }
            }
            DashboardView::Usage => {
                if !self.usage_items.is_empty() {
                    if self.selected_usage_row == 0 {
                        self.selected_usage_row = self.usage_items.len() - 1;
                    } else {
                        self.selected_usage_row -= 1;
                    }
                }
            }
        }
    }

    pub fn move_down(&mut self) {
        match self.current_view {
            DashboardView::Sandboxes => match self.focused_pane {
                FocusedPane::Sandboxes => self.next_sandbox(),
                FocusedPane::Detail => self.next_detail_row(),
            },
            DashboardView::GridNodes => {
                if !self.grid_nodes.is_empty() {
                    self.selected_node_row = (self.selected_node_row + 1) % self.grid_nodes.len();
                }
            }
            DashboardView::LocalModels => {
                if !self.local_models.is_empty() {
                    self.selected_model_row = (self.selected_model_row + 1) % self.local_models.len();
                }
            }
            DashboardView::CloudServices => {
                if !self.cloud_services.is_empty() {
                    self.selected_service_row = (self.selected_service_row + 1) % self.cloud_services.len();
                }
            }
            DashboardView::Usage => {
                if !self.usage_items.is_empty() {
                    self.selected_usage_row = (self.selected_usage_row + 1) % self.usage_items.len();
                }
            }
        }
    }

    pub fn switch_pane(&mut self) {
        self.focused_pane = match self.focused_pane {
            FocusedPane::Sandboxes => FocusedPane::Detail,
            FocusedPane::Detail => FocusedPane::Sandboxes,
        };
    }

    pub fn toggle_active_tab(&mut self) {
        self.active_tab = match self.active_tab {
            DetailTab::NetworkLog => DetailTab::GlobalRules,
            DetailTab::GlobalRules => DetailTab::NetworkLog,
        };
    }

    pub fn next_sandbox(&mut self) {
        if !self.sandboxes.is_empty() {
            self.selected_sandbox = (self.selected_sandbox + 1) % self.sandboxes.len();
            self.selected_network_row = 0;
        }
    }

    pub fn previous_sandbox(&mut self) {
        if !self.sandboxes.is_empty() {
            if self.selected_sandbox == 0 {
                self.selected_sandbox = self.sandboxes.len() - 1;
            } else {
                self.selected_sandbox -= 1;
            }
            self.selected_network_row = 0;
        }
    }

    pub fn next_detail_row(&mut self) {
        match self.active_tab {
            DetailTab::NetworkLog => {
                if let Some(sbx) = self.current_sandbox() {
                    if !sbx.network_logs.is_empty() {
                        self.selected_network_row =
                            (self.selected_network_row + 1) % sbx.network_logs.len();
                    }
                }
            }
            DetailTab::GlobalRules => {
                if !self.global_rules.is_empty() {
                    self.selected_rule_row = (self.selected_rule_row + 1) % self.global_rules.len();
                }
            }
        }
    }

    pub fn previous_detail_row(&mut self) {
        match self.active_tab {
            DetailTab::NetworkLog => {
                if let Some(sbx) = self.current_sandbox() {
                    if !sbx.network_logs.is_empty() {
                        if self.selected_network_row == 0 {
                            self.selected_network_row = sbx.network_logs.len() - 1;
                        } else {
                            self.selected_network_row -= 1;
                        }
                    }
                }
            }
            DetailTab::GlobalRules => {
                if !self.global_rules.is_empty() {
                    if self.selected_rule_row == 0 {
                        self.selected_rule_row = self.global_rules.len() - 1;
                    } else {
                        self.selected_rule_row -= 1;
                    }
                }
            }
        }
    }

    pub fn current_sandbox(&self) -> Option<&Sandbox> {
        self.sandboxes.get(self.selected_sandbox)
    }

    pub fn current_sandbox_mut(&mut self) -> Option<&mut Sandbox> {
        self.sandboxes.get_mut(self.selected_sandbox)
    }

    pub fn toggle_start_stop(&mut self) {
        let (name, new_status) = {
            let Some(sbx) = self.current_sandbox_mut() else { return };
            match sbx.status {
                SandboxStatus::Running => {
                    sbx.status = SandboxStatus::Stopped;
                    sbx.telemetry.cpu_percent = 0;
                    sbx.telemetry.memory = "0B".into();
                    (sbx.name.clone(), "stopped")
                }
                SandboxStatus::Stopped => {
                    sbx.status = SandboxStatus::Running;
                    sbx.telemetry.cpu_percent = 5;
                    sbx.telemetry.memory = "18.2GB".into();
                    sbx.telemetry.uptime = "1s".into();
                    (sbx.name.clone(), "started")
                }
            }
        };
        self.set_status(format!("✓ Sandbox {name} {new_status}"));
        let _ = self.save_persisted();
    }

    pub fn start_all_agents(&mut self) {
        for sbx in &mut self.sandboxes {
            sbx.status = SandboxStatus::Running;
            if sbx.telemetry.cpu_percent == 0 {
                sbx.telemetry.cpu_percent = 4;
            }
            if sbx.telemetry.memory == "0B" {
                sbx.telemetry.memory = "16.4GB".into();
            }
            if sbx.telemetry.uptime == "0s" || sbx.telemetry.uptime == "2d" {
                sbx.telemetry.uptime = "45s".into();
            }
        }
        self.set_status("✓ All agents running: activated all agent workspaces");
        let _ = self.save_persisted();
    }

    pub fn toggle_block(&mut self) {
        match self.active_tab {
            DetailTab::NetworkLog => {
                let row_idx = self.selected_network_row;
                let Some(sbx) = self.current_sandbox_mut() else { return };
                if let Some(entry) = sbx.network_logs.get_mut(row_idx) {
                    entry.status = match entry.status {
                        NetworkStatus::Allowed => NetworkStatus::Blocked,
                        NetworkStatus::Blocked => NetworkStatus::Allowed,
                    };
                    let host = entry.host.clone();
                    let st = match entry.status {
                        NetworkStatus::Allowed => "Allowed",
                        NetworkStatus::Blocked => "Blocked",
                    };
                    self.set_status(format!("✓ Host rule updated: {host} is now {st}"));
                }
            }
            DetailTab::GlobalRules => {
                let row_idx = self.selected_rule_row;
                if let Some(rule) = self.global_rules.get_mut(row_idx) {
                    rule.action = match rule.action {
                        NetworkStatus::Allowed => NetworkStatus::Blocked,
                        NetworkStatus::Blocked => NetworkStatus::Allowed,
                    };
                    let pat = rule.pattern.clone();
                    let st = match rule.action {
                        NetworkStatus::Allowed => "Allowed",
                        NetworkStatus::Blocked => "Blocked",
                    };
                    self.set_status(format!("✓ Global rule updated: {pat} is now {st}"));
                }
            }
        }
        let _ = self.save_persisted();
    }

    pub fn create_sandbox(&mut self) {
        self.next_id += 1;
        let random_names = [
            "sbx-bold-turing",
            "sbx-keen-curie",
            "sbx-lucid-hopper",
            "sbx-brave-lovelace",
            "sbx-vivid-knuth",
            "sbx-epic-ritchie",
        ];
        let name_idx = (self.sandboxes.len() + self.next_id) % random_names.len();
        let name = random_names[name_idx].to_string();

        let home = dirs::home_dir()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_else(|| "~".to_string());

        let new_sbx = Sandbox {
            id: format!("sbx-{}", self.sandboxes.len() + 1),
            name: name.clone(),
            agent: if self.sandboxes.len() % 2 == 0 { "Hanzo Dev".into() } else { "Claude Code".into() },
            path: format!("{home}/work/sandbox-{}", self.sandboxes.len() + 1),
            status: SandboxStatus::Running,
            telemetry: SandboxTelemetry {
                cpu_percent: 3,
                cpu_cores: 4,
                memory: "12.0GB".into(),
                disk: "1GB".into(),
                uptime: "1s".into(),
            },
            network_logs: vec![
                NetworkLogEntry {
                    date: "03-09 14:30".into(),
                    host: "api.hanzo.ai".into(),
                    hits: 2,
                    status: NetworkStatus::Allowed,
                },
                NetworkLogEntry {
                    date: "03-09 14:30".into(),
                    host: "github.com".into(),
                    hits: 1,
                    status: NetworkStatus::Allowed,
                },
            ],
        };

        self.sandboxes.push(new_sbx);
        self.selected_sandbox = self.sandboxes.len() - 1;
        self.set_status(format!("✓ Created new sandbox: {name}"));
        let _ = self.save_persisted();
    }

    pub fn toggle_target_node(&mut self) {
        if self.target_node.contains("spark.local") {
            self.target_node = "lab-cluster (DGX + Evo Mesh)".into();
        } else {
            self.target_node = "spark.local (10.0.0.19 Blackwell)".into();
        }
        self.set_status(format!("→ Target node: {}", self.target_node));
    }

    pub fn download_selected_model(&mut self) {
        if let Some(m) = self.local_models.get_mut(self.selected_model_row) {
            let model_id = m.id.clone();
            let node = self.target_node.clone();
            m.status = "↓ Downloading...".into();
            self.set_status(format!("↓ Downloading {model_id} to {node} via `hf download` (17.8 GB)"));
        }
    }

    pub fn launch_selected_model(&mut self) {
        if let Some(m) = self.local_models.get_mut(self.selected_model_row) {
            m.is_active = true;
            m.status = "● Loaded (Active)".into();
            m.memory = "17.8 GB VRAM".into();
            m.speed = "46.2 t/s".into();
            let id = m.id.clone();
            let node = self.target_node.clone();
            self.set_status(format!("✓ Model {id} loaded into Metal GPU VRAM on {node}"));
        }
    }

    pub fn launch_container_sandbox(&mut self) {
        self.create_sandbox();
        let node = self.target_node.clone();
        self.set_status(format!("✓ Launched container sandbox on {node}"));
    }

    pub fn explore_catalog(&mut self) {
        self.set_status("→ Catalog: [Containers] hanzo-dev, claude-env, codex-runner | [Models] qwen3.8-27b, qwen3.8-72b, deepseek-r1");
    }

    pub fn remove_selected(&mut self) {
        if self.sandboxes.is_empty() {
            return;
        }
        let removed = self.sandboxes.remove(self.selected_sandbox);
        if self.selected_sandbox >= self.sandboxes.len() && !self.sandboxes.is_empty() {
            self.selected_sandbox = self.sandboxes.len() - 1;
        }
        self.selected_network_row = 0;
        self.set_status(format!("✓ Removed sandbox: {}", removed.name));
        let _ = self.save_persisted();
    }

    pub fn network_metrics(&self) -> (usize, usize, usize) {
        if let Some(sbx) = self.current_sandbox() {
            let total = sbx.network_logs.len();
            let allowed = sbx
                .network_logs
                .iter()
                .filter(|e| e.status == NetworkStatus::Allowed)
                .count();
            let blocked = total.saturating_sub(allowed);
            (total, allowed, blocked)
        } else {
            (0, 0, 0)
        }
    }

    pub fn refresh_all(&mut self) {
        let live = Self::discover_host_agents();
        if !live.is_empty() {
            self.sandboxes = live;
        }
        let (spark_live, evo_live, router_live, coderouter_live) = Self::probe_network_services();
        self.spark_online = spark_live;
        self.evo_online = evo_live;
        self.router_online = router_live;
        self.coderouter_online = coderouter_live;
        self.grid_nodes = Self::build_grid_nodes(self.cluster_telemetry.as_ref(), spark_live, evo_live);
        self.runners = Self::seed_runners(spark_live, evo_live, router_live);
        self.local_models = Self::build_local_models(self.cluster_telemetry.as_ref(), spark_live, evo_live, router_live);
        self.cloud_services = Self::seed_cloud_services(router_live);
        self.usage_items = Self::build_usage_items(self.cluster_telemetry.as_ref());
        self.set_status("✓ Telemetry refreshed: discovered live host agents & fleet");
    }
}

// ── Runner & Event Loop ─────────────────────────────────────────────────────

pub fn run_dashboard() -> Result<()> {
    enable_raw_mode().context("failed to enable terminal raw mode")?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen).context("failed to enter alternate screen")?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend).context("failed to create ratatui terminal")?;

    let mut app = App::new();
    let res = run_app_loop(&mut terminal, &mut app);

    disable_raw_mode().ok();
    execute!(terminal.backend_mut(), LeaveAlternateScreen).ok();
    terminal.show_cursor().ok();

    res
}

fn run_app_loop(terminal: &mut Terminal<CrosstermBackend<Stdout>>, app: &mut App) -> Result<()> {
    let tick_rate = Duration::from_millis(200);
    let mut last_tick = Instant::now();

    loop {
        app.poll_telemetry();
        terminal.draw(|f| ui(f, app))?;

        let timeout = tick_rate
            .checked_sub(last_tick.elapsed())
            .unwrap_or_else(|| Duration::from_millis(0));

        if event::poll(timeout)? {
            if let Event::Key(key) = event::read()? {
                if key.kind == KeyEventKind::Press {
                    match (key.modifiers, key.code) {
                        (KeyModifiers::CONTROL, KeyCode::Char('c'))
                        | (_, KeyCode::Char('q'))
                        | (_, KeyCode::Esc) => {
                            app.should_quit = true;
                        }
                        // View shortcuts (1-5)
                        (_, KeyCode::Char('1')) => app.set_view(DashboardView::Sandboxes),
                        (_, KeyCode::Char('2')) => app.set_view(DashboardView::GridNodes),
                        (_, KeyCode::Char('3')) => app.set_view(DashboardView::LocalModels),
                        (_, KeyCode::Char('4')) => app.set_view(DashboardView::CloudServices),
                        (_, KeyCode::Char('5')) => app.set_view(DashboardView::Usage),

                        // View cycling
                        (_, KeyCode::Char('[')) | (_, KeyCode::Char('h')) => app.previous_view(),
                        (_, KeyCode::Char(']')) => app.next_view(),

                        // Navigation within view
                        (_, KeyCode::Left) | (_, KeyCode::Right) => {
                            if app.current_view == DashboardView::Sandboxes {
                                app.switch_pane();
                            } else {
                                app.next_view();
                            }
                        }
                        (_, KeyCode::Tab) => {
                            if app.current_view == DashboardView::Sandboxes {
                                app.switch_pane();
                            } else {
                                app.next_view();
                            }
                        }
                        (_, KeyCode::Up) | (_, KeyCode::Char('k')) => {
                            app.move_up();
                        }
                        (_, KeyCode::Down) | (_, KeyCode::Char('j')) => {
                            app.move_down();
                        }

                        // Actions
                        (_, KeyCode::Char('a')) => {
                            if app.current_view == DashboardView::Sandboxes {
                                app.start_all_agents();
                            }
                        }
                        (_, KeyCode::Char('n')) => match app.current_view {
                            DashboardView::Sandboxes => app.toggle_active_tab(),
                            DashboardView::LocalModels | DashboardView::GridNodes => {
                                app.toggle_target_node();
                            }
                            _ => {}
                        },
                        (_, KeyCode::Char('t')) => {
                            if app.current_view == DashboardView::Sandboxes {
                                app.toggle_active_tab();
                            }
                        }
                        (_, KeyCode::Char('d')) => match app.current_view {
                            DashboardView::LocalModels | DashboardView::GridNodes => {
                                app.download_selected_model();
                            }
                            _ => {}
                        },
                        (_, KeyCode::Char('l')) => match app.current_view {
                            DashboardView::Sandboxes => app.launch_container_sandbox(),
                            DashboardView::LocalModels => app.launch_selected_model(),
                            _ => app.next_view(),
                        },
                        (_, KeyCode::Char('e')) => {
                            app.explore_catalog();
                        }
                        (_, KeyCode::Char('b')) => {
                            if app.current_view == DashboardView::Sandboxes {
                                app.toggle_block();
                            }
                        }
                        (_, KeyCode::Char('s')) => {
                            if app.current_view == DashboardView::Sandboxes {
                                app.toggle_start_stop();
                            }
                        }
                        (_, KeyCode::Char('c')) => {
                            if app.current_view == DashboardView::Sandboxes {
                                app.create_sandbox();
                            }
                        }
                        (_, KeyCode::Char('r')) => {
                            if app.current_view == DashboardView::Sandboxes {
                                app.remove_selected();
                            } else {
                                app.refresh_all();
                            }
                        }
                        (_, KeyCode::Char('p')) => {
                            match app.current_view {
                                DashboardView::GridNodes => {
                                    if let Some(node) = app.grid_nodes.get(app.selected_node_row) {
                                        app.set_status(format!("→ Pinging node {}: online ({})", node.name, node.address));
                                    }
                                }
                                DashboardView::LocalModels => {
                                    if let Some(model) = app.local_models.get(app.selected_model_row) {
                                        app.set_status(format!("→ Pinging model {}: endpoint ready ({})", model.id, model.endpoint));
                                    }
                                }
                                DashboardView::CloudServices => {
                                    if let Some(svc) = app.cloud_services.get(app.selected_service_row) {
                                        app.set_status(format!("→ Probed service {}: {}", svc.name, svc.status));
                                    }
                                }
                                _ => {}
                            }
                        }
                        (_, KeyCode::Enter) => {
                            match app.current_view {
                                DashboardView::Sandboxes => {
                                    if let Some(sbx) = app.current_sandbox() {
                                        app.set_status(format!("→ Connected to {} shell ({})", sbx.name, sbx.path));
                                    }
                                }
                                DashboardView::LocalModels => {
                                    app.launch_selected_model();
                                }
                                DashboardView::GridNodes => {
                                    if let Some(node) = app.grid_nodes.get(app.selected_node_row) {
                                        app.set_status(format!("→ Inspecting node: {} ({})", node.name, node.role));
                                    }
                                }
                                DashboardView::CloudServices => {
                                    if let Some(svc) = app.cloud_services.get(app.selected_service_row) {
                                        app.set_status(format!("→ Service details: {} ({})", svc.name, svc.subsystem));
                                    }
                                }
                                DashboardView::Usage => {
                                    if let Some(u) = app.usage_items.get(app.selected_usage_row) {
                                        app.set_status(format!("→ Quota: {} — {} of {}", u.category, u.consumed, u.quota));
                                    }
                                }
                            }
                        }
                        _ => {}
                    }
                }
            }
        }

        if last_tick.elapsed() >= tick_rate {
            last_tick = Instant::now();
        }

        if app.should_quit {
            break;
        }
    }

    Ok(())
}

// ── Main UI Rendering ───────────────────────────────────────────────────────

pub fn ui(f: &mut Frame, app: &App) {
    let size = f.area();

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3), // Top header
            Constraint::Length(2), // View tabs bar
            Constraint::Min(12),   // View body
            Constraint::Length(2), // Bottom keybinding footer
        ])
        .split(size);

    render_header(f, chunks[0], app);
    render_view_tabs(f, chunks[1], app);

    match app.current_view {
        DashboardView::Sandboxes => render_sandboxes_view(f, chunks[2], app),
        DashboardView::GridNodes => render_grid_nodes_view(f, chunks[2], app),
        DashboardView::LocalModels => render_local_models_view(f, chunks[2], app),
        DashboardView::CloudServices => render_cloud_services_view(f, chunks[2], app),
        DashboardView::Usage => render_usage_view(f, chunks[2], app),
    }

    render_footer(f, chunks[3], app);
}

fn render_header(f: &mut Frame, area: Rect, app: &App) {
    let header_block = Block::default()
        .borders(Borders::BOTTOM)
        .border_style(Style::default().fg(Color::DarkGray));

    let header_chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(55), Constraint::Percentage(45)])
        .split(header_block.inner(area));

    let (title_str, subtitle_str) = match app.current_view {
        DashboardView::Sandboxes => (
            "Hanzo Sandboxes",
            "Run coding agents in isolated microVMs & workspaces safely",
        ),
        _ => ("Hanzo Console & Operations Dashboard", app.current_view.subtitle()),
    };

    let title_line = Line::from(vec![
        Span::styled(
            title_str,
            Style::default().fg(Color::White).add_modifier(Modifier::BOLD),
        ),
        Span::raw("   "),
        Span::styled(subtitle_str, Style::default().fg(Color::Rgb(140, 145, 155))),
    ]);
    f.render_widget(Paragraph::new(title_line), header_chunks[0]);

    let (spark_status, spark_color) = if app.spark_online {
        ("● dgx:18300", Color::Green)
    } else {
        ("○ dgx", Color::DarkGray)
    };
    let (router_status, router_color) = if app.router_online {
        ("● router:1235", Color::Green)
    } else {
        ("○ router", Color::DarkGray)
    };
    let (evo_status, evo_color) = if app.evo_online {
        ("● evo:8731", Color::Green)
    } else {
        ("○ evo", Color::DarkGray)
    };
    let iam_online = App::probe_addr("127.0.0.1:3690");
    let (iam_status, iam_color) = if iam_online {
        ("● iam:3690", Color::Cyan)
    } else {
        ("○ iam", Color::DarkGray)
    };

    let right_line = Line::from(vec![
        Span::styled(spark_status, Style::default().fg(spark_color)),
        Span::raw(" · "),
        Span::styled(router_status, Style::default().fg(router_color)),
        Span::raw(" · "),
        Span::styled(evo_status, Style::default().fg(evo_color)),
        Span::raw("   "),
        Span::styled(iam_status, Style::default().fg(iam_color)),
        Span::raw("   "),
        Span::styled("v8.5.158", Style::default().fg(Color::DarkGray)),
    ]);

    f.render_widget(
        Paragraph::new(right_line).alignment(Alignment::Right),
        header_chunks[1],
    );

    f.render_widget(header_block, area);
}

fn render_view_tabs(f: &mut Frame, area: Rect, app: &App) {
    let titles = DashboardView::all()
        .iter()
        .map(|v| v.title())
        .collect::<Vec<_>>();

    let selected_idx = DashboardView::all()
        .iter()
        .position(|v| *v == app.current_view)
        .unwrap_or(0);

    let tabs = Tabs::new(titles)
        .select(selected_idx)
        .style(Style::default().fg(Color::DarkGray))
        .highlight_style(
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD)
                .add_modifier(Modifier::UNDERLINED),
        )
        .divider(Span::styled(" │ ", Style::default().fg(Color::DarkGray)));

    f.render_widget(tabs, area);
}

// ── [1] Sandboxes View ──────────────────────────────────────────────────────

fn render_sandboxes_view(f: &mut Frame, area: Rect, app: &App) {
    let main_chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(42), Constraint::Percentage(58)])
        .split(area);

    render_sandboxes_pane(f, main_chunks[0], app);
    render_detail_pane(f, main_chunks[1], app);
}

fn render_sandboxes_pane(f: &mut Frame, area: Rect, app: &App) {
    let is_focused = app.focused_pane == FocusedPane::Sandboxes;

    let outer_block = Block::default()
        .borders(Borders::RIGHT)
        .border_style(if is_focused {
            Style::default().fg(Color::Cyan)
        } else {
            Style::default().fg(Color::DarkGray)
        });

    let inner = outer_block.inner(area);
    f.render_widget(outer_block, area);

    if app.sandboxes.is_empty() {
        let empty_msg = Paragraph::new("No sandboxes. Press [c] to create one.")
            .style(Style::default().fg(Color::DarkGray))
            .alignment(Alignment::Center);
        f.render_widget(empty_msg, inner);
        return;
    }

    let card_height: u16 = 7;
    let max_visible = (inner.height / card_height).max(1) as usize;

    let start_idx = if app.selected_sandbox >= max_visible {
        app.selected_sandbox - max_visible + 1
    } else {
        0
    };

    let mut current_y = inner.y;
    for (idx, sbx) in app.sandboxes.iter().enumerate().skip(start_idx).take(max_visible) {
        let is_selected = idx == app.selected_sandbox;

        if current_y + card_height > inner.y + inner.height {
            break;
        }

        let card_rect = Rect {
            x: inner.x,
            y: current_y,
            width: inner.width.saturating_sub(1),
            height: card_height,
        };

        render_sandbox_card(f, card_rect, sbx, is_selected, is_focused);
        current_y += card_height;
    }
}

fn render_sandbox_card(
    f: &mut Frame,
    area: Rect,
    sbx: &Sandbox,
    is_selected: bool,
    pane_focused: bool,
) {
    let (border_color, border_type) = if is_selected && pane_focused {
        (Color::White, BorderType::Rounded)
    } else if is_selected {
        (Color::Cyan, BorderType::Rounded)
    } else {
        (Color::Rgb(60, 60, 75), BorderType::Rounded)
    };

    let title_line = Line::from(vec![
        Span::styled(
            " Sandbox ",
            Style::default()
                .bg(Color::Rgb(65, 45, 110))
                .fg(Color::Rgb(215, 190, 255))
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(" "),
        Span::styled("v§", Style::default().fg(Color::Rgb(90, 230, 160))),
        Span::raw(" "),
    ]);

    let block = Block::default()
        .title(title_line.alignment(Alignment::Right))
        .borders(Borders::ALL)
        .border_type(border_type)
        .border_style(Style::default().fg(border_color));

    let card_inner = block.inner(area);
    f.render_widget(block, area);

    let (status_dot, dot_color) = match sbx.status {
        SandboxStatus::Running => ("●", Color::Rgb(90, 240, 150)),
        SandboxStatus::Stopped => ("■", Color::Rgb(180, 120, 60)),
    };

    let line1 = Line::from(vec![
        Span::styled(format!("{status_dot} "), Style::default().fg(dot_color)),
        Span::styled(
            &sbx.name,
            if is_selected {
                Style::default().fg(Color::White).add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::White)
            },
        ),
    ]);

    let line2 = Line::from(vec![
        Span::styled(&sbx.agent, Style::default().fg(Color::Rgb(200, 205, 215))),
    ]);

    let line3 = Line::from(vec![
        Span::styled(&sbx.path, Style::default().fg(Color::Rgb(120, 125, 140))),
    ]);

    let line4 = match sbx.status {
        SandboxStatus::Running => Line::from(vec![
            Span::styled(
                format!(
                    "{}%/{}c · {} · {} · {}",
                    sbx.telemetry.cpu_percent,
                    sbx.telemetry.cpu_cores,
                    sbx.telemetry.memory,
                    sbx.telemetry.disk,
                    sbx.telemetry.uptime,
                ),
                Style::default().fg(Color::Rgb(170, 175, 185)),
            ),
            Span::raw("   "),
            Span::styled("● run", Style::default().fg(Color::Rgb(90, 230, 180))),
        ]),
        SandboxStatus::Stopped => Line::from(vec![
            Span::styled("stopped", Style::default().fg(Color::Rgb(180, 90, 90))),
        ]),
    };

    let line5 = match sbx.status {
        SandboxStatus::Running => Line::from(vec![
            Span::styled("stop", Style::default().fg(Color::Rgb(255, 185, 70)).add_modifier(Modifier::BOLD)),
            Span::raw("  "),
            Span::styled("exec", Style::default().fg(Color::Rgb(190, 195, 205))),
            Span::raw("  "),
            Span::styled("remove", Style::default().fg(Color::Rgb(255, 95, 125))),
        ]),
        SandboxStatus::Stopped => Line::from(vec![
            Span::styled("start", Style::default().fg(Color::Rgb(90, 230, 150)).add_modifier(Modifier::BOLD)),
            Span::raw("  "),
            Span::styled("exec", Style::default().fg(Color::Rgb(140, 145, 155))),
            Span::raw("  "),
            Span::styled("remove", Style::default().fg(Color::Rgb(255, 95, 125))),
        ]),
    };

    let card_content = Paragraph::new(vec![line1, line2, line3, line4, line5]);
    f.render_widget(card_content, card_inner);
}

fn render_detail_pane(f: &mut Frame, area: Rect, app: &App) {
    let is_focused = app.focused_pane == FocusedPane::Detail;

    let detail_block = Block::default()
        .borders(Borders::NONE)
        .style(Style::default());

    let inner = detail_block.inner(area);
    f.render_widget(detail_block, area);

    let Some(sbx) = app.current_sandbox() else {
        let no_sel = Paragraph::new("No sandbox selected")
            .style(Style::default().fg(Color::DarkGray))
            .alignment(Alignment::Center);
        f.render_widget(no_sel, inner);
        return;
    };

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(2),
            Constraint::Length(2),
            Constraint::Length(1),
            Constraint::Min(6),
            Constraint::Length(2),
        ])
        .split(inner);

    let title_line = Line::from(vec![
        Span::styled(
            &sbx.name,
            Style::default().fg(Color::White).add_modifier(Modifier::BOLD),
        ),
    ]);
    f.render_widget(Paragraph::new(title_line), chunks[0]);

    let tab_titles = vec![
        format!("Network Log ({})", sbx.network_logs.len()),
        format!("Global Network Rules ({})", app.global_rules.len()),
    ];
    let tab_idx = match app.active_tab {
        DetailTab::NetworkLog => 0,
        DetailTab::GlobalRules => 1,
    };
    let tabs = Tabs::new(tab_titles)
        .select(tab_idx)
        .style(Style::default().fg(Color::DarkGray))
        .highlight_style(
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD)
                .add_modifier(Modifier::UNDERLINED),
        )
        .divider(Span::raw("    "));
    f.render_widget(tabs, chunks[1]);

    match app.active_tab {
        DetailTab::NetworkLog => render_network_log_tab(f, chunks[2], chunks[3], chunks[4], sbx, app, is_focused),
        DetailTab::GlobalRules => render_global_rules_tab(f, chunks[2], chunks[3], chunks[4], app, is_focused),
    }
}

fn render_network_log_tab(
    f: &mut Frame,
    summary_area: Rect,
    table_area: Rect,
    hint_area: Rect,
    sbx: &Sandbox,
    app: &App,
    is_focused: bool,
) {
    let (total, allowed, blocked) = app.network_metrics();
    let summary = Line::from(vec![
        Span::styled(format!("{total}"), Style::default().fg(Color::White)),
        Span::styled(" total  ·  ", Style::default().fg(Color::DarkGray)),
        Span::styled("● ", Style::default().fg(Color::Rgb(90, 240, 150))),
        Span::styled(format!("{allowed} allowed"), Style::default().fg(Color::Rgb(90, 240, 150))),
        Span::styled("  ·  ", Style::default().fg(Color::DarkGray)),
        Span::styled("● ", Style::default().fg(Color::Rgb(255, 80, 120))),
        Span::styled(format!("{blocked} blocked"), Style::default().fg(Color::Rgb(255, 80, 120))),
    ]);
    f.render_widget(Paragraph::new(summary), summary_area);

    let header = Row::new(vec![
        Cell::from(Span::styled("Date ↑", Style::default().fg(Color::Rgb(140, 145, 155)))),
        Cell::from(Span::styled("Host", Style::default().fg(Color::Rgb(140, 145, 155)))),
        Cell::from(Span::styled("Hits", Style::default().fg(Color::Rgb(140, 145, 155)))),
        Cell::from(Span::styled("Status", Style::default().fg(Color::Rgb(140, 145, 155)))),
    ])
    .bottom_margin(1);

    let rows: Vec<Row> = sbx
        .network_logs
        .iter()
        .enumerate()
        .map(|(idx, entry)| {
            let is_row_selected = is_focused && idx == app.selected_network_row;

            let (status_text, status_color) = match entry.status {
                NetworkStatus::Allowed => ("Allowed", Color::Rgb(90, 240, 150)),
                NetworkStatus::Blocked => ("Blocked", Color::Rgb(255, 80, 120)),
            };

            let (dot, dot_color) = match entry.status {
                NetworkStatus::Allowed => ("●", Color::Rgb(90, 240, 150)),
                NetworkStatus::Blocked => ("●", Color::Rgb(255, 80, 120)),
            };

            let date_span = Line::from(vec![
                Span::styled(format!("{dot} "), Style::default().fg(dot_color)),
                Span::styled(&entry.date, Style::default().fg(Color::Rgb(170, 175, 185))),
            ]);

            let host_span = if is_row_selected {
                Line::from(vec![
                    Span::styled(&entry.host, Style::default().fg(Color::White).add_modifier(Modifier::BOLD)),
                    Span::raw("    "),
                    match entry.status {
                        NetworkStatus::Allowed => Span::styled("block", Style::default().fg(Color::Rgb(255, 95, 140)).add_modifier(Modifier::BOLD)),
                        NetworkStatus::Blocked => Span::styled("allow", Style::default().fg(Color::Rgb(90, 240, 150)).add_modifier(Modifier::BOLD)),
                    },
                ])
            } else {
                Line::from(vec![
                    Span::styled(&entry.host, Style::default().fg(Color::Rgb(220, 225, 235))),
                ])
            };

            let row_style = if is_row_selected {
                Style::default().bg(Color::Rgb(30, 35, 48)).fg(Color::White)
            } else {
                Style::default().fg(Color::White)
            };

            Row::new(vec![
                Cell::from(date_span),
                Cell::from(host_span),
                Cell::from(Span::styled(entry.hits.to_string(), Style::default().fg(Color::Rgb(160, 165, 175)))),
                Cell::from(Span::styled(status_text, Style::default().fg(status_color))),
            ])
            .style(row_style)
        })
        .collect();

    let widths = [
        Constraint::Length(22),
        Constraint::Min(32),
        Constraint::Length(8),
        Constraint::Length(10),
    ];

    let table = Table::new(rows, widths)
        .header(header)
        .column_spacing(2);

    f.render_widget(table, table_area);

    let summary_bottom = Line::from(vec![
        Span::styled(format!("{allowed} allowed entries  ·  {blocked} blocked entries"), Style::default().fg(Color::DarkGray)),
    ]);
    f.render_widget(Paragraph::new(summary_bottom), hint_area);
}

fn render_global_rules_tab(
    f: &mut Frame,
    summary_area: Rect,
    table_area: Rect,
    hint_area: Rect,
    app: &App,
    is_focused: bool,
) {
    let summary = Line::from(vec![
        Span::styled("Default egress policy: ", Style::default().fg(Color::DarkGray)),
        Span::styled(
            "ZERO-TRUST DENY",
            Style::default().fg(Color::LightRed).add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            " · Only allowlisted outbound endpoints are reachable",
            Style::default().fg(Color::DarkGray),
        ),
    ]);
    f.render_widget(Paragraph::new(summary), summary_area);

    let header = Row::new(vec![
        Span::styled("Rule Pattern", Style::default().fg(Color::DarkGray)),
        Span::styled("Scope", Style::default().fg(Color::DarkGray)),
        Span::styled("Action", Style::default().fg(Color::DarkGray)),
        Span::styled("Hits", Style::default().fg(Color::DarkGray)),
    ])
    .bottom_margin(1);

    let rows: Vec<Row> = app
        .global_rules
        .iter()
        .enumerate()
        .map(|(idx, rule)| {
            let is_row_selected = is_focused && idx == app.selected_rule_row;

            let (action_text, action_color) = match rule.action {
                NetworkStatus::Allowed => ("Allow", Color::Green),
                NetworkStatus::Blocked => ("Block (Deny)", Color::Red),
            };

            let row_style = if is_row_selected {
                Style::default().bg(Color::Rgb(30, 40, 50)).fg(Color::White)
            } else {
                Style::default().fg(Color::White)
            };

            Row::new(vec![
                Span::styled(
                    &rule.pattern,
                    if is_row_selected {
                        Style::default().fg(Color::White).add_modifier(Modifier::BOLD)
                    } else {
                        Style::default().fg(Color::White)
                    },
                ),
                Span::styled(&rule.rule_type, Style::default().fg(Color::Cyan)),
                Span::styled(
                    action_text,
                    Style::default().fg(action_color).add_modifier(Modifier::BOLD),
                ),
                Span::styled(rule.hits.to_string(), Style::default().fg(Color::Gray)),
            ])
            .style(row_style)
        })
        .collect();

    let widths = [
        Constraint::Min(24),
        Constraint::Length(14),
        Constraint::Length(16),
        Constraint::Length(8),
    ];

    let table = Table::new(rows, widths)
        .header(header)
        .column_spacing(2);

    f.render_widget(table, table_area);

    let hint = Line::from(vec![
        Span::styled("[b]", Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)),
        Span::styled(
            " toggle rule allow/block   ",
            Style::default().fg(Color::DarkGray),
        ),
        Span::styled("[n]", Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)),
        Span::styled(" switch tab", Style::default().fg(Color::DarkGray)),
    ]);
    f.render_widget(Paragraph::new(hint), hint_area);
}

// ── [2] Grid & Nodes View ───────────────────────────────────────────────────

fn render_grid_nodes_view(f: &mut Frame, area: Rect, app: &App) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3), // Top KPI metrics
            Constraint::Length(7), // Grid nodes table
            Constraint::Min(6),    // Runners & system units
        ])
        .split(area);

    // KPI Cards
    let kpi_chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage(25),
            Constraint::Percentage(25),
            Constraint::Percentage(25),
            Constraint::Percentage(25),
        ])
        .split(chunks[0]);

    let online_nodes = app.grid_nodes.iter().filter(|n| n.is_online).count();
    let total_nodes = app.grid_nodes.len();
    let nodes_summary = format!("{} Online / {} Discovered", online_nodes, total_nodes);

    let (total_inflight, total_queued, total_prefill, total_decode) = if let Some(t) = &app.cluster_telemetry {
        let inf: usize = t.nodes.iter().map(|n| n.in_flight).sum();
        let q: usize = t.nodes.iter().map(|n| n.queued).sum();
        let p: f64 = t.nodes.iter().map(|n| n.prefill_tok_s).sum();
        let d: f64 = t.nodes.iter().map(|n| n.decode_tok_s).sum();
        (inf, q, p, d)
    } else {
        (5, 5, 3906.0, 19.6)
    };

    let workload_str = format!("{} In-Flight ({} Queued)", total_inflight, total_queued);
    let throughput_str = format!("{:.0} prefill · {:.1} dec", total_prefill, total_decode);

    render_kpi_card(f, kpi_chunks[0], "Grid Nodes", &nodes_summary, Color::Green);
    render_kpi_card(f, kpi_chunks[1], "Compute Fleet", "88 Cores · 249GB VRAM", Color::Cyan);
    render_kpi_card(f, kpi_chunks[2], "Active Workloads", &workload_str, Color::Yellow);
    render_kpi_card(f, kpi_chunks[3], "Cluster Throughput", &throughput_str, Color::White);

    // Grid Nodes Table
    let node_block = Block::default()
        .title(Span::styled(
            " Grid Nodes & Compute Topology ",
            Style::default().fg(Color::White).add_modifier(Modifier::BOLD),
        ))
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(Color::DarkGray));

    let header = Row::new(vec![
        Span::styled("Node Name", Style::default().fg(Color::DarkGray)),
        Span::styled("Role", Style::default().fg(Color::DarkGray)),
        Span::styled("Address", Style::default().fg(Color::DarkGray)),
        Span::styled("CPU Cores / Load", Style::default().fg(Color::DarkGray)),
        Span::styled("Memory Usage", Style::default().fg(Color::DarkGray)),
        Span::styled("Disk Free", Style::default().fg(Color::DarkGray)),
        Span::styled("Status", Style::default().fg(Color::DarkGray)),
        Span::styled("Uptime", Style::default().fg(Color::DarkGray)),
    ])
    .bottom_margin(1);

    let rows: Vec<Row> = app
        .grid_nodes
        .iter()
        .enumerate()
        .map(|(idx, node)| {
            let is_sel = idx == app.selected_node_row;
            let row_style = if is_sel {
                Style::default().bg(Color::Rgb(30, 40, 50)).fg(Color::White)
            } else {
                Style::default().fg(Color::White)
            };

            Row::new(vec![
                Span::styled(&node.name, Style::default().fg(Color::White).add_modifier(Modifier::BOLD)),
                Span::styled(&node.role, Style::default().fg(Color::Cyan)),
                Span::styled(&node.address, Style::default().fg(Color::DarkGray)),
                Span::styled(&node.cpu, Style::default().fg(Color::Gray)),
                Span::styled(&node.memory, Style::default().fg(Color::Gray)),
                Span::styled(&node.disk, Style::default().fg(Color::DarkGray)),
                Span::styled(
                    &node.status,
                    Style::default().fg(if node.is_online { Color::Green } else { Color::DarkGray }),
                ),
                Span::styled(&node.uptime, Style::default().fg(Color::DarkGray)),
            ])
            .style(row_style)
        })
        .collect();

    let widths = [
        Constraint::Length(16),
        Constraint::Min(22),
        Constraint::Length(14),
        Constraint::Length(18),
        Constraint::Length(18),
        Constraint::Length(12),
        Constraint::Length(16),
        Constraint::Length(8),
    ];

    let table = Table::new(rows, widths).header(header).column_spacing(2);
    f.render_widget(table, node_block.inner(chunks[1]));
    f.render_widget(node_block, chunks[1]);

    // Runners & Background Services Table
    let runner_block = Block::default()
        .title(Span::styled(
            " Fleet Runners & Background Units ",
            Style::default().fg(Color::White).add_modifier(Modifier::BOLD),
        ))
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(Color::DarkGray));

    let r_header = Row::new(vec![
        Span::styled("Unit / Runner", Style::default().fg(Color::DarkGray)),
        Span::styled("Host", Style::default().fg(Color::DarkGray)),
        Span::styled("Type", Style::default().fg(Color::DarkGray)),
        Span::styled("Allocation / Guard", Style::default().fg(Color::DarkGray)),
        Span::styled("Status", Style::default().fg(Color::DarkGray)),
        Span::styled("Details", Style::default().fg(Color::DarkGray)),
    ])
    .bottom_margin(1);

    let r_rows: Vec<Row> = app
        .runners
        .iter()
        .map(|r| {
            Row::new(vec![
                Span::styled(&r.name, Style::default().fg(Color::White)),
                Span::styled(&r.host, Style::default().fg(Color::DarkGray)),
                Span::styled(&r.runner_type, Style::default().fg(Color::Cyan)),
                Span::styled(&r.allocation, Style::default().fg(Color::Gray)),
                Span::styled(&r.status, Style::default().fg(Color::Green)),
                Span::styled(&r.detail, Style::default().fg(Color::DarkGray)),
            ])
        })
        .collect();

    let r_widths = [
        Constraint::Length(22),
        Constraint::Length(22),
        Constraint::Length(22),
        Constraint::Min(24),
        Constraint::Length(18),
        Constraint::Length(26),
    ];

    let r_table = Table::new(r_rows, r_widths).header(r_header).column_spacing(2);
    f.render_widget(r_table, runner_block.inner(chunks[2]));
    f.render_widget(runner_block, chunks[2]);
}

// ── [3] Local Models View ───────────────────────────────────────────────────

fn render_local_models_view(f: &mut Frame, area: Rect, app: &App) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3), // Top KPI
            Constraint::Length(9), // Models table
            Constraint::Min(6),    // Active Engine & Diagnostic Panel
        ])
        .split(area);

    let kpi_chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage(25),
            Constraint::Percentage(25),
            Constraint::Percentage(25),
            Constraint::Percentage(25),
        ])
        .split(chunks[0]);

    let (total_prefill, total_decode) = if let Some(t) = &app.cluster_telemetry {
        let p: f64 = t.nodes.iter().map(|n| n.prefill_tok_s).sum();
        let d: f64 = t.nodes.iter().map(|n| n.decode_tok_s).sum();
        (p, d)
    } else {
        (3906.0, 19.6)
    };

    let speed_summary = format!("{:.0} prefill · {:.1} gen t/s", total_prefill, total_decode);
    render_kpi_card(f, kpi_chunks[0], "Active Model", "qwen3.8-flash-next", Color::Green);
    render_kpi_card(f, kpi_chunks[1], "Context Length", "1,000,000 (1M) / 262k", Color::Cyan);
    render_kpi_card(f, kpi_chunks[2], "Inference Engines", "vLLM NVFP4 + Halogen ROCm", Color::Yellow);
    render_kpi_card(f, kpi_chunks[3], "Cluster Speed", &speed_summary, Color::White);

    // Models Table
    let model_block = Block::default()
        .title(Span::styled(
            " Local Models Inventory (OpenAI-compatible & Zen Endpoints) ",
            Style::default().fg(Color::White).add_modifier(Modifier::BOLD),
        ))
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(Color::DarkGray));

    let header = Row::new(vec![
        Span::styled("Model ID", Style::default().fg(Color::DarkGray)),
        Span::styled("Backend Engine", Style::default().fg(Color::DarkGray)),
        Span::styled("Target Node", Style::default().fg(Color::DarkGray)),
        Span::styled("Params", Style::default().fg(Color::DarkGray)),
        Span::styled("Context", Style::default().fg(Color::DarkGray)),
        Span::styled("Quantization", Style::default().fg(Color::DarkGray)),
        Span::styled("Memory Footprint", Style::default().fg(Color::DarkGray)),
        Span::styled("Speed", Style::default().fg(Color::DarkGray)),
        Span::styled("Status", Style::default().fg(Color::DarkGray)),
        Span::styled("Endpoint", Style::default().fg(Color::DarkGray)),
    ])
    .bottom_margin(1);

    let rows: Vec<Row> = app
        .local_models
        .iter()
        .enumerate()
        .map(|(idx, m)| {
            let is_sel = idx == app.selected_model_row;
            let row_style = if is_sel {
                Style::default().bg(Color::Rgb(30, 40, 50)).fg(Color::White)
            } else {
                Style::default().fg(Color::White)
            };

            let status_color = if m.is_active { Color::Green } else { Color::DarkGray };
            let node_color = if m.target_node.contains("spark") { Color::Cyan } else { Color::Yellow };

            Row::new(vec![
                Span::styled(&m.id, Style::default().fg(Color::White).add_modifier(Modifier::BOLD)),
                Span::styled(&m.backend, Style::default().fg(Color::Cyan)),
                Span::styled(&m.target_node, Style::default().fg(node_color)),
                Span::styled(&m.parameters, Style::default().fg(Color::Gray)),
                Span::styled(&m.context_window, Style::default().fg(Color::Gray)),
                Span::styled(&m.quantization, Style::default().fg(Color::DarkGray)),
                Span::styled(&m.memory, Style::default().fg(Color::Gray)),
                Span::styled(&m.speed, Style::default().fg(Color::Gray)),
                Span::styled(&m.status, Style::default().fg(status_color)),
                Span::styled(&m.endpoint, Style::default().fg(Color::DarkGray)),
            ])
            .style(row_style)
        })
        .collect();

    let widths = [
        Constraint::Min(24),
        Constraint::Length(16),
        Constraint::Length(18),
        Constraint::Length(8),
        Constraint::Length(12),
        Constraint::Length(10),
        Constraint::Length(14),
        Constraint::Length(9),
        Constraint::Length(16),
        Constraint::Length(22),
    ];

    let table = Table::new(rows, widths).header(header).column_spacing(2);
    f.render_widget(table, model_block.inner(chunks[1]));
    f.render_widget(model_block, chunks[1]);

    // Model Architecture Details & HIP Policies
    let diag_block = Block::default()
        .title(Span::styled(
            " Zen Engine Specifications & Deployment Controls ",
            Style::default().fg(Color::White).add_modifier(Modifier::BOLD),
        ))
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(Color::DarkGray));

    let diag_inner = diag_block.inner(chunks[2]);
    let diag_text = vec![
        Line::from(vec![
            Span::styled("Deployment Target:  ", Style::default().fg(Color::DarkGray)),
            Span::styled(&app.target_node, Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
            Span::styled("   [ 'n': toggle node · 'd': pull via `hf` · 'l'/Enter: load VRAM ]", Style::default().fg(Color::Cyan)),
        ]),
        Line::from(vec![
            Span::styled("Model Architecture: ", Style::default().fg(Color::DarkGray)),
            Span::styled("Qwen 3.8 Flash-Next with 1,000,000 (DGX vLLM) & 262,144 (Evo Halogen) context", Style::default().fg(Color::White)),
        ]),
        Line::from(vec![
            Span::styled("Inference Routing:  ", Style::default().fg(Color::DarkGray)),
            Span::styled("hanzo-router (127.0.0.1:1235) -> DGX Blackwell (:18300) & Evo Strix Halo (:8731)", Style::default().fg(Color::Cyan)),
        ]),
        Line::from(vec![
            Span::styled("Speculative Head:   ", Style::default().fg(Color::DarkGray)),
            Span::styled("2-token MTP speculative decoding active on DGX (62.4% acceptance) & Halogen (59.2%)", Style::default().fg(Color::Yellow)),
        ]),
        Line::from(vec![
            Span::styled("ENGRAFT Overlays:   ", Style::default().fg(Color::DarkGray)),
            Span::styled("Per-request .pleo token-addressed memory overlays supported with prefix-cache affinity", Style::default().fg(Color::Green)),
        ]),
    ];
    f.render_widget(Paragraph::new(diag_text), diag_inner);
    f.render_widget(diag_block, chunks[2]);
}

// ── [4] Cloud Services View ─────────────────────────────────────────────────

fn render_cloud_services_view(f: &mut Frame, area: Rect, app: &App) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3), // Top KPI
            Constraint::Min(10),   // Services Table
        ])
        .split(area);

    let kpi_chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage(25),
            Constraint::Percentage(25),
            Constraint::Percentage(25),
            Constraint::Percentage(25),
        ])
        .split(chunks[0]);

    render_kpi_card(f, kpi_chunks[0], "Cloud Services", "7 Services Online", Color::Green);
    render_kpi_card(f, kpi_chunks[1], "Wire Protocol", "Native ZAP + HTTP/2", Color::Cyan);
    render_kpi_card(f, kpi_chunks[2], "Container Engine", "Hanzo MicroVM (k3s)", Color::Yellow);
    render_kpi_card(f, kpi_chunks[3], "Average Latency", "0.58 ms (loopback)", Color::White);

    let svc_block = Block::default()
        .title(Span::styled(
            " Hanzo Open AI Cloud Subsystems & Unified Runtimes ",
            Style::default().fg(Color::White).add_modifier(Modifier::BOLD),
        ))
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(Color::DarkGray));

    let header = Row::new(vec![
        Span::styled("Service", Style::default().fg(Color::DarkGray)),
        Span::styled("Subsystem", Style::default().fg(Color::DarkGray)),
        Span::styled("Port / Socket", Style::default().fg(Color::DarkGray)),
        Span::styled("Protocol", Style::default().fg(Color::DarkGray)),
        Span::styled("Latency", Style::default().fg(Color::DarkGray)),
        Span::styled("Status", Style::default().fg(Color::DarkGray)),
        Span::styled("Description", Style::default().fg(Color::DarkGray)),
    ])
    .bottom_margin(1);

    let rows: Vec<Row> = app
        .cloud_services
        .iter()
        .enumerate()
        .map(|(idx, svc)| {
            let is_sel = idx == app.selected_service_row;
            let row_style = if is_sel {
                Style::default().bg(Color::Rgb(30, 40, 50)).fg(Color::White)
            } else {
                Style::default().fg(Color::White)
            };

            Row::new(vec![
                Span::styled(&svc.name, Style::default().fg(Color::White).add_modifier(Modifier::BOLD)),
                Span::styled(&svc.subsystem, Style::default().fg(Color::Cyan)),
                Span::styled(&svc.port_or_socket, Style::default().fg(Color::DarkGray)),
                Span::styled(&svc.protocol, Style::default().fg(Color::Gray)),
                Span::styled(&svc.latency, Style::default().fg(Color::Gray)),
                Span::styled(&svc.status, Style::default().fg(Color::Green)),
                Span::styled(&svc.description, Style::default().fg(Color::DarkGray)),
            ])
            .style(row_style)
        })
        .collect();

    let widths = [
        Constraint::Length(14),
        Constraint::Length(22),
        Constraint::Length(20),
        Constraint::Length(18),
        Constraint::Length(10),
        Constraint::Length(12),
        Constraint::Min(30),
    ];

    let table = Table::new(rows, widths).header(header).column_spacing(2);
    f.render_widget(table, svc_block.inner(chunks[1]));
    f.render_widget(svc_block, chunks[1]);
}

// ── [5] Usage & Quota View ──────────────────────────────────────────────────

fn render_usage_view(f: &mut Frame, area: Rect, app: &App) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3), // Top KPI
            Constraint::Length(8), // Usage Table with Bars
            Constraint::Min(6),    // Throughput & Latency Gauges
        ])
        .split(area);

    let kpi_chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage(25),
            Constraint::Percentage(25),
            Constraint::Percentage(25),
            Constraint::Percentage(25),
        ])
        .split(chunks[0]);

    render_kpi_card(f, kpi_chunks[0], "Monthly Inferences", "28,492 Requests", Color::Green);
    render_kpi_card(f, kpi_chunks[1], "Tokens Consumed", "4.82M Tokens (Total)", Color::Cyan);
    render_kpi_card(f, kpi_chunks[2], "Billing Meter", "Enterprise Dedicated", Color::Yellow);
    render_kpi_card(f, kpi_chunks[3], "Credits Remaining", "$328.40 USD", Color::White);

    let usage_block = Block::default()
        .title(Span::styled(
            " Subsystem Quotas & Consumption Metrics ",
            Style::default().fg(Color::White).add_modifier(Modifier::BOLD),
        ))
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(Color::DarkGray));

    let header = Row::new(vec![
        Span::styled("Category", Style::default().fg(Color::DarkGray)),
        Span::styled("Metric Description", Style::default().fg(Color::DarkGray)),
        Span::styled("Consumed", Style::default().fg(Color::DarkGray)),
        Span::styled("Monthly Quota", Style::default().fg(Color::DarkGray)),
        Span::styled("Utilization Bar", Style::default().fg(Color::DarkGray)),
        Span::styled("Trend", Style::default().fg(Color::DarkGray)),
    ])
    .bottom_margin(1);

    let rows: Vec<Row> = app
        .usage_items
        .iter()
        .enumerate()
        .map(|(idx, u)| {
            let is_sel = idx == app.selected_usage_row;
            let row_style = if is_sel {
                Style::default().bg(Color::Rgb(30, 40, 50)).fg(Color::White)
            } else {
                Style::default().fg(Color::White)
            };

            let bar_filled = (u.percentage as usize / 5).min(20);
            let bar_empty = 20usize.saturating_sub(bar_filled);
            let bar = format!("[{}{}] {}%", "█".repeat(bar_filled), "░".repeat(bar_empty), u.percentage);

            Row::new(vec![
                Span::styled(&u.category, Style::default().fg(Color::White).add_modifier(Modifier::BOLD)),
                Span::styled(&u.metric, Style::default().fg(Color::DarkGray)),
                Span::styled(&u.consumed, Style::default().fg(Color::Cyan)),
                Span::styled(&u.quota, Style::default().fg(Color::Gray)),
                Span::styled(bar, Style::default().fg(if u.percentage > 80 { Color::Red } else { Color::Green })),
                Span::styled(&u.trend, Style::default().fg(Color::DarkGray)),
            ])
            .style(row_style)
        })
        .collect();

    let widths = [
        Constraint::Length(22),
        Constraint::Min(24),
        Constraint::Length(16),
        Constraint::Length(18),
        Constraint::Length(26),
        Constraint::Length(14),
    ];

    let table = Table::new(rows, widths).header(header).column_spacing(2);
    f.render_widget(table, usage_block.inner(chunks[1]));
    f.render_widget(usage_block, chunks[1]);

    // Bottom Performance & Latency Telemetry
    let perf_block = Block::default()
        .title(Span::styled(
            " Realtime Throughput & SLA Metrics ",
            Style::default().fg(Color::White).add_modifier(Modifier::BOLD),
        ))
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(Color::DarkGray));

    let perf_inner = perf_block.inner(chunks[2]);
    let perf_lines = vec![
        Line::from(vec![
            Span::styled("Request Throughput: ", Style::default().fg(Color::DarkGray)),
            Span::styled("14.2 req/sec", Style::default().fg(Color::Green).add_modifier(Modifier::BOLD)),
            Span::raw("   "),
            Span::styled("P50 Latency: ", Style::default().fg(Color::DarkGray)),
            Span::styled("8.4 ms", Style::default().fg(Color::White)),
            Span::raw("   "),
            Span::styled("P95 Latency: ", Style::default().fg(Color::DarkGray)),
            Span::styled("21.6 ms", Style::default().fg(Color::White)),
            Span::raw("   "),
            Span::styled("P99 Latency: ", Style::default().fg(Color::DarkGray)),
            Span::styled("48.2 ms", Style::default().fg(Color::White)),
        ]),
        Line::from(vec![
            Span::styled("Error Rate:         ", Style::default().fg(Color::DarkGray)),
            Span::styled("0.00% (Zero dropped requests in last 24h)", Style::default().fg(Color::Green)),
            Span::raw("   "),
            Span::styled("Active Edge Conn: ", Style::default().fg(Color::DarkGray)),
            Span::styled("18 SSE persistent streams", Style::default().fg(Color::Cyan)),
        ]),
    ];
    f.render_widget(Paragraph::new(perf_lines), perf_inner);
    f.render_widget(perf_block, chunks[2]);
}

// ── Shared Widgets ──────────────────────────────────────────────────────────

fn render_kpi_card(f: &mut Frame, area: Rect, label: &str, value: &str, val_color: Color) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(Color::DarkGray));

    let inner = block.inner(area);
    f.render_widget(block, area);

    let text = vec![
        Line::from(vec![Span::styled(label, Style::default().fg(Color::DarkGray))]),
        Line::from(vec![Span::styled(
            value,
            Style::default().fg(val_color).add_modifier(Modifier::BOLD),
        )]),
    ];
    f.render_widget(Paragraph::new(text), inner);
}

fn render_footer(f: &mut Frame, area: Rect, app: &App) {
    let footer_chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Min(95), Constraint::Length(15)])
        .split(area);

    let keys = match app.current_view {
        DashboardView::Sandboxes => vec![
            Span::styled("^c", Style::default().fg(Color::Cyan)),
            Span::styled(" quit  ", Style::default().fg(Color::DarkGray)),
            Span::styled("←/→", Style::default().fg(Color::Cyan)),
            Span::styled(" panel  ", Style::default().fg(Color::DarkGray)),
            Span::styled("↑/↓", Style::default().fg(Color::Cyan)),
            Span::styled(" move  ", Style::default().fg(Color::DarkGray)),
            Span::styled("Enter", Style::default().fg(Color::Cyan)),
            Span::styled(" shell  ", Style::default().fg(Color::DarkGray)),
            Span::styled("l", Style::default().fg(Color::Cyan)),
            Span::styled(" launch  ", Style::default().fg(Color::DarkGray)),
            Span::styled("c", Style::default().fg(Color::Cyan)),
            Span::styled(" create  ", Style::default().fg(Color::DarkGray)),
            Span::styled("e", Style::default().fg(Color::Cyan)),
            Span::styled(" explore  ", Style::default().fg(Color::DarkGray)),
            Span::styled("n", Style::default().fg(Color::Cyan)),
            Span::styled(" net tab  ", Style::default().fg(Color::DarkGray)),
            Span::styled("a", Style::default().fg(Color::Cyan)),
            Span::styled(" all  ", Style::default().fg(Color::DarkGray)),
            Span::styled("b", Style::default().fg(Color::Cyan)),
            Span::styled(" block  ", Style::default().fg(Color::DarkGray)),
            Span::styled("s", Style::default().fg(Color::Cyan)),
            Span::styled(" start/stop", Style::default().fg(Color::DarkGray)),
        ],
        DashboardView::LocalModels => vec![
            Span::styled("^c/q", Style::default().fg(Color::Cyan)),
            Span::styled(" quit  ", Style::default().fg(Color::DarkGray)),
            Span::styled("1-5", Style::default().fg(Color::Cyan)),
            Span::styled(" views  ", Style::default().fg(Color::DarkGray)),
            Span::styled("[/]", Style::default().fg(Color::Cyan)),
            Span::styled(" tab  ", Style::default().fg(Color::DarkGray)),
            Span::styled("↑/↓", Style::default().fg(Color::Cyan)),
            Span::styled(" select  ", Style::default().fg(Color::DarkGray)),
            Span::styled("n", Style::default().fg(Color::Cyan)),
            Span::styled(" target node  ", Style::default().fg(Color::DarkGray)),
            Span::styled("d", Style::default().fg(Color::Cyan)),
            Span::styled(" hf download  ", Style::default().fg(Color::DarkGray)),
            Span::styled("l/Enter", Style::default().fg(Color::Cyan)),
            Span::styled(" load VRAM  ", Style::default().fg(Color::DarkGray)),
            Span::styled("e", Style::default().fg(Color::Cyan)),
            Span::styled(" explore  ", Style::default().fg(Color::DarkGray)),
            Span::styled("p", Style::default().fg(Color::Cyan)),
            Span::styled(" ping  ", Style::default().fg(Color::DarkGray)),
            Span::styled("r", Style::default().fg(Color::Cyan)),
            Span::styled(" refresh", Style::default().fg(Color::DarkGray)),
        ],
        DashboardView::GridNodes => vec![
            Span::styled("^c/q", Style::default().fg(Color::Cyan)),
            Span::styled(" quit  ", Style::default().fg(Color::DarkGray)),
            Span::styled("1-5", Style::default().fg(Color::Cyan)),
            Span::styled(" views  ", Style::default().fg(Color::DarkGray)),
            Span::styled("[/]", Style::default().fg(Color::Cyan)),
            Span::styled(" tab  ", Style::default().fg(Color::DarkGray)),
            Span::styled("↑/↓", Style::default().fg(Color::Cyan)),
            Span::styled(" select  ", Style::default().fg(Color::DarkGray)),
            Span::styled("n", Style::default().fg(Color::Cyan)),
            Span::styled(" target node  ", Style::default().fg(Color::DarkGray)),
            Span::styled("d", Style::default().fg(Color::Cyan)),
            Span::styled(" pull model  ", Style::default().fg(Color::DarkGray)),
            Span::styled("p", Style::default().fg(Color::Cyan)),
            Span::styled(" ping  ", Style::default().fg(Color::DarkGray)),
            Span::styled("r", Style::default().fg(Color::Cyan)),
            Span::styled(" refresh", Style::default().fg(Color::DarkGray)),
        ],
        _ => vec![
            Span::styled("^c/q", Style::default().fg(Color::Cyan)),
            Span::styled(" quit  ", Style::default().fg(Color::DarkGray)),
            Span::styled("1-5", Style::default().fg(Color::Cyan)),
            Span::styled(" views  ", Style::default().fg(Color::DarkGray)),
            Span::styled("[/]", Style::default().fg(Color::Cyan)),
            Span::styled(" prev/next  ", Style::default().fg(Color::DarkGray)),
            Span::styled("↑/↓", Style::default().fg(Color::Cyan)),
            Span::styled(" move  ", Style::default().fg(Color::DarkGray)),
            Span::styled("p", Style::default().fg(Color::Cyan)),
            Span::styled(" probe  ", Style::default().fg(Color::DarkGray)),
            Span::styled("r", Style::default().fg(Color::Cyan)),
            Span::styled(" refresh fleet", Style::default().fg(Color::DarkGray)),
        ],
    };

    f.render_widget(Paragraph::new(Line::from(keys)), footer_chunks[0]);

    let right_widget = if let Some(msg) = app.current_status() {
        Paragraph::new(Line::from(vec![
            Span::styled(msg, Style::default().fg(Color::Yellow)),
        ]))
        .alignment(Alignment::Right)
    } else {
        Paragraph::new(Line::from(vec![
            Span::styled("v8.5.158", Style::default().fg(Color::DarkGray)),
        ]))
        .alignment(Alignment::Right)
    };

    f.render_widget(right_widget, footer_chunks[1]);
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_app_initialization() {
        let app = App::new();
        assert!(!app.sandboxes.is_empty(), "seed sandboxes should be present");
        assert!(!app.grid_nodes.is_empty(), "grid nodes should be present");
        assert!(!app.local_models.is_empty(), "local models should be present");
        assert!(!app.cloud_services.is_empty(), "cloud services should be present");
        assert!(!app.usage_items.is_empty(), "usage items should be present");
        assert_eq!(app.current_view, DashboardView::Sandboxes);
        assert_eq!(app.selected_sandbox, 0);
        assert_eq!(app.active_tab, DetailTab::NetworkLog);
        assert_eq!(app.focused_pane, FocusedPane::Sandboxes);
    }

    #[test]
    fn test_dashboard_view_switching() {
        let mut app = App::new();
        assert_eq!(app.current_view, DashboardView::Sandboxes);

        app.set_view(DashboardView::GridNodes);
        assert_eq!(app.current_view, DashboardView::GridNodes);

        app.set_view(DashboardView::LocalModels);
        assert_eq!(app.current_view, DashboardView::LocalModels);

        app.set_view(DashboardView::CloudServices);
        assert_eq!(app.current_view, DashboardView::CloudServices);

        app.set_view(DashboardView::Usage);
        assert_eq!(app.current_view, DashboardView::Usage);

        app.next_view();
        assert_eq!(app.current_view, DashboardView::Sandboxes);

        app.previous_view();
        assert_eq!(app.current_view, DashboardView::Usage);
    }

    #[test]
    fn test_navigation_across_views() {
        let mut app = App::new();

        // GridNodes navigation
        app.set_view(DashboardView::GridNodes);
        assert_eq!(app.selected_node_row, 0);
        app.move_down();
        assert_eq!(app.selected_node_row, 1);
        app.move_up();
        assert_eq!(app.selected_node_row, 0);

        // LocalModels navigation
        app.set_view(DashboardView::LocalModels);
        assert_eq!(app.selected_model_row, 0);
        app.move_down();
        assert_eq!(app.selected_model_row, 1);
        app.move_up();
        assert_eq!(app.selected_model_row, 0);

        // CloudServices navigation
        app.set_view(DashboardView::CloudServices);
        assert_eq!(app.selected_service_row, 0);
        app.move_down();
        assert_eq!(app.selected_service_row, 1);
        app.move_up();
        assert_eq!(app.selected_service_row, 0);

        // Usage navigation
        app.set_view(DashboardView::Usage);
        assert_eq!(app.selected_usage_row, 0);
        app.move_down();
        assert_eq!(app.selected_usage_row, 1);
        app.move_up();
        assert_eq!(app.selected_usage_row, 0);
    }

    #[test]
    fn test_sandbox_navigation() {
        let mut app = App::new();
        let total = app.sandboxes.len();
        assert!(total >= 2);

        app.next_sandbox();
        assert_eq!(app.selected_sandbox, 1);

        app.previous_sandbox();
        assert_eq!(app.selected_sandbox, 0);

        app.previous_sandbox();
        assert_eq!(app.selected_sandbox, total - 1);
    }

    #[test]
    fn test_pane_and_tab_switching() {
        let mut app = App::new();
        assert_eq!(app.focused_pane, FocusedPane::Sandboxes);
        app.switch_pane();
        assert_eq!(app.focused_pane, FocusedPane::Detail);
        app.switch_pane();
        assert_eq!(app.focused_pane, FocusedPane::Sandboxes);

        assert_eq!(app.active_tab, DetailTab::NetworkLog);
        app.toggle_active_tab();
        assert_eq!(app.active_tab, DetailTab::GlobalRules);
        app.toggle_active_tab();
        assert_eq!(app.active_tab, DetailTab::NetworkLog);
    }

    #[test]
    fn test_toggle_start_stop() {
        let mut app = App::new();
        let initial_status = app.sandboxes[0].status.clone();

        app.toggle_start_stop();
        assert_ne!(app.sandboxes[0].status, initial_status);

        app.toggle_start_stop();
        assert_eq!(app.sandboxes[0].status, initial_status);
    }

    #[test]
    fn test_toggle_network_block() {
        let mut app = App::new();
        let initial_status = app.sandboxes[0].network_logs[0].status;

        app.toggle_block();
        assert_ne!(app.sandboxes[0].network_logs[0].status, initial_status);

        app.toggle_block();
        assert_eq!(app.sandboxes[0].network_logs[0].status, initial_status);
    }

    #[test]
    fn test_create_and_remove_sandbox() {
        let mut app = App::new();
        let initial_count = app.sandboxes.len();

        app.create_sandbox();
        assert_eq!(app.sandboxes.len(), initial_count + 1);
        assert_eq!(app.selected_sandbox, initial_count);

        app.remove_selected();
        assert_eq!(app.sandboxes.len(), initial_count);
    }

    #[test]
    fn test_network_metrics_calculation() {
        let app = App::new();
        let (total, allowed, blocked) = app.network_metrics();
        assert_eq!(total, allowed + blocked);
        assert!(total > 0);
    }

    #[test]
    fn test_refresh_all() {
        let mut app = App::new();
        app.refresh_all();
        assert!(!app.grid_nodes.is_empty());
        assert!(!app.local_models.is_empty());
        assert!(!app.cloud_services.is_empty());
    }

    #[test]
    fn test_all_agents_running() {
        let mut app = App::new();
        app.start_all_agents();
        assert!(app.sandboxes.iter().all(|s| s.status == SandboxStatus::Running));
        assert!(app.sandboxes.iter().any(|s| s.agent == "Claude Code"));
        assert!(app.sandboxes.iter().any(|s| s.agent == "Hanzo Dev"));
        assert!(app.sandboxes.iter().any(|s| s.agent == "Codex"));
        assert!(app.sandboxes.iter().any(|s| s.agent == "Zen Coder"));
    }

    #[test]
    fn test_model_and_container_actions() {
        let mut app = App::new();
        let initial_node = app.target_node.clone();
        app.toggle_target_node();
        assert_ne!(app.target_node, initial_node);
        app.toggle_target_node();
        assert_eq!(app.target_node, initial_node);

        app.download_selected_model();
        assert_eq!(app.local_models[app.selected_model_row].status, "↓ Downloading...");

        app.launch_selected_model();
        assert!(app.local_models[app.selected_model_row].is_active);
        assert_eq!(app.local_models[app.selected_model_row].status, "● Loaded (Active)");

        let initial_count = app.sandboxes.len();
        app.launch_container_sandbox();
        assert_eq!(app.sandboxes.len(), initial_count + 1);

        app.explore_catalog();
        assert!(app.current_status().unwrap_or_default().contains("Catalog:"));
    }

    #[test]
    fn test_dump_tui_for_screenshot() {
        use ratatui::backend::TestBackend;
        let width = 136u16;
        let height = 36u16;
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut app = App::new();
        app.focused_pane = FocusedPane::Detail;
        app.selected_network_row = 2; // api.github.com
        terminal.draw(|f| ui(f, &app)).unwrap();

        let buffer = terminal.backend().buffer().clone();

        fn to_rgb(c: ratatui::style::Color) -> (u8, u8, u8) {
            match c {
                ratatui::style::Color::Reset => (195, 200, 210),
                ratatui::style::Color::Black => (18, 20, 26),
                ratatui::style::Color::Red => (255, 95, 125),
                ratatui::style::Color::Green => (90, 240, 150),
                ratatui::style::Color::Yellow => (255, 185, 70),
                ratatui::style::Color::Blue => (100, 160, 255),
                ratatui::style::Color::Magenta => (215, 120, 255),
                ratatui::style::Color::Cyan => (90, 230, 210),
                ratatui::style::Color::Gray => (170, 175, 185),
                ratatui::style::Color::DarkGray => (105, 110, 125),
                ratatui::style::Color::LightRed => (255, 120, 140),
                ratatui::style::Color::LightGreen => (120, 255, 180),
                ratatui::style::Color::LightYellow => (255, 210, 100),
                ratatui::style::Color::LightBlue => (140, 190, 255),
                ratatui::style::Color::LightMagenta => (235, 150, 255),
                ratatui::style::Color::LightCyan => (140, 255, 240),
                ratatui::style::Color::White => (255, 255, 255),
                ratatui::style::Color::Rgb(r, g, b) => (r, g, b),
                ratatui::style::Color::Indexed(i) => (i, i, i),
            }
        }

        let mut lines = Vec::new();
        for y in 0..height {
            let mut row = Vec::new();
            for x in 0..width {
                let cell = buffer.cell((x, y)).unwrap();
                let sym = cell.symbol();
                let fg = to_rgb(cell.fg);
                let bg = match cell.bg {
                    ratatui::style::Color::Reset => (18, 19, 28),
                    other => to_rgb(other),
                };
                let bold = cell.modifier.contains(ratatui::style::Modifier::BOLD);
                row.push(serde_json::json!({
                    "s": sym,
                    "fg": [fg.0, fg.1, fg.2],
                    "bg": [bg.0, bg.1, bg.2],
                    "b": bold,
                }));
            }
            lines.push(row);
        }

        let json_data = serde_json::to_string(&lines).unwrap();
        let _ = std::fs::write("/tmp/sbx_tui_dump.json", json_data);

        // Also dump LocalModels view
        app.set_view(DashboardView::LocalModels);
        terminal.draw(|f| ui(f, &app)).unwrap();
        let buffer = terminal.backend().buffer().clone();
        let mut model_lines = Vec::new();
        for y in 0..height {
            let mut row = Vec::new();
            for x in 0..width {
                let cell = buffer.cell((x, y)).unwrap();
                let sym = cell.symbol();
                let fg = to_rgb(cell.fg);
                let bg = match cell.bg {
                    ratatui::style::Color::Reset => (18, 19, 28),
                    other => to_rgb(other),
                };
                let bold = cell.modifier.contains(ratatui::style::Modifier::BOLD);
                row.push(serde_json::json!({
                    "s": sym,
                    "fg": [fg.0, fg.1, fg.2],
                    "bg": [bg.0, bg.1, bg.2],
                    "b": bold,
                }));
            }
            model_lines.push(row);
        }
        let model_json = serde_json::to_string(&model_lines).unwrap();
        let _ = std::fs::write("/tmp/sbx_models_dump.json", model_json);
    }
}
