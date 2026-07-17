use anyhow::{anyhow, Result};
use colored::*;
use std::process::Command;

/// Proxy commands to TypeScript-based CLIs via npx
pub struct TsProxy {
    package: String,
    command: String,
}

impl TsProxy {
    pub fn new(package: &str, command: &str) -> Self {
        Self {
            package: package.to_string(),
            command: command.to_string(),
        }
    }

    pub fn run(&self, args: Vec<String>) -> Result<()> {
        let npx = which::which("npx")
            .map_err(|_| anyhow!("npx not found. Please install Node.js"))?;

        let mut cmd = Command::new(npx);
        cmd.arg(&self.package);

        if !args.is_empty() {
            cmd.args(&args);
        }

        println!(
            "{} {} {}",
            "→".cyan(),
            self.command.bold(),
            args.join(" ").dimmed()
        );

        let status = cmd.status()?;

        if !status.success() {
            return Err(anyhow!(
                "{} exited with status: {}",
                self.command,
                status.code().unwrap_or(-1)
            ));
        }

        Ok(())
    }
}

/// Run docs CLI commands
pub async fn docs(args: Vec<String>) -> Result<()> {
    let proxy = TsProxy::new("@hanzo/docs-cli", "hanzo docs");
    proxy.run(args)
}

/// Run MDX CLI commands
pub async fn mdx(args: Vec<String>) -> Result<()> {
    let proxy = TsProxy::new("@hanzo/mdx", "hanzo mdx");
    proxy.run(args)
}

/// Run UI CLI commands
pub async fn ui(args: Vec<String>) -> Result<()> {
    let proxy = TsProxy::new("@hanzo/ui", "hanzo ui");
    proxy.run(args)
}

/// Run MCP CLI commands
pub async fn mcp(args: Vec<String>) -> Result<()> {
    let proxy = TsProxy::new("@hanzo/mcp", "hanzo mcp");
    proxy.run(args)
}
