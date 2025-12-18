//! Container runtime commands for Hanzo CLI

use anyhow::{Result, anyhow};
use colored::*;
use std::process::Command;
use serde_json::json;

/// Detects if the image is a compose stack or single container
pub fn detect_workload_type(image: &str) -> WorkloadKind {
    // Check if it's a directory with docker-compose.yml
    if std::path::Path::new(image).is_dir() {
        let compose_file = std::path::Path::new(image).join("docker-compose.yml");
        let compose_yaml = std::path::Path::new(image).join("docker-compose.yaml");

        if compose_file.exists() || compose_yaml.exists() {
            return WorkloadKind::ComposeStack;
        }
    }

    // Special handling for known stacks
    if image == "supabase/supabase" || image.contains("supabase") {
        return WorkloadKind::SupabaseStack;
    }

    // Default to single container
    WorkloadKind::SingleContainer
}

#[derive(Debug, Clone)]
pub enum WorkloadKind {
    SingleContainer,
    ComposeStack,
    SupabaseStack,
}

/// Handles the 'hanzo run' command
pub async fn handle_run(
    image: &str,
    command: Vec<String>,
    detach: bool,
    ports: Vec<String>,
    env: Vec<String>,
    volumes: Vec<String>,
) -> Result<()> {
    let workload_type = detect_workload_type(image);

    match workload_type {
        WorkloadKind::SupabaseStack => {
            println!("{}", "🚀 Starting Supabase stack...".cyan().bold());
            handle_supabase_stack().await
        }
        WorkloadKind::ComposeStack => {
            println!("{}", format!("📦 Starting compose stack from {}", image).cyan());
            handle_compose_stack(image).await
        }
        WorkloadKind::SingleContainer => {
            println!("{}", format!("🐳 Running container: {}", image).cyan());
            handle_single_container(image, command, detach, ports, env, volumes).await
        }
    }
}

/// Handles running Supabase stack
async fn handle_supabase_stack() -> Result<()> {
    println!("{}", "📥 Setting up Supabase...".yellow());

    // Check if Supabase repo exists locally
    let supabase_dir = "/tmp/supabase";
    if !std::path::Path::new(supabase_dir).exists() {
        println!("{}", "📦 Cloning Supabase repository...".yellow());
        Command::new("git")
            .args(&["clone", "https://github.com/supabase/supabase.git", supabase_dir])
            .status()?;
    }

    // Navigate to docker directory
    let docker_dir = format!("{}/docker", supabase_dir);

    // Copy example env
    let env_file = format!("{}/.env", docker_dir);
    if !std::path::Path::new(&env_file).exists() {
        println!("{}", "📝 Creating environment configuration...".yellow());
        Command::new("cp")
            .args(&[
                &format!("{}/.env.example", docker_dir),
                &env_file,
            ])
            .status()?;
    }

    // Pull all images first
    println!("{}", "📥 Pulling Supabase images...".yellow());
    Command::new("docker")
        .current_dir(&docker_dir)
        .args(&["compose", "pull"])
        .status()?;

    // Start the stack
    println!("{}", "🚀 Starting Supabase services...".green());
    let status = Command::new("docker")
        .current_dir(&docker_dir)
        .args(&["compose", "up", "-d"])
        .status()?;

    if status.success() {
        println!("{}", "✅ Supabase is running!".green().bold());
        println!("📊 Studio: {}", "http://localhost:54323".blue().underline());
        println!("🔌 API: {}", "http://localhost:54321".blue().underline());
        println!("🗄️  Database: {}", "postgresql://postgres:postgres@localhost:54322/postgres".blue());

        // Record in hanzod
        record_deployment("supabase/supabase").await?;
    } else {
        return Err(anyhow!("Failed to start Supabase"));
    }

    Ok(())
}

/// Records deployment in hanzod
async fn record_deployment(image: &str) -> Result<()> {
    // Try to send workload to hanzod API if running
    if let Ok(client) = reqwest::Client::builder().build() {
        let workload = json!({
            "id": format!("{}-{}", image.replace('/', "-"), chrono::Utc::now().timestamp()),
            "workload_type": {
                "Compute": {
                    "image": image,
                    "command": []
                }
            },
            "resources": {
                "memory_mb": 1024,
                "cpu_cores": 1.0
            }
        });

        let _ = client
            .post("http://localhost:3690/v1/workloads")
            .json(&workload)
            .send()
            .await;
    }

    Ok(())
}

/// Handles compose stack
async fn handle_compose_stack(path: &str) -> Result<()> {
    Command::new("docker")
        .current_dir(path)
        .args(&["compose", "up", "-d"])
        .status()?;

    Ok(())
}

/// Handles single container
pub async fn handle_single_container(
    image: &str,
    command: Vec<String>,
    detach: bool,
    ports: Vec<String>,
    env: Vec<String>,
    volumes: Vec<String>,
) -> Result<()> {
    // First, ensure image is pulled
    println!("{}", format!("📥 Pulling image: {}", image).yellow());
    let pull_status = Command::new("docker")
        .args(&["pull", image])
        .status()?;

    if !pull_status.success() {
        return Err(anyhow!("Failed to pull image: {}", image));
    }

    // Build docker run command
    let mut args = vec!["run"];

    if detach {
        args.push("-d");
    }

    // Add port mappings
    for port in &ports {
        args.push("-p");
        args.push(port);
    }

    // Add environment variables
    for env_var in &env {
        args.push("-e");
        args.push(env_var);
    }

    // Add volume mounts
    for volume in &volumes {
        args.push("-v");
        args.push(volume);
    }

    // Add image
    args.push(image);

    // Add command if specified
    for cmd in &command {
        args.push(cmd);
    }

    // Run the container
    println!("{}", "🏃 Starting container...".green());
    let status = Command::new("docker")
        .args(&args)
        .status()?;

    if status.success() {
        println!("{}", "✅ Container started successfully".green().bold());
        record_deployment(image).await?;
    } else {
        return Err(anyhow!("Failed to start container"));
    }

    Ok(())
}