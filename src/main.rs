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

    /// Secrets, in KMS — the only place they live (`list`, `get`, `set`, `rm`).
    /// Scoped to the active identity's org; `hanzo switch` moves it.
    Kms {
        #[command(subcommand)]
        command: KmsCommands,
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

/// The secret lifecycle. An address is `NAME` or `sub/path/NAME`, the same in
/// every verb; `--env` selects the environment.
///
/// `set` takes NO value argument — not by default, but structurally: a secret
/// on the command line would land in `ps`, shell history and CI logs, so the
/// value can only arrive on stdin. This is why there is no `--value` here, and
/// why adding one would be a security regression rather than a convenience.
#[derive(Subcommand)]
enum KmsCommands {
    /// List secret addresses (never values) — pipes into `hanzo kms get`
    List {
        /// Sub-path to list under, e.g. `ci`. Omit for the org root.
        #[arg(value_name = "PATH")]
        path: Option<String>,

        /// Environment to read from
        #[arg(long, default_value = "default")]
        env: String,
    },

    /// Print one secret's raw value to stdout (pipes byte-exactly)
    Get {
        /// `NAME` or `sub/path/NAME`
        #[arg(value_name = "NAME")]
        name: String,

        /// Environment to read from
        #[arg(long, default_value = "default")]
        env: String,
    },

    /// Store a secret, reading the VALUE FROM STDIN so it never enters argv:
    /// `printf %s "$V" | hanzo kms set NAME --env prod`
    Set {
        /// `NAME` or `sub/path/NAME`
        #[arg(value_name = "NAME")]
        name: String,

        /// Environment to write to. REQUIRED, with no default: the server
        /// refuses to guess, because a silent `default` would commit the write
        /// to a bucket the env's readers never resolve.
        #[arg(long)]
        env: String,
    },

    /// Delete a secret
    Rm {
        /// `NAME` or `sub/path/NAME`
        #[arg(value_name = "NAME")]
        name: String,

        /// Environment to delete from
        #[arg(long, default_value = "default")]
        env: String,
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
fn bare() -> Commands {
    Commands::Code {
        backend: "claude".to_string(),
        link: true,
        no_link: false,
        no_route: false,
        no_mcp: false,
        project_mcp: false,
        resume: None,
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
            bare()
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
        Commands::Kms { command } => match command {
            KmsCommands::List { path, env } => commands::kms::list(&mut config, path, env).await?,
            KmsCommands::Get { name, env } => commands::kms::get(&mut config, name, env).await?,
            KmsCommands::Set { name, env } => {
                // The value can only come from stdin — the same reason
                // `login --token -` does, and here there is not even a flag to
                // pass it by instead.
                let value = commands::kms::read_value(std::io::stdin())?;
                commands::kms::set(&mut config, name, env, value).await?
            }
            KmsCommands::Rm { name, env } => commands::kms::rm(&mut config, name, env).await?,
        },
        Commands::Login { brand, provider, token } => {
            // ONE entrypoint: the picker (interactive), `--provider` (CI), and the
            // `--token -` back-compat all resolve here. A secret only ever arrives
            // on stdin or a hidden prompt — never argv.
            iam::onboarding::run_login(&mut config, &brand, provider, token).await?;
        }
        Commands::Whoami { brand, all } => {
            iam::login::whoami(&mut config, &brand, all).await?;
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
        } = bare()
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

    /// An explicit subcommand is unchanged — it never routes through `bare`.
    #[test]
    fn explicit_subcommand_is_unchanged() {
        let cli = Cli::try_parse_from(["hanzo", "version"]).expect("`hanzo version` parses");
        assert!(matches!(cli.command, Some(Commands::Version)));
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

    // ---- `hanzo kms`: a secret value can never reach argv -------------------

    /// THE invariant of a secrets CLI. Not "we don't put the value in argv" —
    /// there must be NO WAY to, so that no flag, habit or copied snippet can
    /// leak it to `ps`, `~/.zsh_history` or a CI log. Every argv shape that
    /// would carry a value must be a PARSE ERROR, which is a property of the
    /// grammar rather than of the handler's discipline.
    #[test]
    fn a_secret_value_cannot_be_passed_on_the_command_line() {
        // A positional value: rejected — `Set` has exactly one positional.
        assert!(
            Cli::try_parse_from(["hanzo", "kms", "set", "DB_PASSWORD", "--env", "prod", "hunter2"])
                .is_err(),
            "a positional value must not parse — it would land in `ps` and history"
        );
        // The flags a user would reach for, none of which exist.
        for flag in ["--value", "--secret", "--data", "--from-literal", "--val"] {
            assert!(
                Cli::try_parse_from(["hanzo", "kms", "set", "DB", "--env", "prod", flag, "hunter2"])
                    .is_err(),
                "{flag} must not exist: a value-bearing flag is the leak"
            );
        }
        // What DOES parse carries the address and the env — and nothing else.
        let cli = Cli::try_parse_from(["hanzo", "kms", "set", "ci/DB", "--env", "prod"]).unwrap();
        match cli.command {
            Some(Commands::Kms { command: KmsCommands::Set { name, env } }) => {
                assert_eq!((name.as_str(), env.as_str()), ("ci/DB", "prod"));
            }
            _ => panic!("`kms set NAME --env E` must parse to Set"),
        }
    }

    /// The server refuses to guess an env on a WRITE (a silent default splits
    /// the write from the record its readers resolve). The CLI mirrors that
    /// rather than papering over it — so `set` without `--env` cannot parse.
    #[test]
    fn set_refuses_to_guess_an_environment_but_reads_default() {
        assert!(
            Cli::try_parse_from(["hanzo", "kms", "set", "DB"]).is_err(),
            "`set` must require --env: a silent default writes where nobody reads"
        );
        // Reads/deletes keep the server's own `default` compat.
        for verb in ["get", "rm"] {
            let cli = Cli::try_parse_from(["hanzo", "kms", verb, "DB"]).unwrap();
            let env = match cli.command {
                Some(Commands::Kms { command: KmsCommands::Get { env, .. } }) => env,
                Some(Commands::Kms { command: KmsCommands::Rm { env, .. } }) => env,
                _ => panic!("`kms {verb} NAME` must parse"),
            };
            assert_eq!(env, "default");
        }
    }

    /// There is deliberately no `--org`: the org is the active identity's own
    /// `owner` claim, and a flag would be a way to ask for someone else's.
    #[test]
    fn there_is_no_org_flag_on_any_kms_verb() {
        for verb in ["list", "get", "rm"] {
            assert!(
                Cli::try_parse_from(["hanzo", "kms", verb, "DB", "--org", "other"]).is_err(),
                "`kms {verb} --org` must not exist — switch identity instead"
            );
        }
    }
}
