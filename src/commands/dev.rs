<<<<<<< HEAD
use anyhow::Result;
=======
use anyhow::{bail, Result};
>>>>>>> 907d98a8
use colored::*;

pub async fn run(port: u16, hot: bool) -> Result<()> {
    println!("{} Starting development server on port {}", 
        "🚀".green(), 
        port.to_string().cyan()
    );
    
    if hot {
        println!("  {} Hot reload enabled", "🔥".yellow());
    }
    
<<<<<<< HEAD
    // TODO: Implement actual dev server logic
    println!("Dev server would start here...");
    
    Ok(())
=======
    // Never implemented: no process is spawned, no server starts. Exiting 0 said
    // otherwise. Refuse instead of pretending.
    bail!("`hanzo dev` is not implemented — it runs nothing. Use the project's own dev server.")
>>>>>>> 907d98a8
}