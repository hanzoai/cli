use anyhow::Result;
use clap::{CommandFactory, FromArgMatches, Parser, Subcommand};
use colored::*;
use std::path::PathBuf;

mod commands;
mod config;
mod http;
mod private;
mod iam;
mod sdk;

#[derive(Parser)]
#[command(name = "hanzo")]
#[command(author = "Hanzo AI")]
#[command(version)]  // = CARGO_PKG_VERSION; Cargo.toml is the ONE source
#[command(about = "Unified CLI for Hanzo AI development tools", long_about = None)]
struct Cli {
    /// Sets a custom config file
    #[arg(short, long, value_name = "FILE")]
    config: Option<PathBuf>,

    /// Increase logging verbosity
    #[arg(short, long, action = clap::ArgAction::Count)]
    verbose: u8,

    /// Resume a prior linked coding session by its cloud session id. Sugar for
    /// `hanzo code --resume <id>` on a bare `hanzo` (the form printed after every
    /// run). NOT global: an explicit subcommand carries its own `--resume`, so
    /// this applies only to the bare invocation.
    #[arg(long, value_name = "SESSION_ID")]
    resume: Option<String>,

    /// Optional: a truly-bare `hanzo` (no subcommand) launches a cloud-linked
    /// coding session — see `bare`. `--help`/`-h` and every explicit subcommand
    /// are handled by clap before that fallback ever applies.
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    /// Initialize a new Hanzo project
    Init {
        /// Project template
        #[arg(short, long, default_value = "default")]
        template: String,

        /// Project name
        name: Option<String>,
    },

    /// Start development server
    Dev {
        /// Port to use
        #[arg(short, long, default_value = "3000")]
        port: u16,

        /// Enable hot reload
        #[arg(long)]
        hot: bool,
    },

    /// AI agent operations (Python SDK)
    Agent {
        #[command(subcommand)]
        command: AgentCommands,
    },

    /// Session-aware coding: wrap Claude Code or `dev`, attach Hanzo MCP + auth,
    /// route usage through api.hanzo.ai, and stream the session live to Hanzo
    /// cloud (on by default when signed in; `--no-link` opts out). A trailing
    /// `[task]` runs headless; omit it for interactive.
    Code {
        /// Coding backend: claude | dev
        #[arg(long, default_value = "claude")]
        backend: String,

        /// Force streaming this session to Hanzo cloud (mission-control) on. This
        /// is already the default for a signed-in run; `--link` only overrides a
        /// persisted `code.link = false`.
        #[arg(long)]
        link: bool,

        /// Never stream to cloud, even when signed in or `code.link = true`.
        #[arg(long)]
        no_link: bool,

        /// Do not route model calls through api.hanzo.ai (use the backend's own
        /// model account instead of the metered Hanzo gateway).
        #[arg(long)]
        no_route: bool,

        /// Do not attach the Hanzo MCP toolset.
        #[arg(long)]
        no_mcp: bool,

        /// Also load the repository's own `.mcp.json` MCP servers. Off by
        /// default: a repo is untrusted and any server it declares would run
        /// with your session's model key — only pass this for repos you trust.
        #[arg(long)]
        project_mcp: bool,

        /// Resume a prior linked session by its cloud session id.
        #[arg(long, value_name = "SESSION_ID")]
        resume: Option<String>,

        /// Brand / tenant for auth: hanzo | lux | zoo | pars | bootnode
        #[arg(long, default_value_t = iam::paths::DEFAULT_BRAND.to_string())]
        brand: String,

        /// Claude theme to apply (Claude backend only), e.g. `dracula`. Defaults
        /// to the persisted `code.theme` (dracula). `--theme none` skips theming.
        #[arg(long)]
        theme: Option<String>,

        /// Task to run headless. If omitted, launches an interactive session.
        task: Option<String>,

        /// Extra args passed verbatim to the backend (after `--`). Use this to
        /// set the backend's own sandbox/permission flags — never widened by us.
        #[arg(last = true, allow_hyphen_values = true)]
        passthrough: Vec<String>,
    },

    /// Sign in: Hanzo (OIDC), or a provider key (OpenAI / Anthropic). Bare
    /// `hanzo login` on a terminal shows an interactive picker.
    Login {
        /// Brand / tenant: hanzo | lux | zoo | pars | bootnode
        #[arg(long, default_value_t = iam::paths::DEFAULT_BRAND.to_string())]
        brand: String,

        /// Sign in non-interactively with a specific provider: hanzo | openai |
        /// anthropic. `hanzo` runs the OIDC flow (or stores a token with
        /// `--token -`); `openai`/`anthropic` store an API key read from stdin.
        #[arg(long, value_name = "PROVIDER")]
        provider: Option<String>,

        /// Supply the credential on stdin instead of interactively: `--token -`
        /// reads a hanzo.id bearer (default provider) or a provider API key
        /// (`--provider openai|anthropic`), so it never lands in argv or history.
        /// A literal value is refused — for a bearer or a key alike.
        #[arg(long, value_name = "TOKEN")]
        token: Option<String>,
    },

    /// Show the active identity (`--all` lists every identity)
    Whoami {
        /// Brand / tenant: hanzo | lux | zoo | pars | bootnode
        #[arg(long, default_value_t = iam::paths::DEFAULT_BRAND.to_string())]
        brand: String,

        /// List every identity on this brand, marking the active one
        #[arg(long)]
        all: bool,
    },

    /// Stacked, per-account balances: every identity (and provider key) you
    /// hold, each showing ITS OWN remaining balance / usage-left, read client-
    /// side with that account's own token. Disjoint — one account failing never
    /// blanks the rest — and never an aggregate total.
    Usage {
        /// Brand / tenant: hanzo | lux | zoo | pars | bootnode
        #[arg(long, default_value_t = iam::paths::DEFAULT_BRAND.to_string())]
        brand: String,
    },

    /// Switch the active identity (like `gh auth switch`)
    Switch {
        /// `owner/name` (e.g. admin/z), or a bare `owner` when unambiguous.
        /// Omit to toggle when exactly two identities are signed in.
        #[arg(value_name = "IDENTITY")]
        identity: Option<String>,

        /// Brand / tenant: hanzo | lux | zoo | pars | bootnode
        #[arg(long, default_value_t = iam::paths::DEFAULT_BRAND.to_string())]
        brand: String,
    },

    /// Sign out one identity (or `--all` of them) and remove the credential
    Logout {
        /// `owner/name`, or a bare `owner` when unambiguous. Omit to sign out
        /// of the ACTIVE identity.
        #[arg(value_name = "IDENTITY")]
        identity: Option<String>,

        /// Brand / tenant: hanzo | lux | zoo | pars | bootnode
        #[arg(long, default_value_t = iam::paths::DEFAULT_BRAND.to_string())]
        brand: String,

        /// Remove EVERY identity for this brand
        #[arg(long)]
        all: bool,
    },

    /// Network selection + custom/sovereign networks (mirrors the console)
    Network {
        #[command(subcommand)]
        command: NetworkCommands,
    },

    /// Wallet identity — PQ cloud custody (KMS/MPC) or local keychain
    Wallet {
        #[command(subcommand)]
        command: WalletCommands,
    },

    /// Prepaid wallet money — read the balance, mint a deposit. Both bill the
    /// ACTIVE identity: the org is derived from its token, server-side.
    Billing {
        #[command(subcommand)]
        command: BillingCommands,
    },

    /// Connect an external provider account (Cloudflare, …) to your org. The
    /// credential lives in Hanzo KMS server-side; the CLI never holds it.
    Connector {
        #[command(subcommand)]
        command: ConnectorCommands,
    },

    /// Run / join hanzo.network with hanzod (the fabric)
    Node {
        #[command(subcommand)]
        command: NodeCommands,
    },

    /// Hanzo cluster operations (talk to a local/remote hanzo node)
    Cluster {
        /// Node API base URL (defaults to the active network's api endpoint)
        #[arg(long, env = "HANZO_NODE_URL")]
        node: Option<String>,

        #[command(subcommand)]
        command: ClusterCommands,
    },

    /// Build project
    Build {
        /// Build target
        #[arg(short, long)]
        target: Option<String>,

        /// Release build
        #[arg(long)]
        release: bool,
    },

    /// Deploy to Hanzo Cloud (targets the active network; wallet signs)
    Deploy {
        /// Environment
        #[arg(short, long, default_value = "production")]
        env: String,

        /// Dry run
        #[arg(long)]
        dry_run: bool,
    },

    /// Documentation tooling (@hanzo/docs-cli)
    Docs {
        /// Arguments to pass to docs CLI
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },

    /// MDX processing (@hanzo/mdx)
    Mdx {
        /// Arguments to pass to mdx CLI
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },

    /// UI components (@hanzo/ui)
    Ui {
        /// Arguments to pass to ui CLI
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },

    /// MCP server operations (@hanzo/mcp)
    Mcp {
        /// Arguments to pass to mcp CLI
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },

    /// Version information
    Version,
}

#[derive(Subcommand)]
enum AgentCommands {
    /// Create a new agent
    Create {
        name: String,
        #[arg(long)]
        model: Option<String>,
    },
    /// List agents
    List,
    /// Run an agent
    Run { name: String, task: String },
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
        /// Short selector, e.g. my-l1
        name: String,
        /// Primary network ID (== chain-id for a sovereign L1)
        #[arg(long)]
        network_id: u64,
        /// EVM chain ID (defaults to network-id)
        #[arg(long)]
        chain_id: Option<u64>,
        /// JSON-RPC (EVM) endpoint
        #[arg(long)]
        rpc: String,
        /// Hanzo cloud/control API endpoint
        #[arg(long)]
        api: String,
        /// Block explorer URL
        #[arg(long)]
        explorer: Option<String>,
        /// Human label
        #[arg(long)]
        label: Option<String>,
        /// Also make this the active network
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
        /// Create an offline local wallet (key in the OS keychain)
        #[arg(long)]
        local: bool,
        /// Cloud custody kind: kms | mpc
        #[arg(long, default_value = "kms")]
        custody: String,
    },
    /// Import a wallet from a BIP-39 mnemonic or a 0x private key
    Import {
        /// Mnemonic phrase or 0x-prefixed private key
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
        /// Beneficiary: the IAM subject the credit lands on (`org` or `org/name`)
        #[arg(long)]
        user: String,
        /// Amount in CENTS — the unit the ledger states, so nothing is rounded
        #[arg(long)]
        cents: i64,
        /// Currency (server default: usd)
        #[arg(long)]
        currency: Option<String>,
        /// Why — recorded on the ledger row
        #[arg(long)]
        notes: Option<String>,
        /// Ledger tags (the credit's bucket)
        #[arg(long)]
        tags: Option<String>,
        /// Days until the credit expires (server default: never)
        #[arg(long)]
        expires_in: Option<u32>,
    },
}

/// `hanzo connector` — connect a provider account (Cloudflare, …). The credential
/// lives in Hanzo KMS server-side; the CLI never holds it. `add` reads the token
/// from STDIN (`--token -` or a pipe) — a literal is refused, the same law as
/// `kms secrets create` / `login --token -` (shared in `iam::secret`).
#[derive(Subcommand)]
enum ConnectorCommands {
    /// Connect a provider: verify a scoped credential and seal it into KMS. The
    /// token comes from STDIN only:
    /// `printf %s "$CF_TOKEN" | hanzo connector add --provider cloudflare --token -`
    Add {
        /// Provider to connect (e.g. cloudflare)
        #[arg(long)]
        provider: String,

        /// Non-secret account id hint (e.g. a Cloudflare account id) for when the
        /// token cannot disclose it. Optional; safe on argv (it is not a secret).
        #[arg(long)]
        account_id: Option<String>,

        /// `-` reads the token from stdin (or pipe it). A literal secret is
        /// REFUSED — it would land in `ps` and shell history.
        #[arg(long)]
        token: Option<String>,
    },

    /// List your org's connectors and their status (never the credential).
    List,

    /// Re-verify a connected credential against the provider, live.
    Verify {
        /// Provider to verify (e.g. cloudflare)
        #[arg(long)]
        provider: String,
    },

    /// Disconnect a provider: delete its KMS credential and forget it.
    Rm {
        /// Provider to disconnect (e.g. cloudflare)
        #[arg(long)]
        provider: String,
    },
}

#[derive(Subcommand)]
enum NodeCommands {
    /// Start hanzod on the active network (joins hanzo.network)
    Up {
        /// Run attached instead of detached
        #[arg(long)]
        foreground: bool,
        /// Also start the cloud control plane
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

#[derive(Subcommand)]
enum ClusterCommands {
    /// Show cluster topology (this node + discovered peers)
    Topology,
    /// List all models available across the cluster
    Models,
    /// Show which node would serve a given model
    Route {
        /// Model id
        model: String,
    },
    /// Show where to load a model that isn't served yet
    Placement {
        /// Model id
        model: String,
    },
    /// Route a chat prompt to whichever node serves the model
    Chat {
        /// Model id
        model: String,
        /// User message
        message: String,
        /// Max tokens to generate
        #[arg(long, default_value = "256")]
        max_tokens: u32,
    },
    /// Federated RAG search across the cluster
    Search {
        /// Query text
        query: String,
        /// Max results
        #[arg(long, default_value = "10")]
        max_results: u32,
    },
}

/// The command a truly-bare `hanzo` (no subcommand) resolves to: an interactive,
/// cloud-linked coding session — equivalent to `hanzo code --link`. Link is
/// forced ON (the user asked for a linked session, so it wins over any persisted
/// `code.link`); routing and the Hanzo MCP toolset stay on; the repo trust-gate
/// stays CLOSED (`project_mcp = false`), so link-by-default never widens the
/// repo-trust surface. Safety is unchanged and structural: the auth gate in
/// `commands::code::run` degrades to a purely local run when nobody is signed
/// in, so a bare `hanzo` on an unauthenticated machine streams nothing.
fn bare(resume: Option<String>) -> Commands {
    Commands::Code {
        backend: "claude".to_string(),
        link: true,
        no_link: false,
        no_route: false,
        no_mcp: false,
        project_mcp: false,
        resume,
        brand: iam::paths::DEFAULT_BRAND.to_string(),
        theme: None,
        task: None,
        passthrough: Vec::new(),
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    // ONE tree: the derive command, augmented with the generated first-class
    // product commands. There is a single parse and a single dispatch — a matched
    // cloud product goes straight through the same `api` seam, everything else is
    // a derive command (or the truly-bare `hanzo`).
    let matches = commands::product::augment(Cli::command()).get_matches();

    // Setup logging (read the globals off the matches, so both dispatch paths see
    // the same values).
    let log_level = match matches.get_count("verbose") {
        0 => "warn",
        1 => "info",
        2 => "debug",
        _ => "trace",
    };
    tracing_subscriber::fmt().with_env_filter(log_level).init();

    // Load config
    let mut config = config::Config::load(matches.get_one::<PathBuf>("config").cloned())?;

    // A matched generated product dispatches first, through the shared seam.
    if let Some(resolved) = commands::product::resolve(&matches) {
        commands::product::dispatch(&mut config, resolved).await?;
        return Ok(());
    }

    // A truly-bare `hanzo` (no subcommand) resolves to a cloud-linked coding
    // session (`bare`); every explicit subcommand routes normally. On a FRESH
    // machine (no credentials) at an interactive terminal, greet with the
    // onboarding banner + login picker first — so a first run is a welcome, not
    // the coding wrapper's "not signed in" warning — then fall through to the
    // session (which stays local until a sign-in lands).
    let cli = Cli::from_arg_matches(&matches)?;
    let command = match cli.command {
        Some(c) => c,
        None => {
            iam::onboarding::first_run(&mut config, iam::paths::DEFAULT_BRAND).await;
            // A top-level `--resume` (the form printed after every run) is sugar
            // for `hanzo code --resume` on the bare invocation.
            bare(cli.resume)
        }
    };

    // Handle commands
    match command {
        Commands::Init { template, name } => {
            commands::init::run(template, name).await?;
        }
        Commands::Dev { port, hot } => {
            commands::dev::run(port, hot).await?;
        }
        Commands::Agent { command } => {
            sdk::python::run_agent_command(command).await?;
        }
        Commands::Code {
            backend,
            link,
            no_link,
            no_route,
            no_mcp,
            project_mcp,
            resume,
            brand,
            theme,
            task,
            passthrough,
        } => {
            commands::code::run(
                &mut config,
                commands::code::Options {
                    backend,
                    link,
                    no_link,
                    route: !no_route,
                    mcp: !no_mcp,
                    project_mcp,
                    resume,
                    brand,
                    theme,
                    task,
                    passthrough,
                },
            )
            .await?;
        }
        Commands::Login { brand, provider, token } => {
            // ONE entrypoint: the picker (interactive), `--provider` (CI), and the
            // `--token -` back-compat all resolve here. A secret only ever arrives
            // on stdin or a hidden prompt — never argv.
            iam::onboarding::run_login(&mut config, &brand, provider, token).await?;
        }
        Commands::Whoami { brand, all } => {
            iam::login::whoami(&mut config, &brand, all).await?;
        }
        Commands::Usage { brand } => {
            commands::usage::usage(&mut config, &brand).await?;
        }
        Commands::Switch { identity, brand } => {
            iam::login::switch(&mut config, &brand, identity)?;
        }
        Commands::Logout { identity, brand, all } => {
            iam::login::logout(&mut config, &brand, identity, all)?;
        }
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
                &mut config,
                name,
                network_id,
                chain_id,
                rpc,
                api,
                explorer,
                label,
                activate,
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
            WalletCommands::Use { address } => {
                commands::wallet::use_wallet(&mut config, address)?
            }
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
        Commands::Node { command } => match command {
            NodeCommands::Up { foreground, with_cloud } => {
                commands::node::up(&config, foreground, with_cloud).await?
            }
            NodeCommands::Status => commands::node::status(&config).await?,
            NodeCommands::Join { network, foreground, with_cloud } => {
                commands::node::join(&mut config, network, foreground, with_cloud).await?
            }
            NodeCommands::Stop => commands::node::stop(&config)?,
        },
        Commands::Cluster { node, command } => {
            let node = node.unwrap_or_else(|| commands::network::active(&config).api);
            match command {
                ClusterCommands::Topology => commands::cluster::topology(node).await?,
                ClusterCommands::Models => commands::cluster::models(node).await?,
                ClusterCommands::Route { model } => commands::cluster::route(node, model).await?,
                ClusterCommands::Placement { model } => {
                    commands::cluster::placement(node, model).await?
                }
                ClusterCommands::Chat { model, message, max_tokens } => {
                    commands::cluster::chat(node, model, message, max_tokens).await?
                }
                ClusterCommands::Search { query, max_results } => {
                    commands::cluster::search(node, query, max_results).await?
                }
            }
        }
        Commands::Build { target, release } => {
            commands::build::run(target, release).await?;
        }
        Commands::Deploy { env, dry_run } => {
            let net = commands::network::active(&config);
            // Only an ACTIVE wallet — never auto-provision here. `deploy` does not
            // reach the control plane yet, so provisioning a signer for it wrote a
            // wallet the user never asked for, to sign a deploy that never happened.
            // Side effects wait until the command can actually do its job.
            let wallet = commands::wallet::active(&config);
            commands::deploy::run(env, dry_run, net, wallet).await?;
        }
        Commands::Docs { args } => {
            commands::ts_proxy::docs(args).await?;
        }
        Commands::Mdx { args } => {
            commands::ts_proxy::mdx(args).await?;
        }
        Commands::Ui { args } => {
            commands::ts_proxy::ui(args).await?;
        }
        Commands::Mcp { args } => {
            commands::ts_proxy::mcp(args).await?;
        }
        Commands::Version => {
            println!("{} v{}", "Hanzo CLI".bold(), env!("CARGO_PKG_VERSION"));
            println!("Multi-language SDK integration:");
            println!("  - Python SDK: Agent, Auth, MCP");
            println!("  - Go SDK: Blockchain, Infrastructure");
            println!("  - Rust: Core CLI, Network, Wallet, Node, Cluster");
            println!("  - TypeScript: Docs, MDX, UI, MCP");
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A truly-bare `hanzo` parses to no subcommand, so it falls through to `bare`.
    #[test]
    fn bare_hanzo_has_no_subcommand() {
        let cli = Cli::try_parse_from(["hanzo"]).expect("bare hanzo parses");
        assert!(cli.command.is_none());
    }

    /// The bare fallback is an interactive, cloud-linked coding session with
    /// routing + MCP on and the repo trust-gate CLOSED.
    #[test]
    fn bare_is_a_linked_interactive_code_session() {
        let Commands::Code {
            backend,
            link,
            no_link,
            no_route,
            no_mcp,
            project_mcp,
            resume,
            task,
            ..
        } = bare(None)
        else {
            panic!("bare `hanzo` must resolve to a Code session");
        };
        assert!(link, "bare `hanzo` forces link ON");
        assert!(!no_link, "bare `hanzo` never opts out of link");
        assert!(!no_route, "model routing stays on");
        assert!(!no_mcp, "the Hanzo MCP toolset stays attached");
        assert!(!project_mcp, "link-by-default must NOT widen the repo trust-gate");
        assert!(task.is_none(), "no task -> interactive");
        assert!(resume.is_none());
        assert_eq!(backend, "claude");
    }

    /// `hanzo --resume <id>` (the line printed after every run) parses at the
    /// top level and carries into the bare coding session — it is sugar for
    /// `hanzo code --resume <id>`.
    #[test]
    fn bare_resume_flag_carries_into_the_session() {
        let cli = Cli::try_parse_from(["hanzo", "--resume", "abc123"])
            .expect("`hanzo --resume <id>` parses");
        assert!(cli.command.is_none(), "no subcommand -> the bare fallback");
        assert_eq!(cli.resume.as_deref(), Some("abc123"));
        let Commands::Code { resume, .. } = bare(cli.resume) else {
            panic!("bare `hanzo` must resolve to a Code session");
        };
        assert_eq!(resume.as_deref(), Some("abc123"));
    }

    /// An explicit `hanzo code --resume <id>` still carries its own `--resume`
    /// (the subcommand flag), independent of the top-level convenience.
    #[test]
    fn explicit_code_resume_still_works() {
        let cli = Cli::try_parse_from(["hanzo", "code", "--resume", "xyz789"])
            .expect("`hanzo code --resume <id>` parses");
        let Some(Commands::Code { resume, .. }) = cli.command else {
            panic!("expected the Code wrapper");
        };
        assert_eq!(resume.as_deref(), Some("xyz789"));
    }

    /// An explicit subcommand is unchanged — it never routes through `bare`.
    #[test]
    fn explicit_subcommand_is_unchanged() {
        let cli = Cli::try_parse_from(["hanzo", "version"]).expect("`hanzo version` parses");
        assert!(matches!(cli.command, Some(Commands::Version)));
    }

    /// `hanzo usage` is a top-level verb (the money sibling of `whoami --all`):
    /// it defaults the brand, carries no `--org` (the org is the active identity's
    /// own claim, never a flag), and the generated product tree never shadows it.
    #[test]
    fn usage_is_a_top_level_verb_with_no_org_flag() {
        let cli = Cli::try_parse_from(["hanzo", "usage"]).expect("`hanzo usage` parses");
        let Some(Commands::Usage { brand }) = cli.command else {
            panic!("`hanzo usage` must parse to Usage");
        };
        assert_eq!(brand, iam::paths::DEFAULT_BRAND);
        // No `--org`: switch identity to change whose balances you see.
        assert!(Cli::try_parse_from(["hanzo", "usage", "--org", "other"]).is_err());
        // The local stacked view wins its bare name — it is not a generated product.
        let merged = commands::product::augment(Cli::command());
        let m = merged.try_get_matches_from(["hanzo", "usage"]).unwrap();
        assert!(commands::product::resolve(&m).is_none(), "`usage` is the local view, not a cloud product");
    }

    /// Explicit `hanzo code` (no `--link`) leaves the flag false so the persisted
    /// `code.link` decides — the bare-invocation override never leaks into it.
    #[test]
    fn explicit_code_does_not_force_link() {
        let cli = Cli::try_parse_from(["hanzo", "code"]).expect("`hanzo code` parses");
        let Some(Commands::Code { link, no_link, .. }) = cli.command else {
            panic!("expected Code");
        };
        assert!(!link);
        assert!(!no_link);
    }

    // ---- the merged tree: derive commands + generated products, one parse -----

    /// The augmented command must build without a clap debug-assert panic — this
    /// alone catches a duplicate subcommand or a bad arg definition across all 125
    /// generated products.
    #[test]
    fn the_merged_command_tree_is_valid() {
        commands::product::augment(Cli::command()).debug_assert();
    }

    /// A generated product resolves through the merged tree; a derive command does
    /// not (it falls through to the derive dispatch).
    #[test]
    fn a_generated_product_resolves_and_a_local_command_does_not() {
        let merged = commands::product::augment(Cli::command());
        let m = merged.clone().try_get_matches_from(["hanzo", "agents", "list"]).unwrap();
        assert!(commands::product::resolve(&m).is_some(), "a cloud product resolves");

        let m = merged.clone().try_get_matches_from(["hanzo", "version"]).unwrap();
        assert!(commands::product::resolve(&m).is_none(), "a local command is not a product");

        // bare `hanzo` -> no subcommand -> the wrapper, not a product.
        let m = merged.try_get_matches_from(["hanzo"]).unwrap();
        assert!(commands::product::resolve(&m).is_none());
    }

    /// `code` is the flagship wrapper, and ONLY the wrapper — `/v1/code` is not in
    /// the authored specs, so it is not a generated product. `hanzo code <word>`
    /// and `hanzo code "task"` both stay the wrapper (a free-text task), never a
    /// cloud verb; the augmented tree never claims the `code` name.
    #[test]
    fn code_is_the_wrapper_and_never_a_generated_product() {
        assert!(!commands::product::augment(Cli::command())
            .get_subcommands()
            .any(|s| s.get_name() == "code" && s.get_subcommands().any(|v| v.get_name() == "search")));

        let merged = commands::product::augment(Cli::command());
        let m = merged.try_get_matches_from(["hanzo", "code", "fix the bug"]).unwrap();
        assert!(commands::product::resolve(&m).is_none(), "a task stays the wrapper");
        let cli = Cli::from_arg_matches(&m).unwrap();
        let Some(Commands::Code { task, .. }) = cli.command else {
            panic!("expected the Code wrapper");
        };
        assert_eq!(task.as_deref(), Some("fix the bug"));
    }

    /// `--help` / `-h` is intercepted by clap, never swallowed by the fallback.
    #[test]
    fn help_flag_is_preserved() {
        // `.err()` avoids requiring `Cli: Debug` (which `unwrap_err` would need).
        let err = Cli::try_parse_from(["hanzo", "--help"])
            .err()
            .expect("`--help` exits via a clap error");
        assert_eq!(err.kind(), clap::error::ErrorKind::DisplayHelp);
    }

    // `hanzo kms` is now a GENERATED product (`kms secrets {list,get,create,rm}`);
    // its "a secret value can never reach argv" and "no --org" invariants are
    // pinned on the generated path in `commands::product::tests`.

    /// The `connector` command surface: the four verbs parse, `--provider` is
    /// required on the credential verbs, the non-secret `--account-id` hint is
    /// optional, and there is deliberately NO `--org` (the org is the active
    /// identity's `owner`). The credential is never an argv LITERAL — that is
    /// `--token`'s runtime law in `iam::secret` (`-`/pipe reads stdin, a literal
    /// is refused), pinned there and in `commands::connector`; here `--token -`
    /// is the stdin sentinel, not the secret.
    #[test]
    fn the_connector_surface_parses_its_verbs_and_has_no_org_flag() {
        // add: `--provider` required; the non-secret `--account-id` and the
        // stdin sentinel `--token -` are accepted and parse to the right fields.
        let cli = Cli::try_parse_from([
            "hanzo", "connector", "add", "--provider", "cloudflare", "--account-id", "acc-1",
            "--token", "-",
        ])
        .expect("`connector add --provider … --account-id … --token -` parses");
        match cli.command {
            Some(Commands::Connector {
                command: ConnectorCommands::Add { provider, account_id, token },
            }) => {
                assert_eq!(provider, "cloudflare");
                assert_eq!(account_id.as_deref(), Some("acc-1"));
                assert_eq!(token.as_deref(), Some("-"), "`-` is the stdin sentinel, not the secret");
            }
            _ => panic!("`connector add` must parse to Add"),
        }
        // The hint and the token flag are optional — the bare pipe/prompt path.
        assert!(
            Cli::try_parse_from(["hanzo", "connector", "add", "--provider", "cloudflare"]).is_ok(),
            "`--account-id` and `--token` are optional (pipe/prompt path)"
        );
        // `add` requires a provider, and takes no `--org`.
        assert!(
            Cli::try_parse_from(["hanzo", "connector", "add", "--token", "-"]).is_err(),
            "`connector add` must require --provider"
        );
        assert!(
            Cli::try_parse_from([
                "hanzo", "connector", "add", "--provider", "cloudflare", "--org", "other",
            ])
            .is_err(),
            "`connector add --org` must not exist — the org is the identity's own `owner`"
        );
        // list / verify / rm parse; verify & rm require --provider and take no --org.
        assert!(Cli::try_parse_from(["hanzo", "connector", "list"]).is_ok());
        for verb in ["verify", "rm"] {
            assert!(
                Cli::try_parse_from(["hanzo", "connector", verb, "--provider", "cloudflare"]).is_ok(),
                "`connector {verb} --provider …` must parse"
            );
            assert!(
                Cli::try_parse_from([
                    "hanzo", "connector", verb, "--provider", "cloudflare", "--org", "other",
                ])
                .is_err(),
                "`connector {verb} --org` must not exist — switch identity instead"
            );
        }
    }
}
