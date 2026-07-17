use anyhow::Result;
use colored::*;
use std::fs;
use std::path::Path;

pub async fn run(template: String, name: Option<String>) -> Result<()> {
    let project_name = name.unwrap_or_else(|| "hanzo-project".to_string());
    let project_path = Path::new(&project_name);
    
    if project_path.exists() {
        anyhow::bail!("Directory {} already exists", project_name);
    }
    
    println!("{} Initializing {} project: {}", 
        "✨".green(), 
        template.cyan(), 
        project_name.yellow()
    );
    
    // Create project directory
    fs::create_dir_all(project_path)?;
    
    // Create basic structure based on template
    match template.as_str() {
        "rust" => create_rust_project(project_path)?,
        "python" => create_python_project(project_path)?,
        "typescript" => create_typescript_project(project_path)?,
        _ => create_default_project(project_path)?,
    }
    
    println!("{} Project created successfully!", "✅".green());
    println!("\nNext steps:");
    println!("  cd {}", project_name);
    println!("  hanzo dev");
    
    Ok(())
}

fn create_default_project(path: &Path) -> Result<()> {
    // Create README
    fs::write(
        path.join("README.md"),
        "# Hanzo Project\n\nCreated with Hanzo CLI\n"
    )?;
    
    // Create .gitignore
    fs::write(
        path.join(".gitignore"),
        "node_modules/\n.env\n*.log\ntarget/\ndist/\n"
    )?;
    
    Ok(())
}

fn create_rust_project(path: &Path) -> Result<()> {
    create_default_project(path)?;
    
    // Create Cargo.toml
    fs::write(
        path.join("Cargo.toml"),
        r#"[package]
name = "hanzo-project"
version = "0.1.0"
edition = "2021"

[dependencies]
tokio = { version = "1", features = ["full"] }
"#
    )?;
    
    // Create src/main.rs
    fs::create_dir_all(path.join("src"))?;
    fs::write(
        path.join("src/main.rs"),
        r#"#[tokio::main]
async fn main() {
    println!("Hello from Hanzo!");
}
"#
    )?;
    
    Ok(())
}

fn create_python_project(path: &Path) -> Result<()> {
    create_default_project(path)?;
    
    // Create pyproject.toml
    fs::write(
        path.join("pyproject.toml"),
        r#"[project]
name = "hanzo-project"
version = "0.1.0"
dependencies = []
"#
    )?;
    
    // Create main.py
    fs::write(
        path.join("main.py"),
        r#"#!/usr/bin/env python3

def main():
    print("Hello from Hanzo!")

if __name__ == "__main__":
    main()
"#
    )?;
    
    Ok(())
}

fn create_typescript_project(path: &Path) -> Result<()> {
    create_default_project(path)?;
    
    // Create package.json
    fs::write(
        path.join("package.json"),
        r#"{
  "name": "hanzo-project",
  "version": "0.1.0",
  "scripts": {
    "dev": "node index.js"
  }
}
"#
    )?;
    
    // Create index.js
    fs::write(
        path.join("index.js"),
        r#"console.log("Hello from Hanzo!");
"#
    )?;
    
    Ok(())
}