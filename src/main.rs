use anyhow::Result;
use clap::{Arg, Args, Command, CommandFactory, FromArgMatches, Parser, Subcommand};
use colored::*;
use std::path::PathBuf;

mod catalog;
mod commands;
mod config;
mod iam;

#[derive(Parser)]
#[command(name = "hanzo")]
#[command(author = "Hanzo AI")]
// The version is the crate's, read at compile time. Written out as a literal it
// was a second copy of a number that only ever moves in Cargo.toml, and a copy
// that goes stale reports the wrong build to whoever is debugging one.
#[command(version)]
#[command(about = "Unified CLI for Hanzo AI development tools", long_about = None)]
struct Cli {
    /// Sets a custom config file
    #[arg(short, long, value_name = "FILE")]
    config: Option<PathBuf>,

    /// Increase logging verbosity
    #[arg(short, long, action = clap::ArgAction::Count)]
    verbose: u8,

    #[command(subcommand)]
    command: Commands,
}

/// Starting the node, said once.
///
/// It answers to two spellings — `hanzo up`, which is what the site prints, and
/// `hanzo node up`, which is where it lives among the other node verbs. Both
/// parse into THIS struct and run `Up::run`, so a flag added here arrives at
/// both spellings and there is no second implementation to keep agreeing.
#[derive(Args)]
struct Up {
    /// Run attached instead of detached
    #[arg(long)]
    foreground: bool,
    /// Also start the cloud control plane
    #[arg(long)]
    with_cloud: bool,
}

impl Up {
    async fn run(self, config: &config::Config) -> Result<()> {
        commands::node::up(config, self.foreground, self.with_cloud).await
    }
}

#[derive(Subcommand)]
enum Commands {
    /// Start hanzod on the active network (joins hanzo.network)
    Up(Up),

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

    /// Sign in to Hanzo Cloud (IAM OIDC, PKCE S256)
    Login {
        /// Brand / tenant: hanzo | lux | zoo | pars | bootnode
        #[arg(long, default_value_t = iam::paths::DEFAULT_BRAND.to_string())]
        brand: String,

        /// Force the RFC 8628 device flow (link + QR + code) instead of the browser
        #[arg(long)]
        device: bool,
    },

    /// Show the currently signed-in identity
    Whoami {
        /// Brand / tenant: hanzo | lux | zoo | pars | bootnode
        #[arg(long, default_value_t = iam::paths::DEFAULT_BRAND.to_string())]
        brand: String,
    },

    /// Sign out and remove stored credentials
    Logout {
        /// Brand / tenant: hanzo | lux | zoo | pars | bootnode
        #[arg(long, default_value_t = iam::paths::DEFAULT_BRAND.to_string())]
        brand: String,
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
enum NodeCommands {
    /// Start hanzod on the active network (joins hanzo.network)
    Up(Up),
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

/// The tree before the platform is grafted onto it.
///
/// `help` is declared here rather than left to clap because the platform serves
/// a product called `help` too, and a word can only mean one thing. Declared, it
/// carries both: the served verbs as subcommands, and the command path clap's
/// own `help` took, printed the same way.
fn root() -> Command {
    Cli::command().disable_help_subcommand(true).subcommand(
        Command::new("help").about("Print help for a command").arg(
            Arg::new("path")
                .num_args(0..)
                .value_name("COMMAND")
                .help("Command to print help for, e.g. `node status`"),
        ),
    )
}

/// Print the help of the command a path names, the way `hanzo help node status`
/// has always read.
fn help(root: &mut Command, path: &[String]) -> Result<()> {
    let mut at = &mut *root;
    for step in path {
        at = at
            .find_subcommand_mut(step)
            .ok_or_else(|| anyhow::anyhow!("no command named {step}"))?;
    }
    at.print_help()?;
    println!();
    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    // The platform's own command document, grafted on before anything is
    // parsed. This CLI keeps no second list of what the platform serves, so
    // `hanzo <service> <verb>` answers whatever the document publishes today.
    let base = std::env::var("HANZO_API_URL").unwrap_or_else(|_| "https://api.hanzo.ai".into());
    let served = catalog::Catalog::open(&base).await.ok();
    let mut tree = root();
    if let Some(c) = &served {
        tree = c.graft(tree);
    }

    let matches = tree.clone().get_matches();

    if let Some((service, sub)) = matches.subcommand() {
        if let Some((verb, leaf)) = sub.subcommand() {
            if let Some(op) = served.as_ref().and_then(|c| c.op(service, verb)) {
                return catalog::call(op, leaf, &base).await;
            }
        }
        if service == "help" {
            let path: Vec<String> = sub
                .get_many::<String>("path")
                .map(|v| v.cloned().collect())
                .unwrap_or_default();
            return help(&mut tree, &path);
        }
    }

    let cli = Cli::from_arg_matches(&matches)?;

    // Setup logging
    let log_level = match cli.verbose {
        0 => "warn",
        1 => "info",
        2 => "debug",
        _ => "trace",
    };

    tracing_subscriber::fmt().with_env_filter(log_level).init();

    // Load config
    let mut config = config::Config::load(cli.config)?;

    // Handle commands
    match cli.command {
        Commands::Up(up) => up.run(&config).await?,
        Commands::Init { template, name } => {
            commands::init::run(template, name).await?;
        }
        Commands::Dev { port, hot } => {
            commands::dev::run(port, hot).await?;
        }
        Commands::Login { brand, device } => {
            iam::login::login(&brand, device).await?;
        }
        Commands::Whoami { brand } => {
            iam::login::whoami(&brand).await?;
        }
        Commands::Logout { brand } => {
            iam::login::logout(&brand).await?;
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
        Commands::Node { command } => match command {
            NodeCommands::Up(up) => up.run(&config).await?,
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
            // A real deploy needs a signer; auto-provision one if none is set.
            let wallet = match commands::wallet::active(&config) {
                Some(w) => Some(w),
                None if !dry_run => Some(commands::wallet::ensure(&mut config).await?),
                None => None,
            };
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

/// Does the binary answer everything the platform publishes?
///
/// This is the whole point of reading the document instead of copying it, and
/// it is worth failing a build over: the bug it replaces was a served token —
/// `{Service: agents, Name: delete}` — that `hanzo agents delete` met with
/// "unrecognized subcommand", so anyone building a palette from the platform's
/// own surface shipped commands that do not run.
///
/// It reads the document LIVE and refuses to pass without it. A check that
/// falls back to a copy when the platform is unreachable reports agreement it
/// never observed, which is the failure mode it exists to catch.
#[cfg(test)]
mod tests {
    use super::*;

    fn base() -> String {
        std::env::var("HANZO_API_URL").unwrap_or_else(|_| "https://api.hanzo.ai".into())
    }

    #[tokio::test]
    async fn every_served_token_answers() {
        let base = base();
        let served = catalog::Catalog::live(&base)
            .await
            .unwrap_or_else(|e| panic!("could not read {}{}: {e:#}", base, catalog::PATH));

        let mut tree = served.graft(root());
        tree.build();

        let missing: Vec<String> = served
            .ops()
            .filter(|op| {
                let (service, verb) = op.token();
                tree.find_subcommand(service)
                    .and_then(|s| s.find_subcommand(verb))
                    .is_none()
            })
            .map(|op| format!("{} {} ({})", op.service, op.name, op.id))
            .collect();

        // Neither is a token this binary can answer, and neither is this
        // repo's to fix — printed so the gap is visible where it can be
        // closed, in the projection that publishes it.
        for op in served.nameless() {
            println!("no service, so no command: {} {} {}", op.id, op.method, op.path);
        }
        for op in served.shadowed() {
            println!("token already taken: {} {} ({})", op.service, op.name, op.id);
        }
        println!(
            "{} served tokens · {} unreachable · {} nameless",
            served.ops().count(),
            served.shadowed().len(),
            served.nameless().len()
        );

        assert!(
            missing.is_empty(),
            "{} served command(s) name a subcommand this binary does not answer:\n  {}",
            missing.len(),
            missing.join("\n  ")
        );
    }

    /// The exact spelling the bug report carried, parsed end to end.
    #[tokio::test]
    async fn the_reported_token_parses() {
        let base = base();
        let served = catalog::Catalog::live(&base)
            .await
            .unwrap_or_else(|e| panic!("could not read {}{}: {e:#}", base, catalog::PATH));
        let tree = served.graft(root());
        let m = tree
            .try_get_matches_from(["hanzo", "agents", "delete", "agent_1"])
            .expect("hanzo agents delete <ref>");
        let (service, sub) = m.subcommand().expect("a service");
        let (verb, leaf) = sub.subcommand().expect("a verb");
        assert_eq!((service, verb), ("agents", "delete"));
        assert_eq!(served.op(service, verb).unwrap().id, "delete_agents_by_ref");
        assert_eq!(leaf.get_one::<String>("ref").unwrap(), "agent_1");
    }

    /// A local verb keeps its own name and its own flags after the graft.
    #[tokio::test]
    async fn a_local_verb_survives_a_service_of_the_same_name() {
        let served = catalog::Catalog::live(&base())
            .await
            .expect("the platform's command document");
        let tree = served.graft(root());
        let m = tree
            .try_get_matches_from(["hanzo", "deploy", "--env", "staging"])
            .expect("hanzo deploy --env");
        let cli = Cli::from_arg_matches(&m).expect("the local deploy");
        match cli.command {
            Commands::Deploy { env, .. } => assert_eq!(env, "staging"),
            _ => panic!("deploy stopped being deploy"),
        }
    }
}
