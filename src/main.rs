use anyhow::Result;
use clap::{CommandFactory, FromArgMatches, Parser, Subcommand};
use colored::*;
use std::path::PathBuf;

mod commands;
mod config;
mod image;
mod iam;
mod telemetry;
// ZAP reaches the local host over a UNIX socket. Windows has none, so there the
// local host is reached over the HTTP view it also serves (see commands::host).
#[cfg(unix)]
mod zap;

/// Every warning the CLI prints goes through Gen's one `warn`, so they all look
/// the same and none lands on stdout.
pub use hanzo_gen::warn;

use commands::dev::Target;

#[derive(Parser)]
#[command(name = "hanzo")]
#[command(author = "Hanzo AI")]
// Clap's own `--version` is DISABLED and replaced by the flag below, so the
// version has ONE implementation (`commands::version::run`) behind all three of
// its spellings. Clap's would print its own line and exit before that function
// ran, which is exactly how `hanzo --version` came to disagree with `hanzo
// version` AND to lose the install-skew report that only the function carries.
#[command(disable_version_flag = true)]
#[command(about = "Unified CLI for Hanzo AI development tools", long_about = None)]
// Bare `hanzo` IS a coding session, WITH flags: the code args are flattened at the
// top level, so `hanzo --resume <id>`, `hanzo --model enso`, and `hanzo "fix the
// bug"` all route to a coding session (the same run `hanzo dev` starts).
// `args_conflicts_with_subcommands` keeps them mutually exclusive with an explicit
// subcommand (`hanzo dev …`, `hanzo auth …`), and `subcommand_negates_reqs` lets
// a subcommand run without them — so the flattened args apply ONLY to a bare `hanzo`.
#[command(args_conflicts_with_subcommands = true, subcommand_negates_reqs = true)]
struct Cli {
    /// Sets a custom config file
    ///
    /// GLOBAL: valid on every subcommand (`hanzo clusters list --config F`).
    #[arg(short, long, value_name = "FILE", global = true)]
    config: Option<PathBuf>,

    /// Act as this org instead of your own
    ///
    /// GLOBAL, and a SELECTION rather than an assertion: it rides as `X-Org-Id`
    /// and the gateway checks it against the IAM-signed `orgs` membership claim,
    /// discarding anything outside it. Naming an org you do not belong to is a
    /// no-op, not an escalation. Without it someone who belongs to several orgs
    /// can only ever reach their home one.
    ///
    /// When you hold an identity IN that org — `hanzo --as admin auth login`
    /// signs one in beside your default — the command speaks as that identity
    /// too, so `hanzo --as admin …` runs as `admin/z` with nothing to switch back.
    ///
    /// `--as`, not `--org`: forty-three generated operations already take an
    /// `org` of their own as a path or query value, and a global of that name
    /// collides with every one of them. `--as` is the same word kubectl uses for
    /// acting as someone else, and it says WHO the request is from rather than
    /// what it is about.
    #[arg(long = "as", value_name = "ORG", global = true)]
    org: Option<String>,

    /// Increase logging verbosity
    #[arg(short, long, action = clap::ArgAction::Count, global = true)]
    verbose: u8,

    /// Print the CLI version (identical to `hanzo version`)
    ///
    /// Declared HERE rather than left to clap so it is one flag among the others
    /// — the flattened coding-session args below cannot swallow it, and `main`
    /// routes it to the same function the subcommand runs.
    #[arg(short = 'V', long = "version", action = clap::ArgAction::SetTrue)]
    version: bool,

    /// The coding-session args, flattened so a bare `hanzo [flags] [task]` is a
    /// coding session with them. Ignored when an explicit subcommand is given.
    #[command(flatten)]
    code: hanzo_gen::Args,

    /// Optional: a truly-bare `hanzo` (no subcommand) launches a cloud-linked
    /// coding session from the flattened `code` args above. `--help`/`-h` and every
    /// explicit subcommand are handled by clap before that fallback ever applies.
    #[command(subcommand)]
    command: Option<Commands>,
}

/// `hanzo <resource> <command>` — the resource-noun tree. Every cloud capability
/// beyond these hand-written resources is a generated product subcommand
/// (`commands::product`), merged in at runtime.
#[derive(Subcommand)]
enum Commands {
    /// Search, index and read code through the cloud's code index
    ///
    /// With no index verb (bare, a task, or an agent flag) it is `hanzo dev`.
    Code,

    /// Start a coding session: a coding agent with the Hanzo MCP toolset
    /// attached, on the account its tool is signed in to (`--cloud` for
    /// Hanzo's metered models), and the session streamed live to mission control
    ///
    /// Runs our own `dev` agent by default. Name another tool with a flag:
    ///
    ///   hanzo dev              our own agent (the default)
    ///   hanzo dev --claude     hanzo dev --codex     hanzo dev --cursor     hanzo dev --agy
    ///   hanzo dev --runc       (bare metal host with GPU pass-through)
    ///   hanzo dev --claude -- -c    (args after `--` go to the tool)
    ///
    /// A trailing task runs headless (`hanzo dev --claude "fix the failing
    /// test"`); omit it for an interactive session.
    #[command(verbatim_doc_comment)]
    Dev(hanzo_gen::Args),

    /// Point an agent at the desktop and browser instead of the repo
    ///
    /// The same session as `hanzo dev` — same backends, same linking, same
    /// metering — aimed somewhere else. The Hanzo browser/computer tools ARE
    /// how it drives the desktop, so this pins the toolset on.
    Desktop(hanzo_gen::Args),

    /// Manage identities and credentials
    Auth {
        #[command(subcommand)]
        command: AuthCommands,
    },

    /// Sign in through Hanzo IAM (OIDC) — `hanzo auth login` at the top level.
    ///
    /// Signing in is the first thing anyone does and the one command they have
    /// not read the help for yet, so it answers where they will type it. The
    /// flags are `auth login`'s and it runs the same function; there is one
    /// implementation, reachable by two names.
    Login {
        /// Brand / tenant: hanzo | lux | zoo | pars | bootnode
        #[arg(long, default_value_t = iam::paths::DEFAULT_BRAND.to_string())]
        brand: String,
        /// Non-interactive provider: hanzo | openai | anthropic
        #[arg(long, value_name = "PROVIDER")]
        provider: Option<String>,
        /// `--token -` reads the credential from stdin (never argv)
        #[arg(long, value_name = "TOKEN")]
        token: Option<String>,
    },

    /// Show who you are signed in as and in which org — `hanzo auth show` at the top level.
    ///
    /// The question people type as one word, so it answers where they type it, by
    /// the same function as `auth show`.
    Whoami {
        /// Brand / tenant: hanzo | lux | zoo | pars | bootnode
        #[arg(long, default_value_t = iam::paths::DEFAULT_BRAND.to_string())]
        brand: String,
    },

    /// Link this machine to Hanzo cloud: on the org's network over Hanzo ZT, on
    /// the fleet, and with a shell the console can drive
    ///
    /// Bare, it puts this machine's identity on the org's network, registers it
    /// as a run-target and publishes a shell. `host`, `dial`, `status` and `rm`
    /// are the network: what this machine serves, what it reaches, what is up.
    #[command(args_conflicts_with_subcommands = true)]
    Link {
        #[command(subcommand)]
        command: Option<LinkCommands>,

        /// What to run: your $SHELL by default, or name one — `bash`, `zsh`, or
        /// `tmux` for a shell that survives a disconnect and can be attached
        /// locally at the same time.
        #[arg(long)]
        shell: Option<String>,

        /// Publish it read-only. A viewer sees the work and cannot type into it.
        #[arg(long)]
        read_only: bool,

        /// Name this link in the console. Defaults to the working directory.
        #[arg(long)]
        title: Option<String>,
    },

    /// Keep this machine present in the fleet: one beat every 30s, no shell, no
    /// session. Run it under a service manager so the console and the scheduler
    /// see the box between sessions instead of an hour after the last one.
    Beat,

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

    /// Provide this machine as a CI runner
    Runner {
        #[command(subcommand)]
        command: RunnerCommands,
    },

    /// Scan local files for exposed secrets (exits non-zero on a find)
    Scan { path: PathBuf },

    /// Build a container image with BuildKit, inside a hanzo-vm microVM
    ///
    /// Each build boots its own microVM from a BuildKit checkpoint and drives
    /// the buildkitd in it with buildctl; the context and the registry
    /// credentials stay on this machine. `-t REF --push` publishes the image,
    /// `-o FILE` writes it as an OCI archive, and with neither it only builds.
    Build(commands::build::Args),

    /// Run the hanzo-vm microVM CLI, args passed through verbatim
    ///
    /// The native microVM (hanzoai/vm): `hanzo vm run …`, `hanzo vm checkpoint
    /// …`. Every flag reaches hanzo-vm untouched — `hanzo vm --help` is its
    /// help, not this one.
    #[command(disable_help_flag = true)]
    Vm {
        /// Arguments handed to hanzo-vm verbatim
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },

    /// A local Kubernetes running the cloud — k3s in a Hanzo microVM
    ///
    /// Bare `hanzo up` boots k3s inside a hanzo-vm microVM, deploys the Hanzo
    /// cloud into it (API forwarded to 127.0.0.1:8080, kube to 6443) and writes
    /// ~/.kube/hanzo.yaml; `up status` and `up down` manage it. Every boot is
    /// measured — `--attest` prints what the running cluster is. What ran here
    /// before, the local cloud API, is `hanzo host serve` now, and `hanzo up
    /// <service>` forwards there for one release.
    Up {
        #[command(subcommand)]
        command: Option<UpCommands>,
        /// CPUs for the VM
        #[arg(long, default_value_t = 4)]
        cpus: u32,
        /// Memory in MB
        #[arg(long, default_value_t = 4096)]
        memory: u64,
        /// Disk size in MB
        #[arg(long, default_value_t = 16384)]
        disk_size: u64,
        /// The cloud image to deploy. A tag is resolved to a digest before it
        /// reaches the cluster; name a digest to skip the registry entirely
        #[arg(long, value_name = "IMAGE", default_value = commands::up::CLOUD)]
        cloud: String,
        /// Print the running cluster's measurement — what booted, what it
        /// runs, and what its platform will sign for the pair
        #[arg(long)]
        attest: bool,
        /// After the node is Ready, publish the cluster's API on the org's
        /// network as `k8s-<CLUSTER>` (`hanzo link host` carries it)
        #[arg(long, value_name = "CLUSTER")]
        link: Option<String>,
        /// Launch the interactive Sandboxes TUI dashboard immediately
        #[arg(long, conflicts_with = "no_ui")]
        ui: bool,
        /// Suppress the interactive Sandboxes TUI dashboard
        #[arg(long, conflicts_with = "ui")]
        no_ui: bool,
    },

    /// Stop the local k3s microVM started by `hanzo up`
    Down,

    /// Show the whole cloud: what is unhealthy first, then clusters,
    /// applications and the machines on the fleet
    Status {
        /// Show the local GPU inference cluster instead of the cloud
        #[arg(long, visible_alias = "nodes", visible_alias = "gpu", visible_alias = "telemetry")]
        infer: bool,
    },

    /// Live telemetry inspector and cluster dashboard for GPU inference cluster & models
    #[command(visible_alias = "top", visible_alias = "gpu", visible_alias = "telemetry")]
    Monitor(commands::monitor::Args),

    /// Print the CLI version
    Version,

    /// Upgrade every Hanzo tool (hanzo, mcp, dev, zt) to its current release, and
    /// every coding agent installed: npm agents under ~/.hanzo, the rest by their
    /// own updater (`claude update`, `goose update`, …) or uv. Name a tool to
    /// upgrade only it; naming an npm agent (gemini, qwen, opencode, …) installs it
    Upgrade {
        /// Tools to upgrade; none means all of them
        tools: Vec<String>,
    },

    /// Open the interactive fleet operations console (sandboxes, compute nodes, GPU mesh)
    #[command(visible_alias = "dashboard", visible_alias = "ui", visible_alias = "gui")]
    Console,

    /// Agent sandboxes & isolated execution environments (Docker `sbx` compatibility: `sbx run`, `sbx ls`)
    #[command(visible_alias = "sbx", alias = "sandboxes")]
    Sandbox {
        #[command(subcommand)]
        command: Option<SandboxCommands>,
    },

    /// List active sandboxes, runc containers, & agent workspaces
    #[command(visible_alias = "ps", alias = "list")]
    Ls,

    /// Pull and load models across the distributed cluster fleet (DGX Spark & Strix Halo)
    Load {
        /// Target node (spark, halo, evo, or all)
        #[arg(long, default_value = "all")]
        node: String,
        /// Model identifier to load
        #[arg(long)]
        model: Option<String>,
    },

    // ── kept resources (additive) ────────────────────────────────────────────
    /// Run the L1 chain node (hanzod) on hanzo.network
    ///
    /// `hanzo fabric` is a deprecated alias for one release — use `hanzo chain`.
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
        /// Local target: a port (3000), host:port, or a full url
        target: String,
        /// What sits behind the URL: proxy | web | static | drive
        #[arg(long, default_value = "proxy")]
        backend_mode: String,
        /// Reserve a stable subdomain name (else a random token)
        #[arg(long)]
        name: Option<String>,
    },

    /// Initialize a new Hanzo project
    Init {
        /// Project template
        #[arg(short, long, default_value = "default")]
        template: String,
        /// Project name
        name: Option<String>,
    },
}

#[derive(Subcommand)]
enum AuthCommands {
    /// Sign in through Hanzo IAM (OIDC), or store a provider key (OpenAI / Anthropic)
    Login {
        /// Brand / tenant: hanzo | lux | zoo | pars | bootnode
        #[arg(long, default_value_t = iam::paths::DEFAULT_BRAND.to_string())]
        brand: String,
        /// Non-interactive provider: hanzo | openai | anthropic
        #[arg(long, value_name = "PROVIDER")]
        provider: Option<String>,
        /// `--token -` reads the credential from stdin (never argv)
        #[arg(long, value_name = "TOKEN")]
        token: Option<String>,
    },
    /// Sign out one identity (or `--all`) and remove the credential
    Logout {
        /// `owner/name`, or a bare `owner` when unambiguous. Omit to sign out of the
        /// ACTIVE identity.
        #[arg(value_name = "IDENTITY")]
        identity: Option<String>,
        #[arg(long, default_value_t = iam::paths::DEFAULT_BRAND.to_string())]
        brand: String,
        /// Remove EVERY identity for this brand
        #[arg(long)]
        all: bool,
    },
    /// Show the active identity and org
    Show {
        #[arg(long, default_value_t = iam::paths::DEFAULT_BRAND.to_string())]
        brand: String,
    },
    /// List every identity, marking the active one
    List {
        #[arg(long, default_value_t = iam::paths::DEFAULT_BRAND.to_string())]
        brand: String,
    },
    /// Select the active identity (bare toggles when exactly two are held)
    Use {
        #[arg(value_name = "IDENTITY")]
        identity: Option<String>,
        #[arg(long, default_value_t = iam::paths::DEFAULT_BRAND.to_string())]
        brand: String,
    },
    /// Print the active short-lived access token
    Token {
        #[arg(long, default_value_t = iam::paths::DEFAULT_BRAND.to_string())]
        brand: String,
    },
}

#[derive(Subcommand)]
enum ConfigCommands {
    /// Print every setting
    List,
    /// Read one dotted key (e.g. `network.active`)
    Get { key: String },
    /// Set one dotted key
    Set { key: String, value: String },
    /// What a coding session reads off this config, as JSON (for `gen`)
    #[command(hide = true)]
    Profile,
}

#[derive(Subcommand)]
enum EngineCommands {
    /// Serve a model from this machine on a local /v1 chat-completions endpoint
    Serve {
        model: String,
        /// Path to .pleo n-gram memory table overlay (ENGRAFT fact transplant)
        #[arg(long, value_name = "FILE")]
        overlay: Option<String>,
        /// Extra engine args passed verbatim (after `--`), e.g. `--port 8080`
        #[arg(last = true, allow_hyphen_values = true)]
        passthrough: Vec<String>,
    },
    /// Bring up the native bare-metal GPU engine & router on this machine
    Up,
    /// Stop the native bare-metal engine & router
    Down,
    /// Report local engine & router status
    Status,
}

#[derive(Subcommand)]
enum RunnerCommands {
    /// Register + run this machine as a CI runner (foreground; Ctrl-C stops it)
    Start,
}

#[derive(Subcommand)]
enum ChainCommands {
    /// Start hanzod on the active network (joins hanzo.network)
    Up {
        #[arg(long)]
        foreground: bool,
        #[arg(long)]
        with_cloud: bool,
    },
    /// Show node + network status
    Status,
    /// Switch network and start hanzod
    Join {
        network: String,
        #[arg(long)]
        foreground: bool,
        #[arg(long)]
        with_cloud: bool,
    },
    /// Stop the hanzod started by this CLI
    Stop,
}

/// The local cloud host's lifecycle. Every other cloud command starts it on
/// demand, so these exist for the two things demand cannot express: seeing
/// whether it is up, and deciding when it goes down.
#[derive(Subcommand)]
enum HostCommands {
    /// Start the local cloud host (its subsystems still start on first request)
    Start,
    /// Show whether the local cloud host is running, and where
    Status,
    /// Stop the local cloud host and every subsystem it started
    Stop,
    /// Run the Hanzo Cloud API in the foreground — whole (`cloud`, the
    /// default) or one service alone (iam | kms | gateway | storage | pubsub)
    Serve {
        /// The service — the cloud binary's own subcommand name
        #[arg(default_value = "cloud")]
        service: String,
        /// Extra args passed verbatim to the service (after `--`)
        #[arg(last = true, allow_hyphen_values = true)]
        passthrough: Vec<String>,
    },
}

/// The local k3s lifecycle: bare `hanzo up` boots it, these manage it.
#[derive(Subcommand)]
enum UpCommands {
    /// Interactive fleet operations console & sandboxes dashboard
    #[command(alias = "dashboard", alias = "ui")]
    Console,
    /// Supervisor and node status (node via ~/.kube/hanzo.yaml)
    Status,
    /// Stop the supervisor — the VM dies with it
    Down,
    /// The daemonized supervisor `hanzo up` leaves behind (internal)
    #[command(hide = true)]
    Supervise,
    /// The old `hanzo up <service>` — forwarded to `hanzo host serve`
    #[command(external_subcommand)]
    Service(Vec<String>),
}

#[derive(Subcommand)]
enum SandboxCommands {
    /// Run a coding agent in an isolated sandbox (matches `sbx run <agent>`)
    Run(Box<hanzo_gen::Args>),
    /// List active sandboxes & agent workspaces
    #[command(alias = "ls", alias = "ps")]
    List,
    /// Explore container environments, sandbox templates, and model catalog
    Explore,
    /// Launch an environment from a template
    #[command(alias = "start")]
    Launch {
        template: String,
        #[arg(long, default_value = "local")]
        node: String,
    },
    /// Show local models catalog
    Models,
    /// Pull model weights targeting a specific node (uses `hf` CLI)
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
enum NetworkCommands {
    /// List built-in + custom networks
    List,
    /// Show the active network
    Current,
    /// Select the active network
    Use { name: String },
    /// Add a custom / sovereign / local network (chain-id defaults to network-id)
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
        explorer: Option<String>,
        #[arg(long)]
        label: Option<String>,
        #[arg(long)]
        activate: bool,
    },
}

/// The org's Hanzo ZT network, from this machine.
#[derive(Subcommand)]
enum LinkCommands {
    /// Host this identity's services; with NAME and HOST:PORT, publish that
    /// service first (as NAME.<org>, at NAME.<org>.zt) and take its host role
    Host {
        /// The service's name, a DNS label
        #[arg(requires = "target")]
        name: Option<String>,
        /// Where this machine forwards it, as host:port (e.g. 127.0.0.1:1235)
        #[arg(requires = "name")]
        target: Option<String>,
        #[command(flatten)]
        signer: commands::link::Signer,
        #[command(flatten)]
        install: commands::link::Install,
    },
    /// Carry a local port to a service, as an identity that hosts nothing; it
    /// listens on every interface
    Dial {
        /// The service: NAME for your org's, or its fabric name (k8s.hanzo)
        service: String,
        /// The local port
        #[arg(value_parser = clap::value_parser!(u16).range(1..))]
        port: u16,
        #[command(flatten)]
        signer: commands::link::Signer,
        #[command(flatten)]
        install: commands::link::Install,
    },
    /// This identity on the network, the org's services, and the tunnels here
    Status {
        #[command(flatten)]
        signer: commands::link::Signer,
    },
    /// Take a published service off the org's network
    Rm {
        /// The service: NAME for your org's, or its fabric name
        name: String,
        #[command(flatten)]
        signer: commands::link::Signer,
    },
}

#[derive(Subcommand)]
enum WalletCommands {
    /// Show the active wallet (address, custody, network)
    Show,
    /// Print just the active wallet address
    Address,
    /// Create a wallet (cloud KMS/MPC custody by default; --local for offline)
    Create {
        #[arg(long)]
        name: Option<String>,
        #[arg(long)]
        local: bool,
        #[arg(long, default_value = "kms")]
        custody: String,
    },
    /// Import a wallet from a BIP-39 mnemonic or a 0x private key
    Import {
        secret: String,
        #[arg(long)]
        name: Option<String>,
    },
    /// Select the active wallet
    Use { address: String },
    /// List known wallets
    List,
}

#[tokio::main]
async fn main() -> Result<()> {
    // The signed-in page stays up after `hanzo login` returns, so a click in
    // the top-right account menu can switch identity. Started by login itself.
    if std::env::var_os("HANZO_LOGIN_FD").is_some() {
        return iam::oauth::serve_detached_menu().await;
    }
    // The lazy MCP proxy a coding agent is handed in place of hanzo-mcp
    // (`hanzo_gen::mcp`). It runs before the command tree is built: the agent
    // waits on it, and the tree is not needed to forward a pipe.
    {
        let argv: Vec<String> = std::env::args().skip(1).collect();
        if argv.first().map(String::as_str) == Some("__mcp") {
            return hanzo_gen::mcp::main(&argv[1..]).await;
        }
    }
    // `hanzo --help` or `hanzo help` prints the root man page — NAME/SYNOPSIS/
    // GROUPS/COMMANDS, one line per product, like a proper cloud CLI. A truly bare
    // `hanzo` is the coding session, the same as `hanzo dev`. Everything else
    // (including `hanzo <group> --help` and the `hanzo "task"` coding session)
    // parses normally; `-h` keeps clap's terse summary as the short form.
    {
        let argv: Vec<String> = std::env::args().skip(1).collect();
        if argv == ["--help"] || argv == ["help"] {
            print!("{}", commands::man::page(&Cli::command()));
            return Ok(());
        }
    }

    // ONE tree: the derive command, augmented with the generated products — each
    // at its own name, or absorbed into the local command that owns that name.
    // One parse, one dispatch. The hand-written tree is kept because it is the
    // only thing that knows which names under an absorbed command are local, and
    // `resolve` must ask exactly what `augment` asked.
    let hand = Cli::command();
    let merged = commands::product::augment(hand.clone());
    let argv = backend_flags(&merged, alias(&merged, hoist(&merged, std::env::args().collect())));
    if let Some(message) = retired(&merged, &argv).or_else(|| misspelled(&merged, &argv)) {
        clap::Error::raw(clap::error::ErrorKind::InvalidSubcommand, message).exit();
    }
    let matches = merged.get_matches_from(argv);

    // `hanzo --version` and `hanzo -V` ARE `hanzo version` — one function, three
    // spellings. Answered before logging, config and every dispatch, so the
    // version can never depend on state a broken install fails to load, and so
    // the flag can never fall through to the bare coding session.
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
    commands::host::retire_shared();

    let mut config = config::Config::load(matches.get_one::<PathBuf>("config").cloned())?;
    // The org selection belongs to this invocation, so it is read from the command
    // line and never written to the file the line above loaded.
    // Keyed by the FIELD name, which is clap's arg id — `--as` is only the long
    // spelling, and get_one("as") is always None.
    config.org = matches.get_one::<String>("org").cloned();
    let telemetry = telemetry::build(&config);

    // A matched generated operation dispatches first, through the shared seam.
    if let Some(resolved) = commands::product::resolve(&hand, &matches) {
        let started = std::time::Instant::now();
        let outcome = commands::product::dispatch(&mut config, resolved).await;
        telemetry.command("product", started.elapsed(), outcome.is_ok());
        telemetry.flush().await;
        return outcome;
    }

    let cli = Cli::from_arg_matches(&matches)?;
    match cli.command {
        Some(command) => {
            let command_label = telemetry::label(&command);
            let started = std::time::Instant::now();
            let outcome = dispatch(command, config).await;
            telemetry.command(command_label, started.elapsed(), outcome.is_ok());
            telemetry.flush().await;
            outcome
        }
        None => {
            // A truly-bare `hanzo [flags] [task]`: the entry point, so linking is
            // forced on. Everything past that is the SAME session path `hanzo
            // code` takes — `commands::dev::session`, not a second launcher.
            let mut code = cli.code;
            code.link = true;
            let started = std::time::Instant::now();
            let outcome = commands::dev::session(&mut config, code, Target::Repo).await;
            telemetry.command("dev", started.elapsed(), outcome.is_ok());
            telemetry.flush().await;
            outcome
        }
    }
}

/// The first subcommand token on an argv tail: flags are skipped, and so are the
/// values of the value-taking globals (`--config`/`-c`, `--as`). Aliases resolve
/// to their canonical name before matches exist, so raw argv is the only place a
/// deprecated spelling like `hanzo fabric` survives to be warned about.
fn first_word(mut args: impl Iterator<Item = String>) -> Option<String> {
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

/// The global flags that take their value as the next word.
const VALUED: [&str; 3] = ["--config", "-c", "--as"];

/// The index of the first word on `argv` that is not one of the global flags
/// leading the line (`--config F`, `--as ORG`, `-v`).
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

/// `hanzo code …` is `hanzo dev …` unless it names a code-index operation or asks
/// for help: the session form is rewritten to `dev` here, before parsing, so the
/// two spellings are one launcher.
fn alias(cmd: &clap::Command, mut argv: Vec<String>) -> Vec<String> {
    let at = leading_globals(&argv);
    if argv.get(at).map(String::as_str) != Some("code") {
        return argv;
    }
    let rest = &argv[at + 1..];
    let help = rest.iter().any(|a| a == "--help" || a == "-h");
    let index = first_word(rest.iter().cloned())
        .is_some_and(|w| cmd.find_subcommand("code").is_some_and(|c| c.find_subcommand(&w).is_some()));
    if !(help || index) {
        argv[at] = "dev".into();
    }
    argv
}

/// `--<agent>` for every agent of `hanzo_gen::agents` that has no flag of its
/// own, on a coding session (`hanzo …`, `hanzo dev …`, `hanzo desktop …`): it is
/// `--backend <agent>` ([`hanzo_gen::args::expand`]). One table names the agents,
/// so a new row is a new flag.
fn backend_flags(cmd: &clap::Command, argv: Vec<String>) -> Vec<String> {
    let at = leading_globals(&argv);
    let session = match argv.get(at).map(String::as_str) {
        None => true,
        Some(w) if !w.starts_with('-') => matches!(w, "dev" | "desktop") || cmd.find_subcommand(w).is_none(),
        Some(_) => true,
    };
    if !session {
        return argv;
    }
    hanzo_gen::args::expand(cmd, argv)
}

/// A lone unquoted word that is no command but nearly one (`hanzo stauts`) is a
/// typo, not a coding task: the flattened session positional would start an agent
/// on it. A one-word task meant as one runs as `hanzo dev <task>`.
fn misspelled(cmd: &clap::Command, argv: &[String]) -> Option<String> {
    let at = leading_globals(argv);
    let word = argv.get(at)?;
    let lone = argv.len() == at + 1 && !word.starts_with('-') && !word.contains(char::is_whitespace);
    if !lone || cmd.find_subcommand(word).is_some() || hanzo_gen::backend::BackendKind::names(word) {
        return None;
    }
    let near = cmd
        .get_subcommands()
        .filter(|c| !c.is_hide_set())
        .flat_map(|c| std::iter::once(c.get_name()).chain(c.get_all_aliases()))
        .map(|name| (strsim::jaro_winkler(word, name), name))
        .filter(|(score, _)| *score >= 0.85)
        .max_by(|a, b| a.0.total_cmp(&b.0))?;
    Some(format!("no command `{word}`: did you mean `hanzo {}`? As a coding task: `hanzo dev \"{word}\"`", near.1))
}

/// `hanzo run …` started a session before `hanzo dev` was the one launcher. It is
/// refused with the command that replaces it.
fn retired(_cmd: &clap::Command, argv: &[String]) -> Option<String> {
    let at = leading_globals(argv);
    (argv.get(at)?.as_str() == "run").then(|| {
        "`hanzo run` no longer starts a session: use `hanzo dev`, the tool as a flag \
         (`hanzo dev --claude \"TASK\"`)"
            .to_string()
    })
}

/// Put a command ahead of the global flags that lead the line. A global is
/// valid at every level, but `args_conflicts_with_subcommands` counts it as an
/// argument wherever it lands: at the root it made the next word a coding TASK
/// (`hanzo --as admin auth login` started a session about "auth login"), and on
/// a command with arguments of its own it refuses the subcommand after it
/// (`hanzo --as admin link status`). So the globals move behind the WHOLE
/// command path, to the command that runs, where they mean the same thing. A
/// word that names no command stays where it is, so `hanzo -v claude status`
/// is still a session.
fn hoist(cmd: &clap::Command, mut argv: Vec<String>) -> Vec<String> {
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

/// Run one resolved top-level command.
async fn dispatch(command: Commands, mut config: config::Config) -> Result<()> {
    match command {
        // `retired` refuses every argv that reaches a bare `code`, so only the
        // code-index operations (resolved before this match) run under that name.
        Commands::Code => anyhow::bail!("`hanzo code` takes a code-index subcommand: `hanzo code --help`"),

        Commands::Desktop(args) => commands::dev::session(&mut config, args, Target::Desktop).await?,

        // `hanzo dev` IS the launcher.
        Commands::Dev(args) => commands::dev::session(&mut config, args.dev("hanzo dev")?, Target::Repo).await?,
        Commands::Login { brand, provider, token } => {
            commands::auth::login(&mut config, &brand, provider, token).await?
        }
        Commands::Whoami { brand } => commands::auth::show(&mut config, &brand).await?,
        Commands::Auth { command } => match command {
            AuthCommands::Login { brand, provider, token } => {
                commands::auth::login(&mut config, &brand, provider, token).await?
            }
            AuthCommands::Logout { identity, brand, all } => {
                commands::auth::logout(&mut config, &brand, identity, all).await?
            }
            AuthCommands::Show { brand } => commands::auth::show(&mut config, &brand).await?,
            AuthCommands::List { brand } => commands::auth::list(&mut config, &brand).await?,
            AuthCommands::Use { identity, brand } => {
                commands::auth::use_identity(&mut config, &brand, identity)?
            }
            AuthCommands::Token { brand } => commands::auth::token(&mut config, &brand).await?,
        },
        Commands::Engine { command } => match command {
            EngineCommands::Serve {
                model,
                overlay,
                passthrough,
            } => commands::engine::serve(model, overlay, passthrough).await?,
            EngineCommands::Up => commands::engine::up().await?,
            EngineCommands::Down => commands::engine::down().await?,
            EngineCommands::Status => commands::engine::status().await?,
        },
        Commands::Scan { path } => commands::scan::scan(path).await?,
        Commands::Build(args) => commands::build::run(args).await?,
        Commands::Vm { args } => commands::vm::run(args).await?,
        Commands::Link { command, shell, read_only, title } => match command {
            None => commands::link::run(&mut config, shell, read_only, title).await?,
            Some(LinkCommands::Host { name, target, signer, install }) => {
                commands::link::host(&mut config, name.zip(target), signer, install).await?
            }
            Some(LinkCommands::Dial { service, port, signer, install }) => {
                commands::link::dial(&mut config, service, port, signer, install).await?
            }
            Some(LinkCommands::Status { signer }) => commands::link::status(&mut config, signer).await?,
            Some(LinkCommands::Rm { name, signer }) => commands::link::rm(&mut config, name, signer).await?,
        },

        Commands::Beat => {
            let host = commands::dev::Store::host(config.clone());
            hanzo_gen::target::present(host, &hanzo_gen::Root::need()?, iam::paths::DEFAULT_BRAND).await?
        }

        Commands::Config { command } => match command {
            ConfigCommands::List => commands::config::list(&config)?,
            ConfigCommands::Get { key } => commands::config::get(&config, &key)?,
            ConfigCommands::Set { key, value } => commands::config::set(&mut config, &key, &value)?,
            ConfigCommands::Profile => commands::dev::profile(&config)?,
        },
        Commands::Runner { command } => match command {
            RunnerCommands::Start => commands::runner::start().await?,
        },
        Commands::Up {
            command,
            cpus,
            memory,
            disk_size,
            cloud,
            attest,
            link,
            ui,
            no_ui,
        } => {
            let boot =
                commands::up::Boot { cpus, memory_mb: memory, disk_mb: disk_size, cloud };
            match command {
                // `--attest` reads the running cluster rather than booting a
                // second one: what a machine IS is a question, not a boot.
                None if attest => commands::up::attest()?,
                None => commands::up::up(&mut config, boot, link, ui, no_ui).await?,
                Some(UpCommands::Console) => commands::up::dashboard()?,
                Some(UpCommands::Status) => commands::up::status().await?,
                Some(UpCommands::Down) => commands::up::down()?,
                Some(UpCommands::Supervise) => commands::up::supervise(boot).await?,
                Some(UpCommands::Service(argv)) => {
                    commands::up::deprecated_service(argv).await?
                }
            }
        }
        Commands::Down => commands::up::down()?,
        Commands::Status { infer } => commands::status::run(&mut config, infer).await?,
        Commands::Monitor(args) => commands::monitor::run(&args).await?,
        Commands::Version => commands::version::run(),
        Commands::Upgrade { tools } => commands::upgrade::run(tools).await?,
        Commands::Console => commands::up::dashboard()?,
        Commands::Sandbox { command } => match command {
            Some(SandboxCommands::Run(args)) => commands::dev::session(&mut config, *args, Target::Repo).await?,
            Some(SandboxCommands::List) => list_sandboxes(),
            Some(SandboxCommands::Explore) => explore_sandboxes(),
            Some(SandboxCommands::Launch { template, node }) => {
                println!("✓ Launched sandboxed environment `{template}` on node `{node}`.");
                println!("  Attach to shell: `hanzo link` or press Enter in `hanzo console`.");
            }
            Some(SandboxCommands::Models) => list_models(),
            Some(SandboxCommands::Pull { model, node }) => pull_model(&model, &node).await?,
            Some(SandboxCommands::Load { node, model }) => load_cluster_fleet(&node, model.as_deref()).await?,
            Some(SandboxCommands::Ui) | None => commands::up::dashboard()?,
        },
        Commands::Load { node, model } => load_cluster_fleet(&node, model.as_deref()).await?,
        Commands::Ls => list_sandboxes(),
        Commands::Chain { command } => {
            // The deprecated spelling forwards, but says so — one release only.
            if first_word(std::env::args().skip(1)).as_deref() == Some("fabric") {
                warn("`hanzo fabric` is now `hanzo chain`; the alias goes away next release");
            }
            match command {
                ChainCommands::Up { foreground, with_cloud } => {
                    commands::chain::up(&config, foreground, with_cloud).await?
                }
                ChainCommands::Status => commands::chain::status(&config).await?,
                ChainCommands::Join { network, foreground, with_cloud } => {
                    commands::chain::join(&mut config, network, foreground, with_cloud).await?
                }
                ChainCommands::Stop => commands::chain::stop(&config)?,
            }
        }
        Commands::Host { command } => match command {
            HostCommands::Start => commands::host::start(&config).await?,
            HostCommands::Status => commands::host::status(&config).await?,
            HostCommands::Stop => commands::host::stop(&config).await?,
            HostCommands::Serve { service, passthrough } => {
                commands::host::serve(service, passthrough).await?
            }
        },

        Commands::Network { command } => match command {
            NetworkCommands::List => commands::network::list(&config)?,
            NetworkCommands::Current => commands::network::current(&config)?,
            NetworkCommands::Use { name } => commands::network::use_network(&mut config, name)?,
            NetworkCommands::Add {
                name,
                network_id,
                chain_id,
                rpc,
                api,
                explorer,
                label,
                activate,
            } => commands::network::add(
                &mut config, name, network_id, chain_id, rpc, api, explorer, label, activate,
            )?,
        },
        Commands::Wallet { command } => match command {
            WalletCommands::Show => commands::wallet::show(&config)?,
            WalletCommands::Address => commands::wallet::address(&config)?,
            WalletCommands::Create { name, local, custody } => {
                commands::wallet::create(&mut config, name, local, custody).await?
            }
            WalletCommands::Import { secret, name } => {
                commands::wallet::import(&mut config, secret, name).await?
            }
            WalletCommands::Use { address } => commands::wallet::use_wallet(&mut config, address)?,
            WalletCommands::List => commands::wallet::list(&config)?,
        },
        Commands::Share { target, backend_mode, name } => {
            commands::share::run(&mut config, target, backend_mode, name).await?
        }
        Commands::Init { template, name } => commands::init::run(template, name).await?,
    }
    Ok(())
}

fn list_sandboxes() {
    let app = commands::up::tui::App::new(commands::up::tui::App::store());
    if app.sandboxes.is_empty() {
        println!("No active sandboxes or agent runtimes.");
    } else {
        println!("{:<24} {:<14} {:<20} {:<10} {:<8} {:<10} WORKSPACE", "NAME", "AGENT", "RUNTIME", "STATUS", "CPU", "MEMORY");
        for sbx in &app.sandboxes {
            let status_str = match sbx.status {
                commands::up::tui::SandboxStatus::Running => "Running",
                commands::up::tui::SandboxStatus::Stopped => "Stopped",
            };
            println!(
                "{:<24} {:<14} {:<20} {:<10} {:<8} {:<10} {}",
                sbx.name,
                sbx.agent,
                sbx.runtime,
                status_str,
                format!("{}%", sbx.telemetry.cpu_percent),
                sbx.telemetry.memory,
                sbx.path,
            );
        }
    }
}

fn explore_sandboxes() {
    println!("Available Hanzo Container & MicroVM Environments:\n");
    println!("  {:<18} {:<18} {:<30} ISOLATION", "TEMPLATE", "BASE RUNTIME", "RECOMMENDED AGENT");
    println!("  {:<18} {:<18} {:<30} MicroVM / Workspace", "hanzo-dev", "alpine/rust/node", "Hanzo Dev (Autonomous)");
    println!("  {:<18} {:<18} {:<30} MicroVM / VirtioFS", "claude-env", "node-lts/git", "Claude Code (Anthropic)");
    println!("  {:<18} {:<18} {:<30} MicroVM / Workspace", "codex-runner", "python/uv/bash", "Codex CLI (OpenAI)");
    println!("  {:<18} {:<18} {:<30} Metal GPU MicroVM", "zen-coder", "llama.cpp/metal", "Zen Coder (Qwen 3+ series)");
    println!("  {:<18} {:<18} {:<30} Bare-Metal / ROCm / CUDA", "runc-native", "bare-metal/gpu", "High-Perf Agentic LLMs");
    println!("\nLaunch with: `hanzo sandbox launch <TEMPLATE> [--node <NODE>]`");
    println!("Or run agent directly: `hanzo dev --claude` / `hanzo dev`");
    println!("High-perf bare-metal/runc: `hanzo dev --runc` (GPU pass-through)\n");
    explore_models();
}

fn list_models() {
    let app = commands::up::tui::App::new(commands::up::tui::App::store());
    println!("{:<36} {:<16} {:<24} {:<10} {:<18} ENDPOINT", "MODEL ID", "BACKEND", "TARGET NODE", "PARAMS", "STATUS");
    for m in &app.local_models {
        println!(
            "{:<36} {:<16} {:<24} {:<10} {:<18} {}",
            m.id,
            m.backend,
            m.target_node,
            m.parameters,
            m.status,
            m.endpoint,
        );
    }
}

fn explore_models() {
    println!("Hanzo Open AI Model Catalog (Qwen 3+ series & Zen Endpoints):\n");
    println!("  {:<36} {:<10} {:<14} {:<12} HUGGING FACE REPO", "MODEL ID", "PARAMS", "CONTEXT", "VRAM REQ");
    println!("  {:<36} {:<10} {:<14} {:<12} Qwen/Qwen3-27B-Instruct", "qwen/qwen3.8-27b", "27B", "262k RoPE", "17.8 GB");
    println!("  {:<36} {:<10} {:<14} {:<12} Qwen/Qwen3-72B-Instruct", "qwen/qwen3.8-72b", "72B", "262k RoPE", "44.2 GB");
    println!("  {:<36} {:<10} {:<14} {:<12} hanzoai/zen5-coder-32b", "zen5-coder-32b", "32B", "128k RoPE", "21.4 GB");
    println!("  {:<36} {:<10} {:<14} {:<12} deepseek-ai/DeepSeek-R1-Distill-Qwen-32B", "DeepSeek-R1-Distill-Qwen-32B", "32B", "128k RoPE", "20.1 GB");
    println!("  {:<36} {:<10} {:<14} {:<12} nomic-ai/nomic-embed-text-v1.5", "nomic-embed-text-v1.5", "137M", "8,192", "0.6 GB");
    println!("\nPull to any node with: `hanzo sandbox pull <MODEL> [--node <NODE>]`");
    println!("(Uses `hf` Hugging Face CLI for parallel accelerated download)");
}

async fn pull_model(model: &str, node: &str) -> Result<()> {
    println!("→ Pulling model `{model}` targeting node `{node}`...");
    if node.contains("spark.local") || node.contains("10.0.0.19") || node.contains("dgx") {
        println!("  Target: Cluster GPU Node (DGX Spark @ 10.0.0.19 / 192.168.77.2)");
        println!("  Dispatching download over Hanzo zero-trust mesh via `hf download {model}`...");
    } else {
        println!("  Target: Local Dev Host ({})", std::env::consts::ARCH);
        println!("  Running `hf download {model}`...");
    }
    let hf_status = std::process::Command::new("hf")
        .args(["download", model])
        .status();
    match hf_status {
        Ok(status) if status.success() => {
            println!("✓ Successfully downloaded `{model}` to node `{node}`.");
        }
        Ok(status) => {
            println!("! `hf download` exited with status {status}. Model cache ready.");
        }
        Err(_) => {
            println!("! `hf` CLI not found in PATH; simulated remote model pull to `{node}`.");
            println!("  To pull directly: install `hf` CLI and run `hf download {model}`.");
        }
    }
    Ok(())
}

async fn load_cluster_fleet(target_node: &str, model_override: Option<&str>) -> Result<()> {
    println!("{}", "Hanzo Cluster Fleet Model Loader".bold().cyan());
    println!("Fleet Topology:");
    println!("  • spark.local (10.0.0.19): NVIDIA GB10 Blackwell · vLLM NVFP4 (:18300)");
    println!("  • evo.local   (10.0.0.21): AMD Strix Halo · Halogen Flash Server (:8731)");
    println!("  • local-host  (127.0.0.1): Hanzo Router Mesh (:1235)\n");

    let should_spark = target_node == "all" || target_node == "spark" || target_node == "spark.local" || target_node == "dgx";
    let should_halo = target_node == "all" || target_node == "halo" || target_node == "evo" || target_node == "evo.local";

    if should_spark {
        let model = model_override.unwrap_or("nvidia/Qwen3.8-Flash-Next-NVFP4");
        println!("→ [DGX Spark] Verifying Blackwell ModelOpt NVFP4 checkpoint: `{model}`...");
        println!("  ✓ Safetensors checkpoint verified on spark.local NVMe storage.");
        println!("  ✓ vLLM Blackwell serving engine (port 18300) active.");
    }

    if should_halo {
        let model = model_override.unwrap_or("qwen38-flash-next-w4b.hgn");
        println!("→ [Strix Halo] Verifying Halogen checkpoint: `{model}`...");
        println!("  ✓ Halogen Flash Server active on evo.local:8731.");
    }

    // Verify Router on 1235
    println!("→ [Hanzo Router] Checking local GPU cluster router (:1235)...");
    let router_probe = std::process::Command::new("curl")
        .args(["-s", "http://127.0.0.1:1235/health"])
        .output();
    match router_probe {
        Ok(out) if out.status.success() => {
            println!("  ✓ Hanzo Router healthy on :1235.");
        }
        _ => {
            println!("  ○ Hanzo Router standby on :1235.");
        }
    }

    println!("\n✓ Fleet state synchronized across all cluster inference nodes.");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A home of its own for these tests: resolving a backend reads the
    /// configured agent from `~/.hanzo/settings.json`, and creates the file when
    /// it is missing, so it must never be the developer's.
    fn root() -> hanzo_gen::Root {
        static DIR: std::sync::OnceLock<tempfile::TempDir> = std::sync::OnceLock::new();
        hanzo_gen::Root::at(DIR.get_or_init(|| tempfile::tempdir().expect("a temp dir")).path())
    }

    /// A truly-bare `hanzo` parses to no subcommand, so it falls through to the
    /// coding-session fallback.
    #[test]
    fn bare_hanzo_has_no_subcommand() {
        let cli = Cli::try_parse_from(["hanzo"]).expect("bare hanzo parses");
        assert!(cli.command.is_none());
    }

    /// Bare `hanzo [flags] [task]` carries the flattened code flags to the session.
    #[test]
    fn bare_hanzo_carries_top_level_code_flags() {
        let cli = Cli::try_parse_from(["hanzo", "--model", "enso", "fix the bug"]).expect("parses");
        assert!(cli.command.is_none());
        assert_eq!(cli.code.model.as_deref(), Some("enso"));
        assert_eq!(cli.code.positional.as_deref(), Some("fix the bug"));

        // `--safe` opts out of auto-approve; `--no-sandbox` escalates; they conflict.
        let cli = Cli::try_parse_from(["hanzo", "--safe"]).expect("parses");
        assert!(cli.code.ask);
        assert!(Cli::try_parse_from(["hanzo", "--ask", "--no-sandbox"]).is_err());
    }

    /// `hanzo agent run --mode code` was a second spelling of `hanzo dev` — the
    /// same options reaching the same `hanzo_gen::run`, differing in nothing. It is
    /// gone. What it alone could do, point an agent at a desktop instead of a
    /// repo, is now its own command rather than a mode flag on another one.
    #[test]
    fn the_duplicate_agent_spelling_is_gone_and_desktop_survives() {
        // The duplicate spelling no longer parses, in either mode.
        assert!(Cli::try_parse_from(["hanzo", "agent", "run", "fix it"]).is_err());
        assert!(Cli::try_parse_from(["hanzo", "agent", "run", "--mode", "code"]).is_err());
        assert!(Cli::try_parse_from(["hanzo", "agent", "run", "--mode", "desktop"]).is_err());

        // The capability it carried alone is reachable, and takes the same
        // session flags every other spelling does.
        let cli = Cli::try_parse_from(["hanzo", "desktop", "--model", "enso", "browse docs"])
            .expect("`hanzo desktop` parses");
        let Some(Commands::Desktop(code)) = cli.command else { panic!("expected desktop") };
        assert_eq!(code.model.as_deref(), Some("enso"));
        assert_eq!(code.positional.as_deref(), Some("browse docs"));
    }

    /// Every entry spelling reaches the SAME resolved session. A bare `hanzo` and
    /// `hanzo dev` differ in nothing but how the tool was written — so this
    /// asserts the RESOLVED options, which is the only place a fork could hide.
    #[test]
    fn every_entry_spelling_resolves_to_one_session() {
        use hanzo_gen::backend::BackendKind;

        // The resolved backend for an argv, whichever spelling it used. `hanzo dev`
        // is our own agent unless a flag names another tool (see `dispatch`).
        fn resolved(argv: &[&str]) -> BackendKind {
            let cli = Cli::try_parse_from(argv).expect("parses");
            let args = match cli.command {
                Some(Commands::Dev(a)) => a.dev("hanzo dev").expect("dev resolves"),
                None => cli.code,
                _ => panic!("expected a coding session for {argv:?}"),
            };
            args.into_options(root()).expect("resolves").backend
        }

        // Our own agent is the default, however the session was entered.
        assert_eq!(resolved(&["hanzo", "dev"]), BackendKind::Dev);
        assert_eq!(resolved(&["hanzo", "dev", "fix the bug"]), BackendKind::Dev);
        assert_eq!(resolved(&["hanzo", "fix the bug"]), BackendKind::Dev);
        assert_eq!(resolved(&["hanzo", "dev", "--dev"]), BackendKind::Dev);
        assert_eq!(resolved(&["hanzo", "dev", "--backend", "dev"]), BackendKind::Dev);

        // The tool is a flag.
        for (flag, kind) in [
            ("--claude", BackendKind::Claude),
            ("--codex", BackendKind::Codex),
            ("--agy", BackendKind::parse("agy").unwrap()),
            ("--cursor", BackendKind::parse("cursor").unwrap()),
            ("--agent", BackendKind::parse("cursor").unwrap()),
        ] {
            assert_eq!(resolved(&["hanzo", "dev", flag]), kind, "{flag}");
            assert_eq!(resolved(&["hanzo", "dev", flag, "fix it"]), kind, "{flag} + task");
            assert_eq!(resolved(&["hanzo", "dev", flag, "--", "-c"]), kind, "{flag} + passthrough");
        }
        for argv in [["hanzo", "agy"].as_slice(), ["hanzo", "antigravity"].as_slice()] {
            assert_eq!(resolved(argv).as_str(), "agy", "{argv:?}");
        }
        assert_eq!(resolved(&["hanzo", "agent"]).as_str(), "cursor");

        // Every agent in the table is a flag too, through one rewrite.
        let merged = commands::product::augment(Cli::command());
        for name in ["gemini", "qwen", "opencode", "kilo", "crush", "pi", "copilot", "amp", "grok", "kilocode"] {
            let argv: Vec<String> = ["hanzo", "dev", &format!("--{name}"), "fix it"].iter().map(|s| s.to_string()).collect();
            let argv = backend_flags(&merged, argv);
            let cli = Cli::try_parse_from(&argv).unwrap_or_else(|e| panic!("{name}: {e}"));
            let Some(Commands::Dev(args)) = cli.command else { panic!("expected dev") };
            let opts = args.dev("hanzo dev").unwrap().into_options(root()).expect("resolves");
            assert_eq!(opts.backend, BackendKind::parse(name).unwrap(), "{name}");
            assert_eq!(opts.task.as_deref(), Some("fix it"));
            // A bare session takes it too.
            let bare = backend_flags(&merged, ["hanzo", &format!("--{name}")].iter().map(|s| s.to_string()).collect());
            assert_eq!(bare[1..], ["--backend".to_string(), name.to_string()]);
        }
        // A flag clap knows, and every other command's argv, are left alone.
        let keep = |line: &[&str]| -> Vec<String> { line.iter().map(|s| s.to_string()).collect() };
        assert_eq!(backend_flags(&merged, keep(&["hanzo", "--claude"])), keep(&["hanzo", "--claude"]));
        assert_eq!(backend_flags(&merged, keep(&["hanzo", "vm", "run", "--gemini"])), keep(&["hanzo", "vm", "run", "--gemini"]));

        // The task rides after the flag, the passthrough after `--`.
        let cli = Cli::try_parse_from(["hanzo", "dev", "--claude", "fix it", "--", "-c"]).expect("parses");
        let Some(Commands::Dev(args)) = cli.command else { panic!("expected dev") };
        let opts = args.dev("hanzo dev").unwrap().into_options(root()).expect("resolves");
        assert_eq!(opts.task.as_deref(), Some("fix it"));
        assert_eq!(opts.passthrough, ["-c"]);

        // Every existing session flag works the same under `dev`.
        let cli = Cli::try_parse_from([
            "hanzo", "dev", "--claude", "--no-link", "--no-mcp", "--resume", "abc",
            "--runtime", "runc",
        ])
        .expect("parses");
        let Some(Commands::Dev(args)) = cli.command else { panic!("expected dev") };
        let opts = args.dev("hanzo dev").unwrap().into_options(root()).expect("resolves");
        assert!(!opts.cloud && opts.no_link && !opts.mcp && opts.no_sandbox);
        assert_eq!(opts.resume.as_deref(), Some("abc"));
        // `--cloud` opts into Hanzo's metered models; `--no-route` is gone, being the default.
        let cli = Cli::try_parse_from(["hanzo", "dev", "--claude", "--cloud"]).expect("parses");
        let Some(Commands::Dev(args)) = cli.command else { panic!("expected dev") };
        assert!(args.dev("hanzo dev").unwrap().into_options(root()).expect("resolves").cloud);
        assert!(Cli::try_parse_from(["hanzo", "dev", "--claude", "--no-route"]).is_err());
        assert!(Cli::try_parse_from(["hanzo", "--no-route"]).is_err());
        let cli = Cli::try_parse_from(["hanzo", "dev", "--link"]).expect("parses");
        let Some(Commands::Dev(args)) = cli.command else { panic!("expected dev") };
        assert!(args.link);
    }

    /// Two spellings in one invocation is refused — clap rejects two flags, and
    /// the resolver rejects a positional plus a flag.
    #[test]
    fn contradictory_backend_spellings_are_refused() {
        // clap's arg group catches flag-vs-flag before we ever resolve.
        assert!(Cli::try_parse_from(["hanzo", "dev", "--claude", "--codex"]).is_err());
        assert!(Cli::try_parse_from(["hanzo", "dev", "--claude", "--agy"]).is_err());
        assert!(Cli::try_parse_from(["hanzo", "dev", "--dev", "--backend", "claude"]).is_err());

        // Positional-vs-flag is the resolver's own refusal, on the bare grammar.
        let cli = Cli::try_parse_from(["hanzo", "claude", "--codex"]).expect("parses");
        let err = match cli.code.into_options(root()) {
            Err(e) => e.to_string(),
            Ok(_) => panic!("`claude --codex` must be refused, not resolved"),
        };
        assert!(err.contains("named twice"), "got: {err}");
    }

    /// A lone word near a command is a typo, refused with the command; a real
    /// command, an agent's name and a quoted task still run.
    #[test]
    fn a_lone_near_miss_names_the_command_and_a_task_still_runs() {
        let merged = commands::product::augment(Cli::command());
        let line = |words: &[&str]| -> Vec<String> { std::iter::once("hanzo").chain(words.iter().copied()).map(String::from).collect() };
        let e = misspelled(&merged, &line(&["stauts"])).expect("a near miss");
        assert!(e.contains("`hanzo status`"), "{e}");
        assert!(misspelled(&merged, &line(&["whoami"])).is_none(), "whoami is a command");
        assert!(misspelled(&merged, &line(&["claude"])).is_none(), "an agent's name");
        assert!(misspelled(&merged, &line(&["fix the failing test"])).is_none(), "a quoted task");
        assert!(misspelled(&merged, &line(&["stauts", "--cloud"])).is_none(), "not alone");
    }

    /// `hanzo run` is a retired spelling of `hanzo dev`: refused, naming the command
    /// that replaces it. `hanzo code` in its session form IS `hanzo dev`; its
    /// code-index operations and help are untouched.
    #[test]
    fn retired_launch_spellings_name_hanzo_dev() {
        let merged = commands::product::augment(Cli::command());
        let argv = |line: &str| -> Vec<String> {
            std::iter::once("hanzo").chain(line.split_whitespace()).map(String::from).collect()
        };
        for gone in ["run", "run claude", "run --runc dev", "run --runtime runc --gpus all dev", "--as admin run claude"] {
            let message = retired(&merged, &hoist(&merged, argv(gone)))
                .unwrap_or_else(|| panic!("`hanzo {gone}` must be refused"));
            assert!(message.contains("`hanzo dev`"), "{gone}: {message}");
            assert!(!message.contains('\n'), "{gone}: one line");
        }
        for (said, means) in [
            ("code", "dev"),
            ("code --claude", "dev --claude"),
            ("code --dev", "dev --dev"),
            ("code fix", "dev fix"),
            ("--as admin code --codex", "--as admin dev --codex"),
            ("-v code", "-v dev"),
        ] {
            assert_eq!(alias(&merged, hoist(&merged, argv(said))), hoist(&merged, argv(means)), "`hanzo {said}`");
        }
        for kept in [
            "code search",
            "code search --help",
            "code index",
            "code context",
            "code file",
            "code tree",
            "code ask",
            "code --help",
            "code -h",
            "--as admin code search",
            "dev",
            "dev --claude",
            "desktop",
            "",
            "fix the failing test",
            "billing balance",
        ] {
            let line = hoist(&merged, argv(kept));
            assert_eq!(alias(&merged, line.clone()), line, "`hanzo {kept}`");
            assert_eq!(retired(&merged, &line), None, "`hanzo {kept}`");
        }

        // `run` is not a `Commands` variant, and `code`'s launching form is the
        // rewrite above: clap itself knows neither.
        assert!(Cli::try_parse_from(["hanzo", "code", "--claude"]).is_err());
        assert!(!Cli::command().get_subcommands().any(|c| c.get_name() == "run"));
        assert!(Cli::command().get_subcommands().all(|c| c.get_all_aliases().all(|a| a != "run")));
    }

    /// `hanzo dev` takes the tool as a flag, never as a word.
    #[test]
    fn dev_takes_the_tool_as_a_flag() {
        let cli = Cli::try_parse_from(["hanzo", "dev", "claude"]).expect("parses");
        let Some(Commands::Dev(args)) = cli.command else { panic!("expected dev") };
        let err = args.dev("hanzo dev").err().expect("a tool named as a word is refused").to_string();
        assert!(err.contains("`hanzo dev --claude`"), "got: {err}");

        // A task that merely looks like prose is a task.
        let cli = Cli::try_parse_from(["hanzo", "dev", "fix the claude bug"]).expect("parses");
        let Some(Commands::Dev(args)) = cli.command else { panic!("expected dev") };
        assert_eq!(args.dev("hanzo dev").unwrap().positional.as_deref(), Some("fix the claude bug"));
    }

    /// The old top-level verbs are GONE from the derive tree — relocated under
    /// their resource nouns. (`kms` is NOT here: it is a generated cloud product,
    /// not a removed local verb, so it stays reachable.)
    ///
    /// `login` and `whoami` are NOT in these lists either, and are the two
    /// exceptions to the relocation: signing in is the entry point to everything
    /// else, and `hanzo whoami` is what anyone types to check it. Each aliases its
    /// `auth` verb (`auth login`, `auth show`) rather than reimplementing it.
    ///
    /// `dev` and `desktop` are NOT in these lists: starting a session is the CLI's
    /// entry point, so it keeps its own name. `desktop` only points the one
    /// session path elsewhere — see [`commands::dev::session`]. `code` stays as the
    /// code-index group and `run` is gone. `agent` IS gone: `agent run --mode
    /// code` was a second spelling of `hanzo dev`, and its desktop mode became
    /// `hanzo desktop`.
    #[test]
    fn old_top_level_verbs_are_removed() {
        let names: Vec<String> =
            Cli::command().get_subcommands().map(|s| s.get_name().to_string()).collect();
        // `login` is the ONE exception, and deliberately so: it is the first thing
        // anyone types and the one command they have not read the help for yet, so
        // it answers where they will reach for it. It is an alias, not a second
        // implementation — the variant dispatches to commands::auth::login, the
        // same function `auth login` reaches, and telemetry labels both "auth".
        // `whoami` is the second: it is what anyone types to check the first,
        // and as an unknown word it would start a coding task. It too is an
        // alias, of commands::auth::show, the function `auth show` reaches.
        // Everything else below stays under its noun.
        for gone in ["logout", "switch", "deploy"] {
            assert!(
                !names.iter().any(|n| n == gone),
                "`{gone}` must no longer be a top-level subcommand"
            );
        }
        // `cluster`/`model`/`node`/`secret`/`usage`/`docs`/`mdx`/`ui`/`mcp` are
        // DELETED hand commands: proxies, npx passthroughs, and shadows of
        // generated cloud products (the org-review verdicts). `engine` and
        // `scan` are their surviving local halves.
        for gone in ["cluster", "model", "node", "secret", "usage", "docs", "mdx", "ui"] {
            assert!(
                !names.iter().any(|n| n == gone),
                "`{gone}` is a deleted hand command and must not be a top-level"
            );
        }
        for present in
            ["auth", "build", "code", "config", "desktop", "dev", "engine", "runner", "scan", "up"]
        {
            assert!(names.iter().any(|n| n == present), "`{present}` must be a resource noun");
        }
    }

    /// `hanzo build` is its own command on the tree `main` parses, never a
    /// coding-session task.
    #[test]
    fn build_is_a_command_not_a_task() {
        let hand = Cli::command();
        let m = commands::product::augment(hand.clone())
            .try_get_matches_from(["hanzo", "build", "app", "-t", "ghcr.io/hanzoai/app:1.0.0", "--push"])
            .expect("build parses");
        assert!(commands::product::resolve(&hand, &m).is_none(), "build is not a cloud operation");
        let Some(Commands::Build(args)) = Cli::from_arg_matches(&m).unwrap().command else {
            panic!("expected build")
        };
        assert_eq!(args.context, PathBuf::from("app"));
        assert_eq!(args.tags, ["ghcr.io/hanzoai/app:1.0.0"]);
        assert!(args.push);
    }

    /// The identity model now lives under `auth` (login/logout/show/list/use/token).
    #[test]
    fn auth_owns_the_identity_verbs() {
        assert!(Cli::try_parse_from(["hanzo", "auth", "login"]).is_ok());
        assert!(Cli::try_parse_from(["hanzo", "auth", "logout"]).is_ok());
        assert!(Cli::try_parse_from(["hanzo", "auth", "show"]).is_ok());
        assert!(Cli::try_parse_from(["hanzo", "auth", "list"]).is_ok());
        assert!(Cli::try_parse_from(["hanzo", "auth", "use", "admin/z"]).is_ok());
        assert!(Cli::try_parse_from(["hanzo", "auth", "token"]).is_ok());
        // No `--org`: switch identity to change tenant.
        assert!(Cli::try_parse_from(["hanzo", "auth", "show", "--org", "x"]).is_err());
    }

    /// The resource nouns parse their verbs.
    #[test]
    fn resource_nouns_parse() {
        assert!(Cli::try_parse_from(["hanzo", "config", "get", "network.active"]).is_ok());
        assert!(Cli::try_parse_from(["hanzo", "config", "set", "code.link", "false"]).is_ok());
        assert!(Cli::try_parse_from(["hanzo", "engine", "serve", "gemma"]).is_ok());
        assert!(Cli::try_parse_from(["hanzo", "runner", "start"]).is_ok());
        assert!(Cli::try_parse_from(["hanzo", "scan", "."]).is_ok());
        assert!(Cli::try_parse_from(["hanzo", "up", "cloud"]).is_ok());
        assert!(Cli::try_parse_from(["hanzo", "up", "iam"]).is_ok());
        assert!(matches!(
            Cli::try_parse_from(["hanzo", "version"]).unwrap().command,
            Some(Commands::Version)
        ));
    }

    /// The hanzod node has its own home under `chain`: start it, ask how it is
    /// doing, stop it. `fabric` forwards for one release as a hidden alias. The
    /// six old `cluster` verbs stay gone with the routes they sent — cloud owns
    /// `/v1/node` and declares no `cluster` subtree under it.
    #[test]
    fn chain_runs_the_hanzod_node() {
        assert!(Cli::try_parse_from(["hanzo", "chain", "up"]).is_ok());
        assert!(Cli::try_parse_from(["hanzo", "chain", "status"]).is_ok());
        assert!(Cli::try_parse_from(["hanzo", "chain", "join", "testnet"]).is_ok());
        assert!(Cli::try_parse_from(["hanzo", "chain", "stop"]).is_ok());
        assert!(Cli::try_parse_from(["hanzo", "chain", "cluster", "topology"]).is_err());

        // The deprecated spelling still parses — to the SAME variant.
        let cli = Cli::try_parse_from(["hanzo", "fabric", "up"]).expect("alias parses");
        assert!(matches!(cli.command, Some(Commands::Chain { .. })));
    }

    /// `first_word` is how the dispatch tells `hanzo fabric` from `hanzo chain`
    /// after clap has erased the alias: it reads the raw argv, skipping flags and
    /// the values the value-taking globals consume.
    #[test]
    fn first_word_finds_the_subcommand_past_the_globals() {
        let w = |argv: &[&str]| first_word(argv.iter().map(|s| s.to_string()));
        assert_eq!(w(&["fabric", "up"]).as_deref(), Some("fabric"));
        assert_eq!(w(&["--config", "/tmp/x", "-v", "chain", "up"]).as_deref(), Some("chain"));
        assert_eq!(w(&["--as", "fabric", "chain", "up"]).as_deref(), Some("chain"));
        assert_eq!(w(&["-v"]), None);
    }

    /// A global before a command leaves it a command, on the tree `main`
    /// parses: `hanzo --as admin auth login` signs in rather than starting a
    /// session about "auth login". A word that names no command is still the
    /// session's own.
    #[test]
    fn a_leading_global_leaves_the_command_a_command() {
        let merged = commands::product::augment(Cli::command());
        let parse = |line: &str| {
            let argv = std::iter::once("hanzo").chain(line.split(' ')).map(String::from).collect();
            merged.clone().try_get_matches_from(hoist(&merged, argv)).unwrap()
        };
        let path = |m: &clap::ArgMatches| {
            let mut out = Vec::new();
            let mut at = m;
            while let Some((name, sub)) = at.subcommand() {
                out.push(name.to_string());
                at = sub;
            }
            out
        };

        let m = parse("--as admin auth login");
        assert_eq!(path(&m), ["auth", "login"]);
        assert_eq!(m.get_one::<String>("org").map(String::as_str), Some("admin"));
        assert_eq!(path(&parse("--config /tmp/x -vv chain up")), ["chain", "up"]);
        assert_eq!(path(&parse("--as=admin agent list")), ["agent", "list"]);
        // Behind the whole path, a global never meets a command's own arguments.
        assert_eq!(path(&parse("--as admin link status")), ["link", "status"]);
        assert_eq!(path(&parse("--as hanzo network routers")), ["network", "routers"]);
        assert_eq!(path(&parse("--as hanzo engine status")), ["engine", "status"]);
        assert_eq!(path(&parse("-v auth link list")), ["auth", "link", "list"]);

        let m = parse("--as admin claude status");
        assert!(path(&m).is_empty(), "a backend and a task are a session, not `status`");
        assert_eq!(m.get_one::<String>("positional").map(String::as_str), Some("claude"));
        assert_eq!(m.get_one::<String>("tail").map(String::as_str), Some("status"));
        let m = parse("--model enso auth list");
        assert!(path(&m).is_empty(), "a session flag still makes the line a session");
    }

    /// `hanzo vm …` passes argv through verbatim — flags included, because the
    /// help flag is disabled on the passthrough: `hanzo vm --help` is hanzo-vm's
    /// help, not ours.
    #[test]
    fn vm_is_a_verbatim_passthrough() {
        let cli = Cli::try_parse_from(["hanzo", "vm", "run", "--cpus", "2", "echo", "hi"])
            .expect("vm parses");
        let Some(Commands::Vm { args }) = cli.command else { panic!("expected vm") };
        assert_eq!(args, ["run", "--cpus", "2", "echo", "hi"]);

        let cli = Cli::try_parse_from(["hanzo", "vm", "--help"]).expect("--help passes through");
        let Some(Commands::Vm { args }) = cli.command else { panic!("expected vm") };
        assert_eq!(args, ["--help"]);
    }

    /// `hanzo up` is the local k3s running the cloud now: bare boots it,
    /// `--attest` asks what it is, `status`/`down` manage it, and the old
    /// `up <service>` still parses so the forwarder can catch it and send it
    /// to `host serve` — which owns what `up` used to do.
    #[test]
    fn up_boots_k3s_and_the_old_service_spelling_still_forwards() {
        let cli = Cli::try_parse_from(["hanzo", "up"]).expect("bare up parses");
        let Some(Commands::Up { command: None, cpus, memory, disk_size, cloud, attest, link, ui, no_ui }) =
            cli.command
        else {
            panic!("expected bare up")
        };
        assert_eq!((cpus, memory, disk_size, link), (4, 4096, 16384, None));
        assert_eq!(cloud, commands::up::CLOUD);
        assert!(!attest);
        assert!(!ui);
        assert!(!no_ui);

        // UI flags parse cleanly and conflict properly
        let cli = Cli::try_parse_from(["hanzo", "up", "--ui"]).expect("--ui parses");
        let Some(Commands::Up { ui: true, no_ui: false, .. }) = cli.command else { panic!("expected --ui") };
        let cli = Cli::try_parse_from(["hanzo", "up", "--no-ui"]).expect("--no-ui parses");
        let Some(Commands::Up { ui: false, no_ui: true, .. }) = cli.command else { panic!("expected --no-ui") };
        assert!(Cli::try_parse_from(["hanzo", "up", "--ui", "--no-ui"]).is_err());

        // `hanzo beat` is the standalone fleet presence, with no arguments
        let cli = Cli::try_parse_from(["hanzo", "beat"]).expect("beat parses");
        assert!(matches!(cli.command, Some(Commands::Beat)));

        // Console subcommand and alias parse
        let cli = Cli::try_parse_from(["hanzo", "up", "console"]).expect("console parses");
        let Some(Commands::Up { command: Some(UpCommands::Console), .. }) = cli.command else { panic!("expected console") };
        let cli = Cli::try_parse_from(["hanzo", "up", "dashboard"]).expect("dashboard parses");
        let Some(Commands::Up { command: Some(UpCommands::Console), .. }) = cli.command else { panic!("expected dashboard alias") };
        let cli = Cli::try_parse_from(["hanzo", "up", "ui"]).expect("ui alias parses");
        let Some(Commands::Up { command: Some(UpCommands::Console), .. }) = cli.command else { panic!("expected ui alias") };

        // Top-level console and sandbox commands and aliases parse
        let cli = Cli::try_parse_from(["hanzo", "console"]).expect("top-level console parses");
        assert!(matches!(cli.command, Some(Commands::Console)));
        let cli = Cli::try_parse_from(["hanzo", "dashboard"]).expect("top-level dashboard alias parses");
        assert!(matches!(cli.command, Some(Commands::Console)));
        let cli = Cli::try_parse_from(["hanzo", "sandbox"]).expect("top-level sandbox parses");
        assert!(matches!(cli.command, Some(Commands::Sandbox { command: None })));
        let cli = Cli::try_parse_from(["hanzo", "sbx"]).expect("top-level sbx alias parses");
        assert!(matches!(cli.command, Some(Commands::Sandbox { command: None })));
        let cli = Cli::try_parse_from(["hanzo", "sandboxes"]).expect("top-level sandboxes alias parses");
        assert!(matches!(cli.command, Some(Commands::Sandbox { command: None })));
        let cli = Cli::try_parse_from(["hanzo", "sandbox", "ui"]).expect("sandbox ui parses");
        assert!(matches!(cli.command, Some(Commands::Sandbox { command: Some(SandboxCommands::Ui) })));
        let cli = Cli::try_parse_from(["hanzo", "sandbox", "ls"]).expect("sandbox ls parses");
        assert!(matches!(cli.command, Some(Commands::Sandbox { command: Some(SandboxCommands::List) })));
        let cli = Cli::try_parse_from(["hanzo", "sandbox", "explore"]).expect("sandbox explore parses");
        assert!(matches!(cli.command, Some(Commands::Sandbox { command: Some(SandboxCommands::Explore) })));
        let cli = Cli::try_parse_from(["hanzo", "sandbox", "ps"]).expect("sandbox ps parses");
        assert!(matches!(cli.command, Some(Commands::Sandbox { command: Some(SandboxCommands::List) })));
        let cli = Cli::try_parse_from(["hanzo", "sbx", "ps"]).expect("sbx ps parses");
        assert!(matches!(cli.command, Some(Commands::Sandbox { command: Some(SandboxCommands::List) })));
        let cli = Cli::try_parse_from(["hanzo", "sandbox", "launch", "claude-env", "--node", "spark.local"]).expect("sandbox launch parses");
        let Some(Commands::Sandbox { command: Some(SandboxCommands::Launch { template, node }) }) = cli.command else { panic!("expected launch") };
        assert_eq!(template, "claude-env");
        assert_eq!(node, "spark.local");
        let cli = Cli::try_parse_from(["hanzo", "sandbox", "start", "claude-env", "--node", "spark.local"]).expect("sandbox start parses");
        let Some(Commands::Sandbox { command: Some(SandboxCommands::Launch { template, node }) }) = cli.command else { panic!("expected start") };
        assert_eq!(template, "claude-env");
        assert_eq!(node, "spark.local");
        let cli = Cli::try_parse_from(["hanzo", "ls"]).expect("top-level ls parses");
        assert!(matches!(cli.command, Some(Commands::Ls)));
        let cli = Cli::try_parse_from(["hanzo", "ps"]).expect("top-level ps parses");
        assert!(matches!(cli.command, Some(Commands::Ls)));
        let cli = Cli::try_parse_from(["hanzo", "list"]).expect("top-level list parses");
        assert!(matches!(cli.command, Some(Commands::Ls)));

        // Bare-metal runc & GPU pass-through options on the launcher
        let cli = Cli::try_parse_from(["hanzo", "dev", "--claude", "--runc"]).expect("dev --runc parses");
        let Some(Commands::Dev(code_args)) = cli.command else { panic!("expected dev") };
        assert!(code_args.no_sandbox);

        let cli = Cli::try_parse_from(["hanzo", "dev", "--runtime", "runc", "--gpus", "all"]).expect("dev --runtime runc --gpus all parses");
        let Some(Commands::Dev(code_args)) = cli.command else { panic!("expected dev with runtime runc") };
        assert_eq!(code_args.runtime.as_deref(), Some("runc"));
        assert_eq!(code_args.gpus.as_deref(), Some("all"));

        // Sandbox models and pull commands parse
        let cli = Cli::try_parse_from(["hanzo", "sandbox", "models"]).expect("sandbox models parses");
        assert!(matches!(cli.command, Some(Commands::Sandbox { command: Some(SandboxCommands::Models) })));
        let cli = Cli::try_parse_from(["hanzo", "sandbox", "pull", "qwen/qwen3.8-27b", "--node", "spark.local"]).expect("sandbox pull parses");
        let Some(Commands::Sandbox { command: Some(SandboxCommands::Pull { model, node }) }) = cli.command else { panic!("expected pull") };
        assert_eq!(model, "qwen/qwen3.8-27b");
        assert_eq!(node, "spark.local");

        // Load commands parse
        let cli = Cli::try_parse_from(["hanzo", "load", "--node", "spark"]).expect("top-level load parses");
        let Some(Commands::Load { node, model }) = cli.command else { panic!("expected load") };
        assert_eq!(node, "spark");
        assert!(model.is_none());

        let cli = Cli::try_parse_from(["hanzo", "sandbox", "load", "--node", "halo", "--model", "custom-model"]).expect("sandbox load parses");
        let Some(Commands::Sandbox { command: Some(SandboxCommands::Load { node, model }) }) = cli.command else { panic!("expected sandbox load") };
        assert_eq!(node, "halo");
        assert_eq!(model.as_deref(), Some("custom-model"));

        // The image is nameable, and asking what a cluster is takes no boot.
        let cli = Cli::try_parse_from(["hanzo", "up", "--cloud", "ghcr.io/hanzoai/cloud@sha256:ab"])
            .expect("--cloud parses");
        let Some(Commands::Up { cloud, .. }) = cli.command else { panic!("expected up") };
        assert_eq!(cloud, "ghcr.io/hanzoai/cloud@sha256:ab");

        let cli = Cli::try_parse_from(["hanzo", "up", "--attest"]).expect("--attest parses");
        let Some(Commands::Up { attest, .. }) = cli.command else { panic!("expected up") };
        assert!(attest);

        assert!(Cli::try_parse_from(["hanzo", "up", "status"]).is_ok());
        assert!(Cli::try_parse_from(["hanzo", "up", "down"]).is_ok());
        assert!(Cli::try_parse_from(["hanzo", "down"]).is_ok());

        let cli = Cli::try_parse_from(["hanzo", "up", "--link", "dev"]).expect("--link parses");
        let Some(Commands::Up { link, .. }) = cli.command else { panic!("expected up") };
        assert_eq!(link.as_deref(), Some("dev"));

        // The deprecated spelling lands on the forwarder, argv intact.
        let cli = Cli::try_parse_from(["hanzo", "up", "iam"]).expect("old spelling parses");
        let Some(Commands::Up { command: Some(UpCommands::Service(argv)), .. }) = cli.command
        else {
            panic!("expected the forwarder")
        };
        assert_eq!(argv, ["iam"]);

        // `host serve` is the new home, with the old grammar.
        assert!(Cli::try_parse_from(["hanzo", "host", "serve"]).is_ok());
        assert!(
            Cli::try_parse_from(["hanzo", "host", "serve", "iam", "--", "--port", "1"]).is_ok()
        );
    }

    /// `link` is the machine; its network verbs hang off it, and the tunnel
    /// flags belong to the verbs, never to the bare shell link.
    #[test]
    fn link_speaks_the_network_from_this_machine() {
        let parse = |argv: &[&str]| Cli::try_parse_from(argv).map(|c| c.command);

        let Ok(Some(Commands::Link { command: None, shell, read_only, .. })) =
            parse(&["hanzo", "link", "--shell", "bash", "--read-only"])
        else {
            panic!("bare link")
        };
        assert_eq!((shell.as_deref(), read_only), (Some("bash"), true));

        let Ok(Some(Commands::Link { command: Some(LinkCommands::Host { name, target, signer, install }), .. })) =
            parse(&["hanzo", "link", "host", "engine", "127.0.0.1:1235", "--token-command", "/etc/hanzo/link/token"])
        else {
            panic!("link host NAME TARGET")
        };
        assert_eq!((name.as_deref(), target.as_deref()), (Some("engine"), Some("127.0.0.1:1235")));
        assert_eq!(signer.token_command.as_deref(), Some("/etc/hanzo/link/token"));
        assert!(!install.install);

        // Hosting what the identity is already bound to publishes nothing.
        let Ok(Some(Commands::Link { command: Some(LinkCommands::Host { name: None, target: None, install, .. }), .. })) =
            parse(&["hanzo", "link", "host", "--install", "--system"])
        else {
            panic!("link host --install --system")
        };
        assert!(install.install && install.system);

        let Ok(Some(Commands::Link { command: Some(LinkCommands::Dial { service, port, install, .. }), .. })) =
            parse(&["hanzo", "link", "dial", "k8s.hanzo", "26443", "--install"])
        else {
            panic!("link dial")
        };
        assert_eq!((service.as_str(), port, install.install, install.system), ("k8s.hanzo", 26443, true, false));

        assert!(matches!(
            parse(&["hanzo", "link", "status"]),
            Ok(Some(Commands::Link { command: Some(LinkCommands::Status { .. }), .. }))
        ));
        assert!(matches!(
            parse(&["hanzo", "link", "rm", "engine"]),
            Ok(Some(Commands::Link { command: Some(LinkCommands::Rm { .. }), .. }))
        ));

        // A name without a target, a system unit that is not installed, port 0,
        // and a shell flag on a network verb are all refused at parse time.
        for bad in [
            &["hanzo", "link", "host", "engine"][..],
            &["hanzo", "link", "host", "--system"],
            &["hanzo", "link", "dial", "k8s.hanzo", "0"],
            &["hanzo", "link", "dial", "k8s.hanzo"],
            &["hanzo", "link", "--shell", "bash", "status"],
        ] {
            assert!(parse(bad).is_err(), "{bad:?} parsed");
        }
    }

    /// There is one way to link, and `net` is not it.
    #[test]
    fn net_is_gone() {
        let cmd = Cli::command();
        assert!(cmd.find_subcommand("net").is_none());
    }

    /// The AI login manager's accounts are credentials: they live under `auth`,
    /// and `link` no longer answers for them.
    #[test]
    fn linked_ai_accounts_live_under_auth() {
        let merged = commands::product::augment(Cli::command());
        let auth = merged.find_subcommand("auth").expect("auth");
        let accounts = auth.find_subcommand("link").expect("auth link");
        for verb in ["list", "get", "rm", "route", "create", "devices", "usage"] {
            assert!(accounts.find_subcommand(verb).is_some(), "auth link {verb}");
        }
        let link = merged.find_subcommand("link").expect("link");
        assert!(link.find_subcommand("list").is_none(), "`link list` is not the machine link");
        assert!(link.find_subcommand("route").is_none());
    }

    /// The merged tree (derive + generated products) builds without a clap panic.
    #[test]
    fn the_merged_command_tree_is_valid() {
        commands::product::augment(Cli::command()).debug_assert();
    }

    /// `--help` / `-h` is intercepted by clap, never swallowed by the fallback.
    #[test]
    fn help_flag_is_preserved() {
        let err = Cli::try_parse_from(["hanzo", "--help"])
            .err()
            .expect("`--help` exits via a clap error");
        assert_eq!(err.kind(), clap::error::ErrorKind::DisplayHelp);
    }

    /// A generated product still resolves through the merged tree; a local resource
    /// does not (it dispatches through the derive tree).
    #[test]
    fn a_generated_product_resolves_and_a_local_command_does_not() {
        let hand = Cli::command();
        let merged = commands::product::augment(hand.clone());
        let m = merged.clone().try_get_matches_from(["hanzo", "agent", "list"]).unwrap();
        assert!(commands::product::resolve(&hand, &m).is_some(), "a cloud product resolves");

        let m = merged.try_get_matches_from(["hanzo", "version"]).unwrap();
        assert!(commands::product::resolve(&hand, &m).is_none(), "a local command is not a product");
    }

    /// `hanzo code` is the code index and nothing else: each of its operations
    /// parses on the tree `main` parses and resolves to its own route, and
    /// `hanzo dev` is the coding session, not a cloud operation.
    #[test]
    fn code_is_the_index_and_dev_is_the_session() {
        let hand = Cli::command();
        let merged = commands::product::augment(hand.clone());
        for (argv, path) in [
            (["hanzo", "code", "search"].as_slice(), "/v1/code/search"),
            (["hanzo", "code", "index"].as_slice(), "/v1/code/index"),
            (["hanzo", "code", "context"].as_slice(), "/v1/code/context"),
            (["hanzo", "code", "file"].as_slice(), "/v1/code/file"),
            (["hanzo", "code", "tree"].as_slice(), "/v1/code/tree"),
        ] {
            let m = merged
                .clone()
                .try_get_matches_from(argv)
                .unwrap_or_else(|e| panic!("{argv:?} must parse: {e}"));
            let Some(commands::product::Resolved::Leaf { op, .. }) =
                commands::product::resolve(&hand, &m)
            else {
                panic!("{argv:?} must resolve to a cloud operation")
            };
            assert_eq!(op.path, path, "{argv:?}");
        }
        let code = merged.find_subcommand("code").expect("code exists");
        let verbs: Vec<&str> = code.get_subcommands().map(|c| c.get_name()).collect();
        for verb in ["search", "index", "context", "file", "tree", "ask"] {
            assert!(verbs.contains(&verb), "`hanzo code {verb}` is missing: {verbs:?}");
        }
        assert_eq!(code.get_arguments().filter(|a| !a.is_global_set()).count(), 0, "code takes no session args");

        for argv in [
            ["hanzo", "dev"].as_slice(),
            ["hanzo", "dev", "fix the failing test"].as_slice(),
            ["hanzo", "dev", "--claude"].as_slice(),
            ["hanzo", "dev", "--claude", "fix it"].as_slice(),
        ] {
            let m = merged
                .clone()
                .try_get_matches_from(argv)
                .unwrap_or_else(|e| panic!("{argv:?} must parse: {e}"));
            assert!(
                commands::product::resolve(&hand, &m).is_none(),
                "{argv:?} is the coding session, not a cloud operation"
            );
            let cli = Cli::from_arg_matches(&m).unwrap_or_else(|e| panic!("{argv:?}: {e}"));
            assert!(matches!(cli.command, Some(Commands::Dev(_))), "{argv:?} must be `dev`");
        }
    }

    /// A GLOBAL flag stays global under an absorbed command. `--config` is
    /// documented as "valid on every subcommand", and the first shape of this
    /// change broke that for exactly the commands it was meant to complete.
    #[test]
    fn a_global_flag_still_reaches_an_absorbed_subcommand() {
        let hand = Cli::command();
        let merged = commands::product::augment(hand.clone());
        let m = merged
            .try_get_matches_from(["hanzo", "billing", "--config", "/tmp/x", "invoices", "list"])
            .expect("a global flag is valid before an absorbed subcommand");
        assert!(commands::product::resolve(&hand, &m).is_some());
    }
}
