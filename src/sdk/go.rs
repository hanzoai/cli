use anyhow::Result;
use std::process::Command;
use colored::*;

pub async fn run_blockchain_command() -> Result<()> {
    println!("{} Running Go SDK blockchain command...", "🔗".cyan());
    
    let sdk_path = dirs::home_dir()
        .unwrap()
        .join("work/hanzo/sdk/src/go");
    
    // Check if Go binary exists
    let go_binary = sdk_path.join("hanzo");
    
    if !go_binary.exists() {
        println!("{} Go SDK not built. Building now...", "⚙️".yellow());
        
        // Build the Go SDK
        let build_output = Command::new("go")
            .arg("build")
            .arg("-o")
            .arg("hanzo")
            .arg("cmd/hanzo/main.go")
            .current_dir(&sdk_path)
            .output()?;
        
        if !build_output.status.success() {
            eprintln!("{}", String::from_utf8_lossy(&build_output.stderr));
            anyhow::bail!("Failed to build Go SDK");
        }
    }
    
    // Run the Go command
    let output = Command::new(go_binary)
        .args(&["blockchain", "info"])
        .output()?;
    
    if output.status.success() {
        println!("{}", String::from_utf8_lossy(&output.stdout));
    } else {
        eprintln!("{}", String::from_utf8_lossy(&output.stderr));
        anyhow::bail!("Go SDK command failed");
    }
    
    Ok(())
}