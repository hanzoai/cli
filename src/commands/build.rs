use anyhow::{bail, Result};
use colored::*;

pub async fn run(target: Option<String>, release: bool) -> Result<()> {
    let build_type = if release { "release" } else { "debug" };
    
    println!("{} Building project in {} mode", 
        "🔨".cyan(), 
        build_type.yellow()
    );
    
    if let Some(t) = target {
        println!("  Target: {}", t.green());
    }
    
    // Never implemented: no process is spawned, nothing is built. Exiting 0 said
    // otherwise. Refuse instead of pretending.
    bail!("`hanzo build` is not implemented — it runs nothing. Use `cargo build` / the project's own build tool.")
}