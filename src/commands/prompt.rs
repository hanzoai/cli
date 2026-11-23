//! `hanzo prompt` — canonical unified system prompt (`# Hanzo Rules`),
//! backed by `https://github.com/hanzoai/prompt`, with Hanzo MCP and
//! progressive skill discovery via `https://hanzoskills.com/llms.txt`.

use anyhow::{Context, Result};
use clap::{Args, Subcommand};
use colored::*;
use serde_json::json;
use std::path::PathBuf;
use std::time::Duration;

pub const HANZO_PROMPT: &str = include_str!("../../PROMPT.md");
pub const HANZO_PROMPT_REPO: &str = "https://github.com/hanzoai/prompt";
pub const HANZO_PROMPT_RAW_URL: &str =
    "https://raw.githubusercontent.com/hanzoai/prompt/main/PROMPT.md";
pub const HANZO_SKILLS_INDEX_URL: &str = "https://hanzoskills.com/llms.txt";
pub const HANZO_MCP_URL: &str = "https://api.hanzo.ai/v1/mcp";

#[derive(Args, Clone)]
pub struct PromptArgs {
    #[command(subcommand)]
    pub command: Option<PromptCommand>,

    /// Output JSON metadata + prompt text
    #[arg(long)]
    pub json: bool,
}

#[derive(Subcommand, Clone)]
pub enum PromptCommand {
    /// Print the canonical `# Hanzo Rules` system prompt (default)
    Show {
        /// Output JSON metadata + prompt text
        #[arg(long)]
        json: bool,
    },
    /// Install the canonical prompt to `~/.hanzo/PROMPT.md`
    Install {
        /// Fetch the latest PROMPT.md from github.com/hanzoai/prompt before writing
        #[arg(long)]
        refresh: bool,
    },
    /// Print the local installed prompt path (`~/.hanzo/PROMPT.md`)
    Path,
    /// Progressively discover matching `$skill` entries from `hanzoskills.com/llms.txt`
    Skills {
        /// Query words (e.g. "deploy", "iam", "base")
        query: Vec<String>,
        /// Maximum number of skills to return
        #[arg(long, default_value_t = 8)]
        limit: usize,
        /// Emit raw JSON
        #[arg(long)]
        json: bool,
    },
}

pub fn installed_prompt_path() -> Result<PathBuf> {
    let home = dirs::home_dir().context("no home directory")?;
    Ok(home.join(".hanzo").join("PROMPT.md"))
}

fn sync_prompt_file(path: &std::path::Path, content: &str, force: bool) {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let should_write = match std::fs::read_to_string(path) {
        Ok(existing) => {
            existing.trim() != content.trim()
                && (force || existing.trim_start().starts_with("# Hanzo Rules"))
        }
        Err(_) => true,
    };
    if should_write {
        let _ = std::fs::write(path, content);
    }
}

/// Ensure `~/.hanzo/PROMPT.md` and Hanzo's isolated agent homes
/// (`~/.hanzo/claude/CLAUDE.md`, `~/.hanzo/dev/AGENTS.md`) carry the canonical
/// `# Hanzo Rules` system prompt.
pub fn ensure_installed_prompt() -> Option<PathBuf> {
    let path = installed_prompt_path().ok()?;
    sync_prompt_file(&path, HANZO_PROMPT, true);
    if let Some(hanzo_dir) = path.parent() {
        sync_prompt_file(&hanzo_dir.join("claude").join("CLAUDE.md"), HANZO_PROMPT, false);
        sync_prompt_file(&hanzo_dir.join("dev").join("AGENTS.md"), HANZO_PROMPT, false);
    }
    Some(path)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillEntry {
    pub slug: String,
    pub url: String,
    pub description: String,
}

pub fn parse_skills_index(markdown: &str) -> Vec<SkillEntry> {
    let mut out = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for raw in markdown.lines() {
        let line = raw.trim();
        let Some(rest) = line.strip_prefix("- [") else {
            continue;
        };
        let Some((name_part, after_name)) = rest.split_once("](") else {
            continue;
        };
        let Some((url_part, after_url)) = after_name.split_once(')') else {
            continue;
        };
        let slug = name_part.trim().to_string();
        let url = url_part.trim().to_string();
        if slug.is_empty() || !url.starts_with("http") || !seen.insert(slug.clone()) {
            continue;
        }
        let description = after_url
            .trim()
            .trim_start_matches(':')
            .trim()
            .to_string();
        out.push(SkillEntry {
            slug,
            url,
            description,
        });
    }
    out
}

pub fn match_skills(entries: &[SkillEntry], query: &str, limit: usize) -> Vec<SkillEntry> {
    let terms: Vec<String> = query
        .split_whitespace()
        .map(|t| t.trim_start_matches('$').to_ascii_lowercase())
        .filter(|t| !t.is_empty())
        .collect();
    if terms.is_empty() {
        return entries.iter().take(limit).cloned().collect();
    }
    let mut scored: Vec<(usize, &SkillEntry)> = entries
        .iter()
        .filter_map(|e| {
            let slug_l = e.slug.to_ascii_lowercase();
            let desc_l = e.description.to_ascii_lowercase();
            let mut score = 0usize;
            for t in &terms {
                if slug_l == *t {
                    score += 100;
                } else if slug_l.contains(t) {
                    score += 30;
                } else if desc_l.contains(t) {
                    score += 10;
                }
            }
            (score > 0).then_some((score, e))
        })
        .collect();
    scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.slug.cmp(&b.1.slug)));
    scored.into_iter().take(limit).map(|(_, e)| e.clone()).collect()
}

pub async fn run(args: PromptArgs) -> Result<()> {
    match args.command.unwrap_or(PromptCommand::Show { json: args.json }) {
        PromptCommand::Show { json: as_json } => {
            let path = ensure_installed_prompt();
            if as_json || args.json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&json!({
                        "repo": HANZO_PROMPT_REPO,
                        "raw_url": HANZO_PROMPT_RAW_URL,
                        "skills_index": HANZO_SKILLS_INDEX_URL,
                        "mcp": HANZO_MCP_URL,
                        "path": path.as_ref().map(|p| p.display().to_string()),
                        "prompt": HANZO_PROMPT.trim(),
                    }))?
                );
            } else {
                print!("{HANZO_PROMPT}");
            }
            Ok(())
        }
        PromptCommand::Install { refresh } => {
            let path = installed_prompt_path()?;
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let content = if refresh {
                let client = reqwest::Client::builder()
                    .timeout(Duration::from_secs(10))
                    .build()?;
                match client.get(HANZO_PROMPT_RAW_URL).send().await {
                    Ok(resp) if resp.status().is_success() => {
                        let text = resp.text().await?;
                        if text.contains("# Hanzo Rules") {
                            text
                        } else {
                            HANZO_PROMPT.to_string()
                        }
                    }
                    _ => HANZO_PROMPT.to_string(),
                }
            } else {
                HANZO_PROMPT.to_string()
            };
            std::fs::write(&path, &content)?;
            if let Some(hanzo_dir) = path.parent() {
                sync_prompt_file(&hanzo_dir.join("claude").join("CLAUDE.md"), &content, false);
                sync_prompt_file(&hanzo_dir.join("dev").join("AGENTS.md"), &content, false);
            }
            println!(
                "{} installed Hanzo prompt -> {}",
                "✓".green(),
                path.display().to_string().cyan()
            );
            println!("  repo    {HANZO_PROMPT_REPO}");
            println!("  skills  {HANZO_SKILLS_INDEX_URL}");
            println!("  mcp     {HANZO_MCP_URL}");
            Ok(())
        }
        PromptCommand::Path => {
            let path = installed_prompt_path()?;
            println!("{}", path.display());
            Ok(())
        }
        PromptCommand::Skills { query, limit, json: as_json } => {
            let client = reqwest::Client::builder()
                .timeout(Duration::from_secs(10))
                .build()?;
            let body = client
                .get(HANZO_SKILLS_INDEX_URL)
                .send()
                .await
                .context("fetching hanzoskills.com/llms.txt")?
                .error_for_status()
                .context("hanzoskills.com/llms.txt returned non-200")?
                .text()
                .await?;
            let entries = parse_skills_index(&body);
            let q = query.join(" ");
            let matched = match_skills(&entries, &q, limit);
            if as_json {
                let items: Vec<_> = matched
                    .iter()
                    .map(|s| json!({ "slug": s.slug, "url": s.url, "description": s.description }))
                    .collect();
                println!(
                    "{}",
                    serde_json::to_string_pretty(&json!({
                        "index": HANZO_SKILLS_INDEX_URL,
                        "total": entries.len(),
                        "matched": items,
                    }))?
                );
            } else {
                for s in &matched {
                    println!("  ${:<24} {}  {}", s.slug.cyan(), s.url.dimmed(), s.description);
                }
            }
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_prompt_matches_canonical_hanzo_rules() {
        assert!(HANZO_PROMPT.starts_with("# Hanzo Rules"));
        for heading in [
            "## Voice",
            "## Workflow",
            "## Portability",
            "## Hanzo Setup",
            "## MCP — 19 Canonical Tools",
            "## Skills",
            "## Code",
            "## AI",
            "## API",
            "## IAM",
            "## Secrets",
            "## Source",
            "## Fleet",
            "## Data",
            "## Lux",
            "## Docs",
            "## Validation",
            "## Ship",
        ] {
            assert!(HANZO_PROMPT.contains(heading), "missing {heading}");
        }
        for tool in [
            "`fs`", "`exec`", "`code`", "`git`", "`fetch`", "`workspace`",
            "`browser`", "`computer`", "`ui`", "`kai`", "`think`", "`plan`",
            "`memory`", "`tasks`", "`tracker`", "`coding`", "`research`",
            "`skills`", "`hanzo`",
        ] {
            assert!(HANZO_PROMPT.contains(tool), "missing MCP tool {tool}");
        }
        assert!(HANZO_PROMPT.contains("https://hanzoskills.com"));
        assert!(HANZO_PROMPT.contains("npx -y hanzo mcp"));
        assert!(!HANZO_PROMPT.contains("Ilove"), "must not contain bootstrap credentials");
    }

    #[test]
    fn progressive_skill_discovery_parses_and_ranks_matches() {
        let sample = "\
# Hanzo Skills
- [hanzo-app](https://hanzoskills.com/hanzo-app.md): Unified web and desktop app.
- [hanzo-deploy](https://hanzoskills.com/hanzo-deploy.md): Deploy services via cd.hanzo.ai.
- [hanzo-base](https://hanzoskills.com/hanzo-base.md): Per-org and per-user Base database.
";
        let entries = parse_skills_index(sample);
        assert_eq!(entries.len(), 3);
        let hits = match_skills(&entries, "$hanzo-deploy", 5);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].slug, "hanzo-deploy");
    }
}
