use anyhow::Result;
use std::process::Command;
use colored::*;

use crate::AgentCommands;

pub async fn run_agent_command(command: AgentCommands) -> Result<()> {
    // Convert to owned strings to avoid lifetime issues
    let python_args: Vec<String> = match command {
        AgentCommands::Create { name, model } => {
            let mut args = vec!["agent".to_string(), "create".to_string(), name];
            if let Some(m) = model {
                args.push("--model".to_string());
                args.push(m);
            }
            args
        }
        AgentCommands::List => vec!["agent".to_string(), "list".to_string()],
        AgentCommands::Run { name, task } => {
            vec!["agent".to_string(), "run".to_string(), name, task]
        }
    };

    // Convert Vec<String> to Vec<&str> for the function call
    let args_refs: Vec<&str> = python_args.iter().map(|s| s.as_str()).collect();
    run_python_sdk(args_refs).await
}

async fn run_python_sdk(args: Vec<&str>) -> Result<()> {
    println!("{} Running Python SDK command...", "🐍".green());
    
    let sdk_path = dirs::home_dir()
        .unwrap()
        .join("work/hanzo/sdk/src/py");
    
    let output = Command::new("python3")
        .arg("-m")
        .arg("hanzo.cli")
        .args(&args)
        .env("PYTHONPATH", sdk_path)
        .output()?;
    
    if output.status.success() {
        println!("{}", String::from_utf8_lossy(&output.stdout));
    } else {
        eprintln!("{}", String::from_utf8_lossy(&output.stderr));
        anyhow::bail!("Python SDK command failed");
    }
    
    Ok(())
}