use clap::Subcommand;
use std::path::PathBuf;

use crate::{commands, iam};

#[derive(Subcommand)]
pub(crate) enum McpCommands {
    /// Diagnose MCP clients, native computer control, ZAP router, and browser extension
    Doctor {
        /// Output machine-readable JSON
        #[arg(long)]
        json: bool,
        /// Self-repair browser native messaging registrations before checking
        #[arg(long)]
        fix: bool,
    },
    /// Configure Hanzo MCP for one or all local AI clients (`all`, `hanzo`, `codex`, `claude`, `claude-desktop`, `cursor`, `gemini`, `antigravity`)
    Install {
        /// Target client (`all` configures every supported local AI client)
        #[arg(default_value = "all")]
        target: String,
        /// Optional path to the hanzo-mcp binary to register
        #[arg(long)]
        binary: Option<PathBuf>,
        /// Preview changes without writing files
        #[arg(long)]
        dry_run: bool,
    },
    /// Print or reset this user's browser pairing code for ZAP
    Pair {
        #[arg(long)]
        reset: bool,
    },
    /// Build tgrep's trigram index for a directory
    Index {
        #[arg(default_value = ".")]
        dir: PathBuf,
    },
    /// Serve a single explicitly shared browser tab over stdio
    Share {
        #[arg(long)]
        browser: String,
        #[arg(long)]
        tab: String,
        #[arg(long)]
        url: String,
    },
    /// Run a Kai action via hanzo-mcp
    Kai {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Connect this machine to Hanzo Cloud (`wss://api.hanzo.ai/v1/node/connect`) for mobile, cloud agents, and third-party MCP clients
    Connect {
        /// Cloud WebSocket node endpoint (defaults to wss://api.hanzo.ai/v1/node/connect)
        #[arg(long)]
        url: Option<String>,
        /// Stable node identifier within your org (defaults to `<hostname>-mcp`)
        #[arg(long)]
        node_id: Option<String>,
        /// Human-readable display name shown in mobile/console
        #[arg(long)]
        name: Option<String>,
        /// Optional comma-separated subset of tools to expose remotely (default: all)
        #[arg(long)]
        tools: Option<String>,
        /// Verify cloud handshake, print connected node info, and exit
        #[arg(long)]
        once: bool,
    },
}

#[derive(Subcommand)]
pub(crate) enum ServiceCommands {
    /// Start a local platform service (`cloud`, `engine`, `iam`, `datastore`, `analytics`, `insights`, `storage`, `commerce`)
    Start { name: String },
    /// Stop a local platform service
    Stop { name: String },
    /// Restart a local platform service
    Restart { name: String },
    /// List local platform services and their status
    #[command(visible_alias = "ls", alias = "status")]
    List {
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand)]
pub(crate) enum AppCommands {
    /// Start a registered application (by name/prefix, or current directory if omitted)
    Start { name: Option<String> },
    /// Stop a registered application (by name/prefix, or current directory if omitted)
    Stop { name: Option<String> },
    /// List registered local applications
    #[command(visible_alias = "ls", alias = "ps")]
    List {
        #[arg(long)]
        json: bool,
    },
    /// View or follow logs for a local application
    Logs {
        name: Option<String>,
        #[arg(short = 'f', long = "follow")]
        follow: bool,
    },
}

#[derive(Subcommand)]
pub(crate) enum ModelCommands {
    /// Pull model weights into the shared local cache
    Pull {
        model: String,
        #[arg(long, default_value = "local")]
        node: String,
    },
    /// Run local inference or start serving a model (`hanzo model run <model> [prompt...]`)
    Run {
        model: String,
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        prompt: Vec<String>,
    },
    /// List cached and active local AI models
    #[command(visible_alias = "ls")]
    List {
        #[arg(long)]
        json: bool,
    },
    /// Serve a model on a local `/v1` inference endpoint
    Serve {
        model: String,
        #[arg(long, value_name = "FILE")]
        overlay: Option<String>,
        #[arg(last = true, allow_hyphen_values = true)]
        passthrough: Vec<String>,
    },
}

#[derive(Subcommand)]
pub(crate) enum AuthCommands {
    Login {
        #[arg(long, default_value_t = iam::paths::DEFAULT_BRAND.to_string())]
        brand: String,
        #[arg(long, value_name = "PROVIDER")]
        provider: Option<String>,
        #[arg(long, value_name = "TOKEN")]
        token: Option<String>,
        #[arg(long, value_name = "ACCOUNT", conflicts_with = "token")]
        account: Option<String>,
    },
    Logout {
        #[arg(value_name = "IDENTITY")]
        identity: Option<String>,
        #[arg(long, default_value_t = iam::paths::DEFAULT_BRAND.to_string())]
        brand: String,
        #[arg(long)]
        all: bool,
    },
    Show {
        #[arg(long, default_value_t = iam::paths::DEFAULT_BRAND.to_string())]
        brand: String,
    },
    List {
        #[arg(long, default_value_t = iam::paths::DEFAULT_BRAND.to_string())]
        brand: String,
        #[arg(short = 'o', long = "output", value_name = "FORMAT")]
        output: Option<String>,
    },
    /// Switch active identity (`owner/name`) and/or default organization context (`<org>`)
    #[command(visible_alias = "switch")]
    Use {
        /// Identity (`owner/name`) or organization (`<org>`) to switch to
        #[arg(value_name = "TARGET")]
        identity: Option<String>,
        /// Persist default organization context (`X-Org-Id`) for subsequent commands
        #[arg(long = "org", value_name = "ORG", conflicts_with = "clear_org")]
        context_org: Option<String>,
        /// Clear persisted organization override back to the active identity's home org
        #[arg(long)]
        clear_org: bool,
        #[arg(long, default_value_t = iam::paths::DEFAULT_BRAND.to_string())]
        brand: String,
    },
    Token {
        #[arg(long, default_value_t = iam::paths::DEFAULT_BRAND.to_string())]
        brand: String,
        #[arg(long)]
        network: Option<String>,
    },
}

#[derive(Subcommand)]
pub(crate) enum ConfigCommands {
    List,
    Get {
        key: String,
    },
    Set {
        key: String,
        value: String,
    },
    #[command(hide = true)]
    Profile,
}

#[derive(Subcommand)]
pub(crate) enum EngineCommands {
    Pull {
        model: String,
        #[arg(long, default_value = "local")]
        node: String,
    },
    #[command(visible_alias = "ls")]
    List,
    Serve {
        model: String,
        #[arg(long, value_name = "FILE")]
        overlay: Option<String>,
        #[arg(last = true, allow_hyphen_values = true)]
        passthrough: Vec<String>,
    },
    Up,
    Down,
    Status,
}

#[derive(Subcommand)]
pub(crate) enum BotCommands {
    #[command(visible_alias = "start", alias = "serve", alias = "up")]
    Gateway {
        #[arg(long, default_value_t = 19001)]
        port: u16,
        #[arg(long, default_value = "zen-nano")]
        model: String,
        #[arg(long, default_value = "http://127.0.0.1:1234")]
        engine_url: String,
        #[arg(long)]
        force: bool,
    },
    #[command(visible_alias = "chat", alias = "say")]
    Ask {
        message: String,
        #[arg(long, default_value = "zen-nano")]
        model: String,
        #[arg(long, default_value = "http://127.0.0.1:1234")]
        engine_url: String,
        #[arg(long)]
        json: bool,
    },
    #[command(visible_alias = "computer")]
    Browser {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    Status {
        #[arg(long, default_value = "http://127.0.0.1:1234")]
        engine_url: String,
        #[arg(long, default_value_t = 19001)]
        port: u16,
    },
}

#[derive(Subcommand)]
pub(crate) enum RunnerCommands {
    Start,
}

#[derive(Subcommand)]
pub(crate) enum ChainCommands {
    Up { network: String },
    Status,
    Down,
}

#[derive(Subcommand)]
pub(crate) enum HostCommands {
    Start,
    Status,
    Stop,
    Serve {
        #[arg(default_value = "cloud")]
        service: String,
        #[arg(last = true, allow_hyphen_values = true)]
        passthrough: Vec<String>,
    },
}

#[derive(Subcommand)]
pub(crate) enum UpCommands {
    #[command(alias = "dashboard", alias = "ui")]
    Console,
    #[command(alias = "ps")]
    Status,
    Down {
        #[arg(long, alias = "all", conflicts_with_all = ["cloud", "all_workloads"])]
        vm: bool,
        #[arg(long, conflicts_with_all = ["vm", "all_workloads"])]
        cloud: bool,
        #[arg(long, conflicts_with_all = ["vm", "cloud"])]
        all_workloads: bool,
    },
    #[command(hide = true)]
    Supervise {
        #[arg(long)]
        runtime_only: bool,
    },
    #[command(external_subcommand)]
    Service(Vec<String>),
}

#[derive(Subcommand)]
pub(crate) enum SandboxCommands {
    /// Run a coding agent in an isolated sandbox (matches `sbx run <agent>`)
    Run(Box<commands::cloud_sessions::SessionArgs>),
    /// Execute a command inside the Seatbelt kernel sandbox (or `--vm` Linux microVM)
    Exec {
        #[arg(long)]
        vm: bool,
        #[arg(short = 'C', long)]
        cd: Option<PathBuf>,
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        command: Vec<String>,
    },
    /// Run a sandboxed headless browser computer-use capture & local Zen-Nano audit on a URL
    #[command(visible_alias = "browse", alias = "inspect")]
    Computer {
        url: String,
        #[arg(short, long)]
        out: Option<PathBuf>,
        #[arg(long, default_value = "http://127.0.0.1:1234")]
        engine_url: String,
    },
    /// List active sandboxes & agent workspaces
    #[command(alias = "ls", alias = "ps")]
    List,
    /// Explore container environments, sandbox templates, and model catalog
    Explore,
    /// Start an isolated sandbox environment (`hanzo sandbox start [name]`)
    Start {
        name: Option<String>,
        #[arg(long)]
        image: Option<String>,
        #[arg(long, default_value = "local")]
        node: String,
    },
    /// Stop a running sandbox environment (`hanzo sandbox stop [name]`)
    Stop { name: Option<String> },
    /// Launch an environment from a template
    Launch {
        template: String,
        #[arg(long, default_value = "local")]
        node: String,
    },
    /// Show local models catalog
    Models,
    /// Pull model weights targeting a specific node
    Pull {
        model: String,
        #[arg(long, default_value = "local")]
        node: String,
    },
    /// Load model weights into memory across the cluster fleet
    Load {
        #[arg(long, default_value = "all")]
        node: String,
        #[arg(long)]
        model: Option<String>,
    },
    /// Open the interactive fleet operations console
    #[command(alias = "dashboard", alias = "console")]
    Ui,
}

#[derive(Subcommand)]
pub(crate) enum NetworkCommands {
    List,
    Current,
    Use {
        name: String,
    },
    Add {
        name: String,
        #[arg(long)]
        network_id: u64,
        #[arg(long)]
        chain_id: Option<u64>,
        #[arg(long)]
        rpc: String,
        #[arg(long)]
        api: String,
        #[arg(long)]
        iam: Option<String>,
        #[arg(long)]
        explorer: Option<String>,
        #[arg(long)]
        label: Option<String>,
        #[arg(long)]
        activate: bool,
    },
}

#[derive(Subcommand)]
pub(crate) enum LinkCommands {
    Host {
        #[arg(requires = "target")]
        name: Option<String>,
        #[arg(requires = "name")]
        target: Option<String>,
        #[command(flatten)]
        signer: commands::link::Signer,
        #[command(flatten)]
        install: commands::link::Install,
    },
    Dial {
        service: String,
        #[arg(value_parser = clap::value_parser!(u16).range(1..))]
        port: u16,
        #[command(flatten)]
        signer: commands::link::Signer,
        #[command(flatten)]
        install: commands::link::Install,
    },
    Status {
        #[command(flatten)]
        signer: commands::link::Signer,
    },
    Rm {
        name: String,
        #[command(flatten)]
        signer: commands::link::Signer,
    },
    Grant {
        folder: PathBuf,
    },
    Revoke {
        folder: PathBuf,
    },
    Folders,
}

#[derive(Subcommand)]
pub(crate) enum WalletCommands {
    Show,
    Address,
    Create {
        #[arg(long)]
        name: Option<String>,
        #[arg(long)]
        local: bool,
        #[arg(long, default_value = "kms")]
        custody: String,
    },
    Import {
        secret: String,
        #[arg(long)]
        name: Option<String>,
    },
    Use {
        address: String,
    },
    List,
}
