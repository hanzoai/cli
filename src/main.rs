use anyhow::Result;
use clap::{Parser, Subcommand};
use colored::*;
use std::path::PathBuf;

mod commands;
mod config;
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

    /// Sign in to Hanzo Cloud (IAM OIDC, PKCE S256)
    Login {
        /// Brand / tenant: hanzo | lux | zoo | pars | bootnode
        #[arg(long, default_value_t = iam::paths::DEFAULT_BRAND.to_string())]
        brand: String,
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
        Commands::Login { brand } => {
            iam::login::login(&brand).await?;
        }
        Commands::Whoami { brand } => {
            iam::login::whoami(&brand).await?;
        }
        Commands::Logout { brand } => {
            iam::login::logout(&brand).await?;
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
        Commands::Version => {
            println!("{} v{}", "Hanzo CLI".bold(), env!("CARGO_PKG_VERSION"));
            println!("Multi-language SDK integration:");
            println!("  - Python SDK: Agent, Auth, MCP, Network");
            println!("  - Go SDK: Blockchain, Infrastructure");
            println!("  - Rust: Performance, Core CLI");
            println!("  - TypeScript: Docs, MDX, UI, MCP");
        }
    }

    Ok(())
}