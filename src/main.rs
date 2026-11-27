use anyhow::Result;
use clap::{CommandFactory, FromArgMatches, Parser, Subcommand};
use std::path::PathBuf;

mod cli_dispatch;
mod cli_subcommands;
mod commands;
mod config;
mod iam;
mod image;
mod telemetry;
#[cfg(unix)]
mod zap;

#[cfg(test)]
mod main_lifecycle_tests;
#[cfg(test)]
mod main_product_tests;
#[cfg(test)]
mod main_tests;

pub use hanzo_gen::warn;

use cli_subcommands::*;
use commands::dev::Target;
use commands::lifecycle::units::parse_size_mb;

#[derive(Parser)]
#[command(name = "hanzo")]
#[command(author = "Hanzo AI")]
#[command(disable_version_flag = true)]
#[command(about = "Unified CLI for Hanzo AI development tools", long_about = None)]
#[command(args_conflicts_with_subcommands = true, subcommand_negates_reqs = true)]
struct Cli {
    /// Sets a custom config file
    #[arg(short, long, value_name = "FILE", global = true)]
    config: Option<PathBuf>,

    /// Act as this org instead of your own
    #[arg(long = "as", value_name = "ORG", global = true)]
    org: Option<String>,

    /// Increase logging verbosity
    #[arg(short, long, action = clap::ArgAction::Count, global = true)]
    verbose: u8,

    /// Print the CLI version (identical to `hanzo version`)
    #[arg(short = 'V', long = "version", action = clap::ArgAction::SetTrue)]
    version: bool,

    /// The coding-session args, flattened so a bare `hanzo [flags] [task]` is a
    /// coding session with them. Ignored when an explicit subcommand is given.
    #[command(flatten)]
    code: commands::cloud_sessions::SessionArgs,

    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    /// Search, index and read code through the cloud's code index
    Code,

    /// Start a coding session: a coding agent with the Hanzo MCP toolset
    /// attached, on the account its tool is signed in to (`--metered` for
    /// Hanzo's metered models), and the session streamed live to mission control.
    /// Use `--cloud` to execute the task remotely in a Hanzo sandbox.
    ///
    /// Runs Hanzo's own agent by default. Name another tool with a flag:
    ///
    ///   hanzo dev              our own agent (the default)
    ///   hanzo dev --claude     hanzo dev --codex     hanzo dev --cursor     hanzo dev --agy
    ///   hanzo dev --runc       (bare metal host with GPU pass-through)
    ///   hanzo dev --debug      (verbose session & inspector mode)
    ///   hanzo dev --claude -- -c    (args after `--` go to the tool)
    ///
    /// A trailing task runs headless (`hanzo dev --claude "fix the failing
    /// test"`); omit it for an interactive session.
    #[command(verbatim_doc_comment)]
    Dev(commands::cloud_sessions::SessionArgs),

    /// Execute an application, model, agent or workload
    Run {
        /// Model ID/alias, project directory, registered app name, script, or image
        target: String,
        /// Arguments or prompt passed to the target
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },

    /// Point an agent at the desktop and browser instead of the repo
    Desktop(commands::cloud_sessions::SessionArgs),

    /// Talk directly to the local Rust `hanzod` daemon (same backend Hanzo Desktop uses)
    Daemon(commands::daemon::DaemonArgs),

    /// Unified linked machines, provider accounts, and cross-source usage (HIP-1350)
    #[command(alias = "source")]
    Sources(commands::sources::SourcesArgs),

    /// Canonical unified Hanzo system prompt (`# Hanzo Rules`, `hanzoai/prompt`) & progressive `hanzoskills.com` discovery
    Prompt(commands::prompt::PromptArgs),

    /// Manage identities and credentials
    Auth {
        #[command(subcommand)]
        command: AuthCommands,
    },

    /// Sign in through Hanzo IAM (OIDC) — `hanzo auth login` at the top level
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

    /// Show who you are signed in as and in which org — `hanzo auth show` at the top level
    Whoami {
        #[arg(long, default_value_t = iam::paths::DEFAULT_BRAND.to_string())]
        brand: String,
    },

    /// Link this machine to Hanzo cloud: on the org's network over Hanzo ZT, on
    /// the fleet, and with a shell the console can drive
    #[command(args_conflicts_with_subcommands = true)]
    Link {
        #[command(subcommand)]
        command: Option<LinkCommands>,
        #[arg(long)]
        shell: Option<String>,
        #[arg(long)]
        read_only: bool,
        #[arg(long)]
        title: Option<String>,
    },

    /// Keep this machine present in the fleet: one beat every 30s, no shell, no session
    Beat,

    /// Opt-in shell startup hook and quiet background fleet presence + runner
    Shell {
        #[command(subcommand)]
        command: commands::shell::Action,
    },

    /// Manage local CLI settings
    Config {
        #[command(subcommand)]
        command: ConfigCommands,
    },

    /// Run AI engines on this machine (`engine serve <model>`)
    Engine {
        #[command(subcommand)]
        command: EngineCommands,
    },

    /// Manage local AI models (`model pull`, `model run`, `model list`)
    Model {
        #[command(subcommand)]
        command: ModelCommands,
    },

    /// Start, stop, restart, and inspect local Hanzo platform services (`cloud`, `engine`, `analytics`, `insights`)
    #[command(visible_alias = "services", alias = "svc")]
    Service {
        #[command(subcommand)]
        command: Option<ServiceCommands>,
    },

    /// Start, stop, list, and inspect logs for local user applications
    #[command(visible_alias = "apps")]
    App {
        #[command(subcommand)]
        command: Option<AppCommands>,
    },

    /// Run a local Hanzo Bot gateway or embedded agent wired to local LLMs, or manage cloud `/v1/bot`
    Bot {
        #[command(subcommand)]
        command: BotCommands,
    },

    /// Pull local AI model weights into the shared Hugging Face cache (`hanzo engine pull <model>`)
    Pull {
        model: String,
        #[arg(long, default_value = "local")]
        node: String,
    },

    /// Personal & organization knowledge graph — local Apple PKG (`local`, `sync`) and cloud `/v1/graph`
    Graph {
        #[command(subcommand)]
        command: commands::graph::GraphCommands,
    },

    /// Provide this machine as a CI runner
    Runner {
        #[command(subcommand)]
        command: RunnerCommands,
    },

    /// List and manage connected social & messaging channels (`/v1/channel`)
    Channels(commands::channels::ChannelsArgs),

    /// Connect a social or messaging channel via OAuth (`/v1/channel/connect`)
    Connect(commands::channels::ConnectArgs),

    /// Create, schedule, publish, and inspect social posts & RSS autoposts (`/v1/post`)
    #[command(visible_alias = "posts")]
    Post(commands::channels::PostArgs),

    /// Scan local files for exposed secrets (exits non-zero on a find)
    Scan { path: PathBuf },

    /// Serve, bundle, and preview local web SPAs, Go hanzo.ai/zip services, and static sites
    Serve(commands::serve::Args),

    /// Build a container image with BuildKit, inside a hanzo-vm microVM
    Build(commands::build::Args),

    /// Manage the local microVM (`vm start|stop|resize|status`) or pass args through to `hanzo-vm`
    #[command(disable_help_flag = true)]
    Vm {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },

    /// Manage the local or Zero-Trust Hanzo Kubernetes cluster (`k8s`) or pass kubectl args through
    #[command(disable_help_flag = true, visible_alias = "kubectl", alias = "kube")]
    K8s {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },

    /// Bring up the local Hanzo developer environment (runtime, Cloud, and current project)
    Up {
        #[command(subcommand)]
        command: Option<UpCommands>,
        /// CPUs for the VM
        #[arg(long, default_value_t = 4)]
        cpus: u32,
        /// Memory size (e.g. `8GiB`, `4096MiB`, or plain MiB integer `4096`)
        #[arg(long, default_value = "4096", value_parser = parse_size_mb)]
        memory: u64,
        /// Disk size (e.g. `32GiB`, `8192MiB`, or plain MiB integer `32768`)
        #[arg(long, visible_alias = "disk", default_value = "32768", value_parser = parse_size_mb)]
        disk_size: u64,
        /// The cloud image to deploy
        #[arg(long, value_name = "IMAGE", default_value = commands::up::CLOUD)]
        cloud: String,
        /// Opt-in staged Cloud subsystems (`CLOUD_ENABLE_STAGED`)
        #[arg(long, value_name = "SUBSYSTEMS", env = "CLOUD_ENABLE_STAGED")]
        staged: Option<String>,
        /// Hanzo IAM OIDC issuer URL (`CLOUD_IAM_ISSUER`)
        #[arg(long, value_name = "URL", env = "CLOUD_IAM_ISSUER")]
        iam_issuer: Option<String>,
        /// Hanzo AI / Engine HTTP base URL (`CLOUD_AI_BASE_URL`)
        #[arg(long, value_name = "URL", env = "CLOUD_AI_BASE_URL")]
        ai_base_url: Option<String>,
        /// Refuse non-Hanzo container registries (`ghcr.io`, Docker Hub) and require `oci.hanzo.ai`
        #[arg(long, env = "HANZO_STRICT_REGISTRY")]
        strict_registry: bool,
        /// Print the running cluster's measurement
        #[arg(long)]
        attest: bool,
        /// Publish the cluster's API on the org's network as `k8s-<CLUSTER>`
        #[arg(long, value_name = "CLUSTER")]
        link: Option<String>,
        /// Launch the interactive Sandboxes TUI dashboard immediately
        #[arg(long, conflicts_with = "no_ui")]
        ui: bool,
        /// Suppress the interactive Sandboxes TUI dashboard
        #[arg(long, conflicts_with = "ui")]
        no_ui: bool,
        /// Start local AI runtime (`engine`) only without deploying heavy Cloud workloads
        #[arg(long, conflicts_with = "cloud_only")]
        ai_only: bool,
        /// Bring up Hanzo Cloud only (skip deploying the current directory's user app)
        #[arg(long, conflicts_with = "ai_only")]
        cloud_only: bool,
        /// Use native Linux host runtime (`k3s` + `containerd`) instead of a microVM
        #[arg(long)]
        native: bool,
    },

    /// Stop workloads in the selected environment while keeping the local runtime warm
    Down {
        /// Also shut down the local k3s microVM (equivalent to `hanzo stop`)
        #[arg(long, alias = "all")]
        vm: bool,
        /// Stop Hanzo Cloud platform workloads explicitly (even inside an app directory)
        #[arg(long)]
        cloud: bool,
        /// Stop both Hanzo Cloud and all registered local applications while keeping runtime warm
        #[arg(long)]
        all_workloads: bool,
    },

    /// Start the local Hanzo runtime (microVM or native Linux k3s) without deploying Cloud or apps
    Start {
        /// CPUs for the VM
        #[arg(long, default_value_t = 4)]
        cpus: u32,
        /// Memory size (e.g. `8GiB`, `4096MiB`, or plain MiB integer `4096`)
        #[arg(long, default_value = "4096", value_parser = parse_size_mb)]
        memory: u64,
        /// Disk size (e.g. `32GiB`, `8192MiB`, or plain MiB integer `32768`)
        #[arg(long, visible_alias = "disk", default_value = "32768", value_parser = parse_size_mb)]
        disk_size: u64,
        /// Use native Linux host runtime (`k3s` + `containerd`) instead of a microVM
        #[arg(long)]
        native: bool,
        /// Refuse non-Hanzo container registries (`ghcr.io`, Docker Hub) and require `oci.hanzo.ai`
        #[arg(long, env = "HANZO_STRICT_REGISTRY")]
        strict_registry: bool,
    },

    /// Gracefully stop the entire local Hanzo runtime (drain workloads, stop k3s & microVM)
    Stop,

    /// Restart the local Hanzo runtime and restore previously running workloads
    Restart {
        /// CPUs for the VM
        #[arg(long, default_value_t = 4)]
        cpus: u32,
        /// Memory size (e.g. `8GiB`, `4096MiB`, or plain MiB integer `4096`)
        #[arg(long, default_value = "4096", value_parser = parse_size_mb)]
        memory: u64,
        /// Disk size (e.g. `32GiB`, `8192MiB`, or plain MiB integer `32768`)
        #[arg(long, visible_alias = "disk", default_value = "32768", value_parser = parse_size_mb)]
        disk_size: u64,
        /// The cloud image to deploy if Cloud was running
        #[arg(long, value_name = "IMAGE", default_value = commands::up::CLOUD)]
        cloud: String,
        /// Opt-in staged Cloud subsystems (`CLOUD_ENABLE_STAGED`)
        #[arg(long, value_name = "SUBSYSTEMS", env = "CLOUD_ENABLE_STAGED")]
        staged: Option<String>,
        /// Hanzo IAM OIDC issuer URL (`CLOUD_IAM_ISSUER`)
        #[arg(long, value_name = "URL", env = "CLOUD_IAM_ISSUER")]
        iam_issuer: Option<String>,
        /// Hanzo AI / Engine HTTP base URL (`CLOUD_AI_BASE_URL`)
        #[arg(long, value_name = "URL", env = "CLOUD_AI_BASE_URL")]
        ai_base_url: Option<String>,
        /// Refuse non-Hanzo container registries
        #[arg(long, env = "HANZO_STRICT_REGISTRY")]
        strict_registry: bool,
        /// Use native Linux host runtime (`k3s` + `containerd`) instead of a microVM
        #[arg(long)]
        native: bool,
    },

    /// Explicitly reset local Hanzo dev environment state (stop runtime, remove ~/.hanzo/up & kubeconfig)
    Reset {
        /// Also delete cached `hanzo-vm` k3s checkpoints so the next boot rebuilds them from scratch
        #[arg(long, alias = "vm-cache")]
        all: bool,
    },

    /// Show multi-layer status of the local Hanzo runtime, platform services, and applications
    Ps {
        /// Emit structured JSON
        #[arg(long)]
        json: bool,
    },

    /// Show local 3-layer system status and cloud fleet health: unhealthy first, then clusters, applications & machines
    Status {
        /// Show the local GPU inference cluster instead of the cloud
        #[arg(
            long,
            visible_alias = "nodes",
            visible_alias = "gpu",
            visible_alias = "telemetry"
        )]
        infer: bool,
        /// Show only the local 3-layer system status (skip remote cloud fleet query)
        #[arg(long)]
        local: bool,
    },

    /// Live telemetry inspector and cluster dashboard for GPU inference cluster & models
    #[command(
        visible_alias = "top",
        visible_alias = "gpu",
        visible_alias = "telemetry"
    )]
    Monitor(commands::monitor::Args),

    /// Print the CLI version
    Version,

    /// Check installed tools, local 3-layer runtime health, authentication, GPU runtime & release integrity
    Doctor {
        /// Download current releases and verify provenance and installed bytes
        #[arg(long, conflicts_with = "offline")]
        verify: bool,
        /// Skip network and authentication checks
        #[arg(long)]
        offline: bool,
        /// Share diagnostics, Kai decision trace, and recent coding session report with Hanzo
        #[arg(long, visible_alias = "report", visible_alias = "upload")]
        share: bool,
    },

    /// Collect and share a redacted coding session & diagnostic report with Hanzo
    #[command(visible_alias = "bug", visible_alias = "feedback")]
    Report {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },

    /// Upgrade every Hanzo tool (hanzo, mcp, gen, zt) to its current release
    Upgrade { tools: Vec<String> },

    /// Open the interactive fleet operations console (sandboxes, compute nodes, GPU mesh)
    #[command(
        visible_alias = "dashboard",
        visible_alias = "ui",
        visible_alias = "gui"
    )]
    Console,

    /// Agent sandboxes & isolated execution environments (`sandbox start`, `stop`, `run`, `ls`)
    #[command(visible_alias = "sbx", alias = "sandboxes")]
    Sandbox {
        #[command(subcommand)]
        command: Option<SandboxCommands>,
    },

    /// List active sandboxes, local runtime summary, & agent workspaces
    #[command(alias = "list")]
    Ls,

    /// Pull and load models across the distributed cluster fleet (DGX Spark & Strix Halo)
    Load {
        #[arg(long, default_value = "all")]
        node: String,
        #[arg(long)]
        model: Option<String>,
    },

    /// Opt in to the chain (`hanzo-chain`, a child of hanzod), or out of it
    #[command(alias = "fabric")]
    Chain {
        #[command(subcommand)]
        command: ChainCommands,
    },

    /// Network selection + custom/sovereign networks (mirrors the console)
    Network {
        #[command(subcommand)]
        command: NetworkCommands,
    },

    /// The local cloud host — every cloud command, served from a checkout
    Host {
        #[command(subcommand)]
        command: HostCommands,
    },

    /// Wallet identity — PQ cloud custody (KMS/MPC) or local keychain
    Wallet {
        #[command(subcommand)]
        command: WalletCommands,
    },

    /// Publish a local service to a public https://<token>.share.hanzo.ai URL
    Share {
        target: String,
        #[arg(long, default_value = "proxy")]
        backend_mode: String,
        #[arg(long)]
        name: Option<String>,
    },

    /// Initialize a new Hanzo project
    Init {
        #[arg(short, long, default_value = "default")]
        template: String,
        name: Option<String>,
    },
}

pub(crate) fn is_broken_pipe(payload: &(dyn std::any::Any + Send)) -> bool {
    let text = payload
        .downcast_ref::<String>()
        .map(String::as_str)
        .or_else(|| payload.downcast_ref::<&str>().copied())
        .unwrap_or("");
    text.contains("Broken pipe") || text.contains("failed printing to")
}

fn install_broken_pipe_quiet() {
    let default = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        if is_broken_pipe(info.payload()) {
            std::process::exit(0);
        }
        default(info);
    }));
}

#[tokio::main]
async fn main() -> std::process::ExitCode {
    install_broken_pipe_quiet();
    match run().await {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("Error: {err:#}");
            std::process::ExitCode::FAILURE
        }
    }
}

async fn run() -> Result<()> {
    {
        let argv: Vec<String> = std::env::args().skip(1).collect();
        if argv.first().map(String::as_str) == Some("__mcp") {
            return hanzo_gen::mcp::main(hanzo_gen::Root::user(), &argv[1..]).await;
        }
    }
    {
        let argv: Vec<String> = std::env::args().skip(1).collect();
        if argv == ["--help"] || argv == ["help"] {
            print!("{}", commands::man::page(&Cli::command()));
            return Ok(());
        }
    }

    let hand = Cli::command();
    let merged = commands::product::augment(hand.clone());
    let argv = backend_flags(
        &merged,
        alias(&merged, hoist(&merged, std::env::args().collect())),
    );
    if let Some(message) = misspelled(&merged, &argv) {
        clap::Error::raw(clap::error::ErrorKind::InvalidSubcommand, message).exit();
    }
    let matches = merged.get_matches_from(argv);

    if matches.get_flag("version") {
        commands::version::run();
        return Ok(());
    }

    let log_level = match matches.get_count("verbose") {
        0 => "warn",
        1 => "info",
        2 => "debug",
        _ => "trace",
    };
    tracing_subscriber::fmt().with_env_filter(log_level).init();
    if matches.subcommand_name() != Some("doctor") {
        commands::host::retire_shared();
    }

    let mut config = config::Config::load(matches.get_one::<PathBuf>("config").cloned())?;
    config.org = matches
        .get_one::<String>("org")
        .cloned()
        .or_else(|| config.default_org.clone())
        .or_else(|| std::env::var("HANZO_ORG").ok());
    config.network.bound = Some(commands::network::active(&config).name);
    let telemetry = if matches
        .subcommand_matches("doctor")
        .is_some_and(|args| args.get_flag("offline"))
    {
        hanzo_event::Telemetry::disabled()
    } else {
        let t = telemetry::build(&config);
        telemetry::install_panic_hook(&t);
        t
    };

    if let Some(resolved) = commands::product::resolve(&hand, &matches) {
        let started = std::time::Instant::now();
        let outcome = commands::product::dispatch(&mut config, resolved).await;
        if let Err(ref err) = outcome {
            if telemetry::should_capture_error(err) {
                telemetry.capture_error(
                    "CommandError",
                    &err.to_string(),
                    None,
                    true,
                    Some("product"),
                );
            }
        }
        telemetry.command("product", started.elapsed(), outcome.is_ok());
        telemetry.flush().await;
        return outcome;
    }

    let cli = Cli::from_arg_matches(&matches)?;
    match cli.command {
        Some(command) => {
            let command_label = telemetry::label(&command);
            let started = std::time::Instant::now();
            let outcome = cli_dispatch::dispatch(command, config).await;
            if let Err(ref err) = outcome {
                if telemetry::should_capture_error(err) {
                    telemetry.capture_error(
                        "CommandError",
                        &err.to_string(),
                        None,
                        true,
                        Some(command_label),
                    );
                }
            }
            telemetry.command(command_label, started.elapsed(), outcome.is_ok());
            telemetry.flush().await;
            outcome
        }
        None => {
            let mut code = cli.code;
            code.link = true;
            let started = std::time::Instant::now();
            let outcome = commands::dev::session(&mut config, code, Target::Repo).await;
            if let Err(ref err) = outcome {
                if telemetry::should_capture_error(err) {
                    telemetry.capture_error(
                        "CommandError",
                        &err.to_string(),
                        None,
                        true,
                        Some("dev"),
                    );
                }
            }
            telemetry.command("dev", started.elapsed(), outcome.is_ok());
            telemetry.flush().await;
            outcome
        }
    }
}

pub(crate) fn first_word(mut args: impl Iterator<Item = String>) -> Option<String> {
    while let Some(a) = args.next() {
        match a.as_str() {
            a if VALUED.contains(&a) => {
                args.next();
            }
            _ if a.starts_with('-') => {}
            _ => return Some(a),
        }
    }
    None
}

const VALUED: [&str; 3] = ["--config", "-c", "--as"];

fn leading_globals(argv: &[String]) -> usize {
    let mut i = 1;
    while let Some(a) = argv.get(i).map(String::as_str) {
        if VALUED.contains(&a) {
            i += 2;
        } else if a == "--verbose"
            || a.starts_with("--config=")
            || a.starts_with("--as=")
            || (a.len() > 1 && a.starts_with('-') && a[1..].bytes().all(|b| b == b'v'))
        {
            i += 1;
        } else {
            break;
        }
    }
    i
}

pub(crate) fn alias(cmd: &clap::Command, mut argv: Vec<String>) -> Vec<String> {
    let at = leading_globals(&argv);
    if argv.get(at).map(String::as_str) != Some("code") {
        return argv;
    }
    let rest = &argv[at + 1..];
    let help = rest.iter().any(|a| a == "--help" || a == "-h");
    let index = first_word(rest.iter().cloned()).is_some_and(|w| {
        cmd.find_subcommand("code")
            .is_some_and(|c| c.find_subcommand(&w).is_some())
    });
    let cloud = rest.iter().any(|a| a == "--cloud" || a == "--teleport");
    if cloud || !(help || index) {
        argv[at] = "dev".into();
    }
    argv
}

pub(crate) fn backend_flags(cmd: &clap::Command, argv: Vec<String>) -> Vec<String> {
    let at = leading_globals(&argv);
    let session = match argv.get(at).map(String::as_str) {
        None => true,
        Some(w) if !w.starts_with('-') => {
            matches!(w, "dev" | "desktop") || cmd.find_subcommand(w).is_none()
        }
        Some(_) => true,
    };
    if !session {
        return argv;
    }
    hanzo_gen::args::expand(cmd, argv)
}

pub(crate) fn misspelled(cmd: &clap::Command, argv: &[String]) -> Option<String> {
    let at = leading_globals(argv);
    let word = argv.get(at)?;
    let lone =
        argv.len() == at + 1 && !word.starts_with('-') && !word.contains(char::is_whitespace);
    if !lone || cmd.find_subcommand(word).is_some() || hanzo_gen::backend::BackendKind::names(word)
    {
        return None;
    }
    let near = cmd
        .get_subcommands()
        .filter(|c| !c.is_hide_set())
        .flat_map(|c| std::iter::once(c.get_name()).chain(c.get_all_aliases()))
        .map(|name| (strsim::jaro_winkler(word, name), name))
        .filter(|(score, _)| *score >= 0.85)
        .max_by(|a, b| a.0.total_cmp(&b.0))?;
    Some(format!(
        "no command `{word}`: did you mean `hanzo {}`? As a coding task: `hanzo dev \"{word}\"`",
        near.1
    ))
}

pub(crate) fn hoist(cmd: &clap::Command, mut argv: Vec<String>) -> Vec<String> {
    let i = leading_globals(&argv);
    let (mut at, mut end) = (cmd, i);
    while let Some(sub) = argv.get(end).and_then(|w| at.find_subcommand(w)) {
        (at, end) = (sub, end + 1);
    }
    if i > 1 && end > i {
        argv[1..end].rotate_left(i - 1);
    }
    argv
}
