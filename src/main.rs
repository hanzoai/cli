use anyhow::Result;
use clap::{Parser, Subcommand};
use colored::*;
use std::path::PathBuf;

mod commands;
mod config;
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

    #[command(subcommand)]
    command: Commands,
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

    /// Authentication and authorization (Python SDK)
    Auth {
        #[command(subcommand)]
        command: AuthCommands,
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

    /// Deploy to Hanzo Cloud
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

    /// Commerce platform operations
    Commerce {
        /// API base URL
        #[arg(long, env = "HANZO_API_URL")]
        api_url: Option<String>,

        /// API key for authentication
        #[arg(long, env = "HANZO_API_KEY")]
        api_key: Option<String>,

        #[command(subcommand)]
        command: CommerceCommands,
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
    Run {
        name: String,
        task: String,
    },
}

#[derive(Subcommand)]
enum AuthCommands {
    /// Login to Hanzo
    Login {
        #[arg(long)]
        email: Option<String>,
        #[arg(long)]
        api_key: Option<String>,
    },
    /// Logout
    Logout,
    /// Show current user
    Whoami,
    /// Check auth status
    Status,
}

#[derive(Subcommand)]
enum CommerceCommands {
    /// Order operations
    Orders {
        #[command(subcommand)]
        command: OrderCommands,
    },
    /// Product operations
    Products {
        #[command(subcommand)]
        command: ProductCommands,
    },
    /// Cart operations
    Carts {
        #[command(subcommand)]
        command: CartCommands,
    },
    /// Deploy commerce to environment
    Deploy {
        /// Target environment
        #[arg(short, long, default_value = "production")]
        env: String,

        /// Dry run without making changes
        #[arg(long)]
        dry_run: bool,
    },
}

#[derive(Subcommand)]
enum OrderCommands {
    /// List orders
    List {
        /// Maximum number of orders to fetch
        #[arg(short, long)]
        limit: Option<u32>,

        /// Filter by status (pending, completed, cancelled)
        #[arg(short, long)]
        status: Option<String>,
    },
    /// Get order details
    Get {
        /// Order ID
        id: String,
    },
    /// Create a new order
    Create {
        /// Customer email
        #[arg(short, long)]
        email: String,

        /// Line items as JSON array
        #[arg(short, long)]
        items: Option<String>,
    },
}

#[derive(Subcommand)]
enum ProductCommands {
    /// List products
    List {
        /// Maximum number of products to fetch
        #[arg(short, long)]
        limit: Option<u32>,
    },
    /// Sync products from source
    Sync {
        /// Sync source (local, stripe, shopify)
        #[arg(short, long)]
        source: Option<String>,
    },
}

#[derive(Subcommand)]
enum CartCommands {
    /// View cart(s)
    View {
        /// Cart ID (optional, lists all if not provided)
        id: Option<String>,
    },
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
    
    tracing_subscriber::fmt()
        .with_env_filter(log_level)
        .init();

    // Load config
    let config = config::Config::load(cli.config)?;

    // Handle commands
    match cli.command {
        Commands::Init { template, name } => {
            commands::init::run(template, name).await?;
        }
        Commands::Dev { port, hot } => {
            commands::dev::run(port, hot).await?;
        }
        Commands::Agent { command } => {
            sdk::python::run_agent_command(command).await?;
        }
        Commands::Auth { command } => {
            sdk::python::run_auth_command(command).await?;
        }
        Commands::Build { target, release } => {
            commands::build::run(target, release).await?;
        }
        Commands::Deploy { env, dry_run } => {
            commands::deploy::run(env, dry_run).await?;
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
        Commands::Commerce { api_url, api_key, command } => {
            // Use config values as fallback
            let url = api_url.or(config.base_url);
            let key = api_key.or(config.api_key);

            match command {
                CommerceCommands::Orders { command: order_cmd } => match order_cmd {
                    OrderCommands::List { limit, status } => {
                        commands::commerce::orders_list(url, key, limit, status).await?;
                    }
                    OrderCommands::Get { id } => {
                        commands::commerce::orders_get(url, key, id).await?;
                    }
                    OrderCommands::Create { email, items } => {
                        commands::commerce::orders_create(url, key, email, items).await?;
                    }
                },
                CommerceCommands::Products { command: product_cmd } => match product_cmd {
                    ProductCommands::List { limit } => {
                        commands::commerce::products_list(url, key, limit).await?;
                    }
                    ProductCommands::Sync { source } => {
                        commands::commerce::products_sync(url, key, source).await?;
                    }
                },
                CommerceCommands::Carts { command: cart_cmd } => match cart_cmd {
                    CartCommands::View { id } => {
                        commands::commerce::carts_view(url, key, id).await?;
                    }
                },
                CommerceCommands::Deploy { env, dry_run } => {
                    commands::commerce::deploy(url, key, env, dry_run).await?;
                }
            }
        }
        Commands::Version => {
            println!("{} v{}", "Hanzo CLI".bold(), env!("CARGO_PKG_VERSION"));
            println!("Multi-language SDK integration:");
            println!("  - Python SDK: Agent, Auth, MCP, Network");
            println!("  - Go SDK: Blockchain, Infrastructure");
            println!("  - Rust: Performance, Core CLI, Commerce");
            println!("  - TypeScript: Docs, MDX, UI, MCP");
        }
    }

    Ok(())
}