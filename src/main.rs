use anyhow::Result;
use clap::{Parser, Subcommand};
use colored::*;
use std::path::PathBuf;

mod commands;
mod config;
mod private;
mod iam;
mod sdk;

#[derive(Parser)]
#[command(name = "hanzo")]
#[command(author = "Hanzo AI")]
#[command(version = "1.0.0")]
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

    /// Sign in to Hanzo Cloud (IAM OIDC, PKCE S256)
    Login {
        /// Brand / tenant: hanzo | lux | zoo | pars | bootnode
        #[arg(long, default_value_t = iam::paths::DEFAULT_BRAND.to_string())]
        brand: String,

        /// Store a hanzo.id bearer token directly instead of the browser flow
        /// (like `gh auth login --with-token`). Reads the token from stdin when
        /// given the value `-`, so it never lands in argv or shell history.
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
    let cli = Cli::parse();

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

    // A truly-bare `hanzo` (no subcommand) resolves to a cloud-linked coding
    // session (`bare`); every explicit subcommand routes normally.
    let command = cli.command.unwrap_or_else(bare);

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
        Commands::Login { brand, token } => match token {
            Some(t) => {
                // Read from stdin when `-` so the token never rides argv/history.
                let raw = if t == "-" {
                    use std::io::Read;
                    let mut s = String::new();
                    std::io::stdin().read_to_string(&mut s)?;
                    s
                } else {
                    t
                };
                let raw = raw.trim();
                if raw.is_empty() {
                    anyhow::bail!("no token provided");
                }
                iam::login::login_with_token(&mut config, &brand, raw).await?;
            }
            None => iam::login::login(&mut config, &brand).await?,
        },
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

    /// `--help` / `-h` is intercepted by clap, never swallowed by the fallback.
    #[test]
    fn help_flag_is_preserved() {
        // `.err()` avoids requiring `Cli: Debug` (which `unwrap_err` would need).
        let err = Cli::try_parse_from(["hanzo", "--help"])
            .err()
            .expect("`--help` exits via a clap error");
        assert_eq!(err.kind(), clap::error::ErrorKind::DisplayHelp);
    }
}
