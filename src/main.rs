use anyhow::Result;
use clap::{CommandFactory, FromArgMatches, Parser, Subcommand};
use colored::*;
use std::path::PathBuf;

mod commands;
mod config;
mod http;
mod private;
mod iam;
mod telemetry;
// ZAP reaches the local host over a UNIX socket. Windows has none, so there the
// local host is reached over the HTTP view it also serves (see commands::host).
#[cfg(unix)]
mod zap;

/// Tell the user something went sideways WITHOUT failing the run — one place, so
/// every warning the CLI prints looks the same and none of them lands on stdout,
/// where it would corrupt what a caller is piping (`hanzo auth token`).
pub fn warn(msg: &str) {
    eprintln!("{} {}", "warning:".yellow().bold(), msg);
}

#[derive(Parser)]
#[command(name = "hanzo")]
#[command(author = "Hanzo AI")]
#[command(version)]  // = CARGO_PKG_VERSION; Cargo.toml is the ONE source
#[command(about = "Unified CLI for Hanzo AI development tools", long_about = None)]
// Bare `hanzo` IS a coding session, WITH flags: the code args are flattened at the
// top level, so `hanzo --resume <id>`, `hanzo --model enso`, and `hanzo "fix the
// bug"` all route to a coding session (the same run `hanzo agent run` starts).
// `args_conflicts_with_subcommands` keeps them mutually exclusive with an explicit
// subcommand (`hanzo agent …`, `hanzo auth …`), and `subcommand_negates_reqs` lets
// a subcommand run without them — so the flattened args apply ONLY to a bare `hanzo`.
#[command(args_conflicts_with_subcommands = true, subcommand_negates_reqs = true)]
struct Cli {
    /// Sets a custom config file
    ///
    /// GLOBAL: valid on every subcommand (`hanzo cluster list --config F`).
    #[arg(short, long, value_name = "FILE", global = true)]
    config: Option<PathBuf>,

    /// Increase logging verbosity
    #[arg(short, long, action = clap::ArgAction::Count, global = true)]
    verbose: u8,

    /// The coding-session args, flattened so a bare `hanzo [flags] [task]` is a
    /// coding session with them. Ignored when an explicit subcommand is given.
    #[command(flatten)]
    code: CodeArgs,

    /// Optional: a truly-bare `hanzo` (no subcommand) launches a cloud-linked
    /// coding session from the flattened `code` args above. `--help`/`-h` and every
    /// explicit subcommand are handled by clap before that fallback ever applies.
    #[command(subcommand)]
    command: Option<Commands>,
}

/// The coding-session arguments — shared between `hanzo agent run` and a bare
/// `hanzo …` (flattened onto [`Cli`]), so both accept exactly the same flags.
#[derive(clap::Args, Clone)]
struct CodeArgs {
    /// Coding backend: claude | dev
    #[arg(long, default_value = "claude")]
    backend: String,

    /// Force streaming this session to Hanzo cloud (mission-control) on. Already
    /// the default for a signed-in run; `--link` only overrides a persisted
    /// `code.link = false`.
    #[arg(long)]
    link: bool,

    /// Never stream to cloud, even when signed in or `code.link = true`.
    #[arg(long)]
    no_link: bool,

    /// Do not route model calls through api.hanzo.ai (use the backend's own model
    /// account instead of the metered Hanzo gateway).
    #[arg(long)]
    no_route: bool,

    /// Do not attach the Hanzo MCP toolset.
    #[arg(long)]
    no_mcp: bool,

    /// Also load the repository's own `.mcp.json` MCP servers. Off by default: a
    /// repo is untrusted and any server it declares would run with your session's
    /// model key — only pass this for repos you trust.
    #[arg(long)]
    project_mcp: bool,

    /// Ask before each action instead of auto-approving it (`--safe` is an alias).
    /// Mutually exclusive with `--no-sandbox`.
    #[arg(long, visible_alias = "safe", conflicts_with = "no_sandbox")]
    ask: bool,

    /// Escalate PAST auto-approve to a full bypass that also drops the sandbox. A
    /// deliberate, per-invocation act — never a persisted default.
    #[arg(long)]
    no_sandbox: bool,

    /// Resume a prior linked session by its cloud session id.
    #[arg(long, value_name = "SESSION_ID")]
    resume: Option<String>,

    /// Brand / tenant for auth: hanzo | lux | zoo | pars | bootnode
    #[arg(long, default_value_t = iam::paths::DEFAULT_BRAND.to_string())]
    brand: String,

    /// Claude theme to apply (Claude backend only), e.g. `dracula`. `--theme none`
    /// skips theming.
    #[arg(long)]
    theme: Option<String>,

    /// The gateway model to use, e.g. `enso`, `enso-ultra`, `zen5-coder`. Applies
    /// on the metered Hanzo gateway route only; a direct provider key names its own
    /// model. No client-side allowlist — the gateway validates the id.
    #[arg(long, value_name = "MODEL")]
    model: Option<String>,

    /// Task to run headless. If omitted, launches an interactive session.
    task: Option<String>,

    /// Extra args passed verbatim to the backend (after `--`).
    #[arg(last = true, allow_hyphen_values = true)]
    passthrough: Vec<String>,
}

impl CodeArgs {
    /// Map the parsed args to the code runner's [`Options`]. The `no_*` flags become
    /// their positive sense here, in exactly ONE place.
    fn into_options(self) -> commands::code::Options {
        commands::code::Options {
            backend: self.backend,
            link: self.link,
            no_link: self.no_link,
            route: !self.no_route,
            mcp: !self.no_mcp,
            project_mcp: self.project_mcp,
            ask: self.ask,
            no_sandbox: self.no_sandbox,
            resume: self.resume,
            brand: self.brand,
            theme: self.theme,
            model: self.model,
            task: self.task,
            passthrough: self.passthrough,
        }
    }
}

/// `hanzo <resource> <command>` — the resource-noun tree. Every cloud capability
/// beyond these hand-written resources is a generated product subcommand
/// (`commands::product`), merged in at runtime.
#[derive(Subcommand)]
enum Commands {
    /// Run managed AI tasks
    Agent {
        #[command(subcommand)]
        command: AgentCommands,
    },

    /// Manage identities and credentials
    Auth {
        #[command(subcommand)]
        command: AuthCommands,
    },

    /// Put this shell on the fabric so it can be driven from the console
    Link {
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

    /// Run a Hanzo service: `cloud` for the whole API, or one service
    /// (iam | kms | gateway | storage | pubsub)
    Serve {
        /// `cloud`, or a single service name (iam | kms | gateway | storage | pubsub)
        service: String,
        /// Extra args passed verbatim to the service (after `--`)
        #[arg(last = true, allow_hyphen_values = true)]
        passthrough: Vec<String>,
    },

    /// Print the CLI version
    Version,

    // ── kept resources (additive) ────────────────────────────────────────────
    /// Run / join hanzo.network (the L1 fabric) with hanzod, and query its cluster
    Fabric {
        #[command(subcommand)]
        command: FabricCommands,
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

    /// Prepaid wallet money — read the balance, mint a deposit
    Billing {
        #[command(subcommand)]
        command: BillingCommands,
    },

    /// Connect an external provider account (Cloudflare, …) to your org
    Connector {
        #[command(subcommand)]
        command: ConnectorCommands,
    },

    /// Publish a local service to a public https://<token>.share.hanzo.ai URL
    Share {
        /// Local target: a port (3000), host:port, or a full url
        target: String,
        /// Backend mode: proxy | web | caddy | drive
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
enum AgentCommands {
    /// Run a managed AI task. `--mode code` (default) is a managed coding
    /// workspace; `--mode desktop` is browser/desktop control (à la hanzo.bot).
    Run {
        /// What the agent is pointed at: code | desktop
        #[arg(long, default_value = "code",
              value_parser = clap::builder::PossibleValuesParser::new(["code", "desktop"]))]
        mode: String,
        /// The coding-session flags (`--model`, `--backend`, `--resume`, `[task]`, …)
        #[command(flatten)]
        code: CodeArgs,
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
}

#[derive(Subcommand)]
enum EngineCommands {
    /// Serve a model on an OpenAI-compatible local endpoint (via the Hanzo engine)
    Serve {
        model: String,
        /// Extra engine args passed verbatim (after `--`), e.g. `--port 8080`
        #[arg(last = true, allow_hyphen_values = true)]
        passthrough: Vec<String>,
    },
}

#[derive(Subcommand)]
enum RunnerCommands {
    /// Register + run this machine as a CI runner
    Start,
    /// Stop the runner on this machine
    Stop,
    /// Report the runner's state
    Status,
}

#[derive(Subcommand)]
enum FabricCommands {
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
    /// Query the model cluster a running node serves
    Cluster {
        /// Node API base URL (defaults to the active network's api endpoint)
        #[arg(long, env = "HANZO_NODE_URL")]
        node: Option<String>,
        #[command(subcommand)]
        command: FabricClusterCommands,
    },
}

#[derive(Subcommand)]
enum FabricClusterCommands {
    /// Show cluster topology (this node + discovered peers)
    Topology,
    /// List all models available across the cluster
    Models,
    /// Show which node would serve a given model
    Route { model: String },
    /// Show where to load a model that isn't served yet
    Placement { model: String },
    /// Route a chat prompt to whichever node serves the model
    Chat {
        model: String,
        message: String,
        #[arg(long, default_value = "256")]
        max_tokens: u32,
    },
    /// Federated RAG search across the cluster
    Search {
        query: String,
        #[arg(long, default_value = "10")]
        max_results: u32,
    },
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

#[derive(Subcommand)]
enum BillingCommands {
    /// Show the active identity's prepaid balance
    Balance,
    /// Credit an account (SuperAdmin / internal service only — the server rules)
    Deposit {
        #[arg(long)]
        user: String,
        #[arg(long)]
        cents: i64,
        #[arg(long)]
        currency: Option<String>,
        #[arg(long)]
        notes: Option<String>,
        #[arg(long)]
        tags: Option<String>,
        #[arg(long)]
        expires_in: Option<u32>,
    },
}

#[derive(Subcommand)]
enum ConnectorCommands {
    /// Connect a provider: verify a scoped credential and seal it into KMS
    Add {
        #[arg(long)]
        provider: String,
        #[arg(long)]
        account_id: Option<String>,
        /// `-` reads the token from stdin (a literal is REFUSED)
        #[arg(long)]
        token: Option<String>,
    },
    /// List your org's connectors and their status (never the credential)
    List,
    /// Re-verify a connected credential against the provider, live
    Verify {
        #[arg(long)]
        provider: String,
    },
    /// Disconnect a provider: delete its KMS credential and forget it
    Rm {
        #[arg(long)]
        provider: String,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    // A truly bare `hanzo`, `hanzo --help` or `hanzo help` prints the root man
    // page — NAME/SYNOPSIS/GROUPS/COMMANDS, one line per product, like a proper
    // cloud CLI. Everything else (including `hanzo <group> --help` and the
    // `hanzo "task"` coding session) parses normally; `-h` keeps clap's terse
    // summary as the short form.
    {
        let argv: Vec<String> = std::env::args().skip(1).collect();
        if argv.is_empty() || argv == ["--help"] || argv == ["help"] {
            print!("{}", commands::man::page());
            return Ok(());
        }
    }
    // ONE tree: the derive command, augmented with the generated first-class
    // product commands. One parse, one dispatch — a matched cloud product goes
    // through the product seam, everything else is a derive command (or bare).
    let matches = commands::product::augment(Cli::command()).get_matches();

    let log_level = match matches.get_count("verbose") {
        0 => "warn",
        1 => "info",
        2 => "debug",
        _ => "trace",
    };
    tracing_subscriber::fmt().with_env_filter(log_level).init();

    let mut config = config::Config::load(matches.get_one::<PathBuf>("config").cloned())?;
    let telemetry = telemetry::build(&config);

    // A matched generated product dispatches first, through the shared seam.
    if let Some(resolved) = commands::product::resolve(&matches) {
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
            // A truly-bare `hanzo` (no subcommand): greet a fresh machine, then run
            // a cloud-linked coding session from the flattened top-level code args
            // (link forced on, exactly as `hanzo agent run` does when signed in).
            iam::onboarding::first_run(&mut config, iam::paths::DEFAULT_BRAND).await;
            let mut code = cli.code;
            code.link = true;
            let started = std::time::Instant::now();
            let outcome = commands::code::run(&mut config, code.into_options()).await;
            telemetry.command("code", started.elapsed(), outcome.is_ok());
            telemetry.flush().await;
            outcome
        }
    }
}

/// Run one resolved top-level command.
async fn dispatch(command: Commands, mut config: config::Config) -> Result<()> {
    match command {
        Commands::Agent { command } => match command {
            AgentCommands::Run { mode, code } => {
                commands::agent::run(
                    &mut config,
                    code.into_options(),
                    commands::agent::Mode::parse(&mode),
                )
                .await?
            }
        },
        Commands::Auth { command } => match command {
            AuthCommands::Login { brand, provider, token } => {
                commands::auth::login(&mut config, &brand, provider, token).await?
            }
            AuthCommands::Logout { identity, brand, all } => {
                commands::auth::logout(&mut config, &brand, identity, all)?
            }
            AuthCommands::Show { brand } => commands::auth::show(&mut config, &brand).await?,
            AuthCommands::List { brand } => commands::auth::list(&mut config, &brand).await?,
            AuthCommands::Use { identity, brand } => {
                commands::auth::use_identity(&mut config, &brand, identity)?
            }
            AuthCommands::Token { brand } => commands::auth::token(&mut config, &brand).await?,
        },
        Commands::Engine { command } => match command {
            EngineCommands::Serve { model, passthrough } => {
                commands::engine::serve(model, passthrough).await?
            }
        },
        Commands::Scan { path } => commands::scan::scan(path).await?,
        Commands::Link {
            shell,
            read_only,
            title,
        } => commands::link::run(&mut config, shell, read_only, title).await?,

        Commands::Config { command } => match command {
            ConfigCommands::List => commands::config::list(&config)?,
            ConfigCommands::Get { key } => commands::config::get(&config, &key)?,
            ConfigCommands::Set { key, value } => commands::config::set(&mut config, &key, &value)?,
        },
        Commands::Runner { command } => match command {
            RunnerCommands::Start => commands::runner::start().await?,
            RunnerCommands::Stop => commands::runner::stop().await?,
            RunnerCommands::Status => commands::runner::status().await?,
        },
        Commands::Serve { service, passthrough } => {
            if service == "cloud" {
                commands::serve::cloud(passthrough).await?
            } else {
                commands::serve::service(service, passthrough).await?
            }
        }
        Commands::Version => {
            println!("{} v{}", "Hanzo CLI".bold(), env!("CARGO_PKG_VERSION"));
        }
        Commands::Fabric { command } => match command {
            FabricCommands::Up { foreground, with_cloud } => {
                commands::fabric::up(&config, foreground, with_cloud).await?
            }
            FabricCommands::Status => commands::fabric::status(&config).await?,
            FabricCommands::Join { network, foreground, with_cloud } => {
                commands::fabric::join(&mut config, network, foreground, with_cloud).await?
            }
            FabricCommands::Stop => commands::fabric::stop(&config)?,
            FabricCommands::Cluster { node, command } => {
                let node = node.unwrap_or_else(|| commands::network::active(&config).api);
                match command {
                    FabricClusterCommands::Topology => {
                        commands::fabric::cluster::topology(node).await?
                    }
                    FabricClusterCommands::Models => commands::fabric::cluster::models(node).await?,
                    FabricClusterCommands::Route { model } => {
                        commands::fabric::cluster::route(node, model).await?
                    }
                    FabricClusterCommands::Placement { model } => {
                        commands::fabric::cluster::placement(node, model).await?
                    }
                    FabricClusterCommands::Chat { model, message, max_tokens } => {
                        commands::fabric::cluster::chat(node, model, message, max_tokens).await?
                    }
                    FabricClusterCommands::Search { query, max_results } => {
                        commands::fabric::cluster::search(node, query, max_results).await?
                    }
                }
            }
        },
        Commands::Host { command } => match command {
            HostCommands::Start => commands::host::start(&config).await?,
            HostCommands::Status => commands::host::status(&config).await?,
            HostCommands::Stop => commands::host::stop(&config).await?,
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
        Commands::Billing { command } => match command {
            BillingCommands::Balance => commands::billing::balance(&mut config).await?,
            BillingCommands::Deposit { user, cents, currency, notes, tags, expires_in } => {
                commands::billing::deposit(
                    &mut config,
                    commands::billing::Deposit { user, cents, currency, notes, tags, expires_in },
                )
                .await?
            }
        },
        Commands::Connector { command } => match command {
            ConnectorCommands::Add { provider, account_id, token } => {
                commands::connector::add(&mut config, provider, account_id, token).await?
            }
            ConnectorCommands::List => commands::connector::list(&mut config).await?,
            ConnectorCommands::Verify { provider } => {
                commands::connector::verify(&mut config, provider).await?
            }
            ConnectorCommands::Rm { provider } => {
                commands::connector::rm(&mut config, provider).await?
            }
        },
        Commands::Share { target, backend_mode, name } => {
            commands::share::run(&mut config, target, backend_mode, name).await?
        }
        Commands::Init { template, name } => commands::init::run(template, name).await?,
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

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
        assert_eq!(cli.code.task.as_deref(), Some("fix the bug"));

        // `--safe` opts out of auto-approve; `--no-sandbox` escalates; they conflict.
        let cli = Cli::try_parse_from(["hanzo", "--safe"]).expect("parses");
        assert!(cli.code.ask);
        assert!(Cli::try_parse_from(["hanzo", "--ask", "--no-sandbox"]).is_err());
    }

    /// `hanzo agent run` is the ONE way to run an agent, with `--mode code|desktop`
    /// and the full coding-session flags.
    #[test]
    fn agent_run_is_the_one_agent_verb() {
        let cli = Cli::try_parse_from(["hanzo", "agent", "run", "--mode", "desktop", "browse docs"])
            .expect("`agent run --mode desktop` parses");
        let Some(Commands::Agent { command: AgentCommands::Run { mode, code } }) = cli.command else {
            panic!("expected agent run");
        };
        assert_eq!(mode, "desktop");
        assert_eq!(code.task.as_deref(), Some("browse docs"));

        // Default mode is code, and the code flags flatten in.
        let cli = Cli::try_parse_from(["hanzo", "agent", "run", "--model", "enso", "fix it"]).unwrap();
        let Some(Commands::Agent { command: AgentCommands::Run { mode, code } }) = cli.command else {
            panic!("expected agent run");
        };
        assert_eq!(mode, "code");
        assert_eq!(code.model.as_deref(), Some("enso"));

        // An unknown mode is rejected by clap.
        assert!(Cli::try_parse_from(["hanzo", "agent", "run", "--mode", "wat"]).is_err());
    }

    /// The old top-level verbs are GONE from the derive tree — relocated under
    /// their resource nouns. (`kms` is NOT here: it is a generated cloud product,
    /// not a removed local verb, so it stays reachable.)
    #[test]
    fn old_top_level_verbs_are_removed() {
        let names: Vec<String> =
            Cli::command().get_subcommands().map(|s| s.get_name().to_string()).collect();
        for gone in ["login", "logout", "whoami", "switch", "code", "deploy", "build"] {
            assert!(
                !names.iter().any(|n| n == gone),
                "`{gone}` must no longer be a top-level subcommand"
            );
        }
        // `cluster`/`model`/`node`/`secret`/`usage`/`dev`/`docs`/`mdx`/`ui`/`mcp`
        // are DELETED hand commands: proxies, npx passthroughs, and shadows of
        // generated cloud products (the org-review verdicts). `engine` and
        // `scan` are their surviving local halves.
        for gone in ["cluster", "model", "node", "secret", "usage", "dev", "docs", "mdx", "ui"] {
            assert!(
                !names.iter().any(|n| n == gone),
                "`{gone}` is a deleted hand command and must not be a top-level"
            );
        }
        for present in ["agent", "auth", "config", "engine", "runner", "scan", "serve"] {
            assert!(names.iter().any(|n| n == present), "`{present}` must be a resource noun");
        }
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
        assert!(Cli::try_parse_from(["hanzo", "serve", "cloud"]).is_ok());
        assert!(Cli::try_parse_from(["hanzo", "serve", "iam"]).is_ok());
        assert!(matches!(
            Cli::try_parse_from(["hanzo", "version"]).unwrap().command,
            Some(Commands::Version)
        ));
    }

    /// The hanzod fabric moved to `fabric` (its own home), keeping the node-talk
    /// cluster verbs under `fabric cluster` — reachable, not lost.
    #[test]
    fn fabric_keeps_the_hanzod_node_and_its_cluster() {
        assert!(Cli::try_parse_from(["hanzo", "fabric", "up"]).is_ok());
        assert!(Cli::try_parse_from(["hanzo", "fabric", "status"]).is_ok());
        assert!(Cli::try_parse_from(["hanzo", "fabric", "cluster", "topology"]).is_ok());
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
        let merged = commands::product::augment(Cli::command());
        let m = merged.clone().try_get_matches_from(["hanzo", "agents", "list"]).unwrap();
        assert!(commands::product::resolve(&m).is_some(), "a cloud product resolves");

        let m = merged.try_get_matches_from(["hanzo", "version"]).unwrap();
        assert!(commands::product::resolve(&m).is_none(), "a local command is not a product");
    }
}
