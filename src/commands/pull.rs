//! `hanzo pull <id>` — pull down a cloud coding run's branch into the local repository.
//!
//! Fetches and checks out the git branch created by a coding agent run:
//!   hanzo pull <session_or_run_id>
//!
//! Resolves the branch name (e.g. `agent/<short_id>`), fetches it from the git remote,
//! and checks it out so you can immediately inspect, test, and continue working on the code.

use anyhow::{bail, Context, Result};
use colored::*;
use std::path::PathBuf;
use std::process::Command;

/// Derives the git branch name for a given session or run id.
///
/// Matches the server's BranchFor convention: `agent/<shortID>` where shortID is
/// the last 12 chars of the id (or the id itself if shorter).
pub fn branch_for(id: &str) -> String {
    let trimmed = id.trim();
    if trimmed.starts_with("agent/") {
        return trimmed.to_string();
    }
    let suffix = if let Some((_, rest)) = trimmed.rsplit_once('_') {
        rest
    } else {
        trimmed
    };
    let short = if suffix.len() > 12 {
        &suffix[suffix.len() - 12..]
    } else {
        suffix
    };
    format!("agent/{}", short)
}

/// Execute `hanzo pull <id>`
pub async fn run(id: &str, target_dir: Option<PathBuf>) -> Result<()> {
    let cwd = target_dir.unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
    let branch = branch_for(id);

    println!("{} Pulling coding run `{}`...", "→".cyan().bold(), id.bold());

    // 1. Verify that cwd is inside a git work tree
    let check_git = Command::new("git")
        .arg("-C")
        .arg(&cwd)
        .args(["rev-parse", "--is-inside-work-tree"])
        .output();

    match check_git {
        Ok(out) if out.status.success() && String::from_utf8_lossy(&out.stdout).trim() == "true" => {}
        _ => {
            bail!(
                "Directory `{}` is not inside a git repository.\nPlease run `hanzo pull <id>` inside the repository you want to update.",
                cwd.display()
            );
        }
    }

    // 2. Fetch the branch from origin
    println!("{} Fetching branch `{}` from remote...", "→".cyan().bold(), branch);

    let fetch_res = Command::new("git")
        .arg("-C")
        .arg(&cwd)
        .args(["fetch", "origin", &format!("{}:{}", branch, branch)])
        .output();

    let fetched = match fetch_res {
        Ok(out) if out.status.success() => true,
        _ => {
            // Try fetching just the ref if local branch already exists
            let fetch_ref = Command::new("git")
                .arg("-C")
                .arg(&cwd)
                .args(["fetch", "origin", &branch])
                .output();
            matches!(fetch_ref, Ok(out) if out.status.success())
        }
    };

    if !fetched {
        // As a fallback, try fetching all heads
        let _ = Command::new("git")
            .arg("-C")
            .arg(&cwd)
            .args(["fetch", "origin"])
            .output();
    }

    // 3. Checkout the branch
    println!("{} Checking out `{}`...", "→".cyan().bold(), branch);
    let checkout = Command::new("git")
        .arg("-C")
        .arg(&cwd)
        .args(["checkout", &branch])
        .output()
        .context("failed to execute git checkout")?;

    if !checkout.status.success() {
        let err_msg = String::from_utf8_lossy(&checkout.stderr);
        bail!(
            "Could not check out branch `{}`:\n{}",
            branch,
            err_msg.trim()
        );
    }

    // 4. Print recent commit info
    if let Ok(log_out) = Command::new("git")
        .arg("-C")
        .arg(&cwd)
        .args(["log", "-1", "--oneline"])
        .output()
    {
        if log_out.status.success() {
            let commit = String::from_utf8_lossy(&log_out.stdout).trim().to_string();
            println!("{} Head commit: {}", "✓".green().bold(), commit);
        }
    }

    println!(
        "{} Successfully pulled work from run `{}` into branch `{}`.",
        "✓".green().bold(),
        id.bold(),
        branch.bold()
    );

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_branch_for() {
        assert_eq!(branch_for("agent/65a4760da6f0"), "agent/65a4760da6f0");
        assert_eq!(branch_for("65a4760da6f0"), "agent/65a4760da6f0");
        assert_eq!(
            branch_for("sess_0123456789abcdef0123456789abcdef"),
            "agent/456789abcdef"
        );
        assert_eq!(branch_for("sess_abc"), "agent/abc");
    }
}
