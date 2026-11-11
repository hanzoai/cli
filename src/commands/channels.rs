//! Unified social & messaging channel and post management (`hanzo channels`, `hanzo connect`, `hanzo post`).
//!
//! Speaks natively to `/v1/channel`, `/v1/post`, and `/v1/autopost` on `api.hanzo.ai`
//! through [`Seam`], sharing the active Hanzo IAM identity (`--as <org>` or `HANZO_API_KEY`)
//! with every other cloud capability, with transparent fallback to `HANZO_POST_URL`
//! (`https://post.hanzo.ai`) if the cloud gateway has not yet rolled a route.

use anyhow::{bail, Result};
use clap::{Args, Subcommand};
use colored::*;
use hanzo_client::Method;
use serde_json::{json, Value};

use crate::commands::product::{envelope_error, Seam};
use crate::config::Config;
use crate::iam::paths::DEFAULT_BRAND;

const DEFAULT_POST_ORIGIN: &str = "https://post.hanzo.ai";

#[derive(Args, Debug, Clone)]
#[command(args_conflicts_with_subcommands = true)]
pub struct ChannelsArgs {
    #[command(subcommand)]
    pub command: Option<ChannelsCommand>,

    /// Filter channels by employee or brand group name (e.g. "Antje Worring", "Hanzo AI")
    #[arg(short, long)]
    pub group: Option<String>,

    /// Filter channels by provider (e.g. x, linkedin, instagram, facebook, discord, bluesky)
    #[arg(short, long)]
    pub provider: Option<String>,

    /// Output raw JSON
    #[arg(long)]
    pub json: bool,
}

#[derive(Subcommand, Debug, Clone)]
pub enum ChannelsCommand {
    /// List connected social & messaging channels
    #[command(visible_alias = "ls")]
    List {
        /// Filter by group name
        #[arg(short, long)]
        group: Option<String>,
        /// Filter by provider
        #[arg(short, long)]
        provider: Option<String>,
        /// Output raw JSON
        #[arg(long)]
        json: bool,
    },
    /// List channel groups (brands, teams, and employee groups)
    Groups {
        /// Output raw JSON
        #[arg(long)]
        json: bool,
    },
    /// Show supported social providers and OAuth readiness
    Providers {
        /// Output raw JSON
        #[arg(long)]
        json: bool,
    },
    /// Assign a channel to an employee or brand group
    #[command(alias = "set-group")]
    Group {
        /// Channel ID
        id: String,
        /// Group name (e.g. "Antje Worring" or "Hanzo AI"; pass "" to clear)
        group: String,
        /// Output raw JSON
        #[arg(long)]
        json: bool,
    },
    /// Fetch channel engagement & post analytics
    Analytics {
        /// Channel ID
        id: String,
        /// Lookback window in days (1..90)
        #[arg(long, default_value_t = 7)]
        days: u32,
        /// Output raw JSON
        #[arg(long)]
        json: bool,
    },
    /// Disconnect and remove a channel
    #[command(visible_alias = "rm")]
    Disconnect {
        /// Channel ID
        id: String,
    },
}

#[derive(Args, Debug, Clone)]
pub struct ConnectArgs {
    /// Provider to connect (x, linkedin, facebook, instagram, threads, tiktok, youtube, reddit, discord, slack, telegram, bluesky, mastodon)
    pub provider: String,

    /// Assign the newly connected channel to an employee or brand group (e.g. "Antje Worring")
    #[arg(short, long)]
    pub group: Option<String>,

    /// Post-OAuth redirect URL (defaults to https://post.hanzo.ai/launches)
    #[arg(long = "return")]
    pub return_url: Option<String>,

    /// Open the OAuth authorization URL in your default browser immediately
    #[arg(short, long)]
    pub open: bool,

    /// Output raw JSON
    #[arg(long)]
    pub json: bool,
}

#[derive(Args, Debug, Clone)]
#[command(args_conflicts_with_subcommands = true)]
pub struct PostArgs {
    #[command(subcommand)]
    pub command: Option<PostCommand>,

    /// Filter listed posts by status (draft, scheduled, published, failed)
    #[arg(short, long)]
    pub status: Option<String>,

    /// Filter listed posts by provider/channel (e.g. x, linkedin, bluesky)
    #[arg(long)]
    pub channel: Option<String>,

    /// Filter listed posts by employee or brand group
    #[arg(short, long)]
    pub group: Option<String>,

    /// Output raw JSON
    #[arg(long)]
    pub json: bool,
}

#[derive(Subcommand, Debug, Clone)]
pub enum PostCommand {
    /// List scheduled, published, and draft social posts
    #[command(visible_alias = "ls")]
    List {
        /// Filter by status (draft, scheduled, published, failed)
        #[arg(short, long)]
        status: Option<String>,
        /// Filter by provider/channel (x, linkedin, bluesky, discord, etc.)
        #[arg(long)]
        channel: Option<String>,
        /// Filter by employee or brand group
        #[arg(short, long)]
        group: Option<String>,
        /// Output raw JSON
        #[arg(long)]
        json: bool,
    },
    /// Create, draft, or schedule a post across channels or an employee/brand group
    Create {
        /// Post body text
        content: String,
        /// Target social provider(s) (repeatable: `--channel x --channel linkedin`)
        #[arg(long = "channel")]
        channels: Vec<String>,
        /// Target specific connected channel ID(s) (repeatable: `--account ch_1`)
        #[arg(short, long = "account")]
        accounts: Vec<String>,
        /// Fan out to all connected channels in an employee or brand group (e.g. "Hanzo AI", "Antje Worring")
        #[arg(short, long)]
        group: Option<String>,
        /// Lifecycle state: draft, scheduled, or now
        #[arg(long, default_value = "draft")]
        status: String,
        /// Unix timestamp (seconds) or RFC3339 datetime to publish at
        #[arg(long = "at")]
        schedule_at: Option<String>,
        /// Attach media URL(s) (repeatable: `-m https://...`)
        #[arg(short, long = "media")]
        media: Vec<String>,
        /// Output raw JSON
        #[arg(long)]
        json: bool,
    },
    /// Publish a post immediately across channels or a group (shorthand for `create --status now`)
    Now {
        /// Post body text
        content: String,
        /// Target social provider(s) (repeatable: `--channel x --channel linkedin`)
        #[arg(long = "channel")]
        channels: Vec<String>,
        /// Target specific connected channel ID(s)
        #[arg(short, long = "account")]
        accounts: Vec<String>,
        /// Fan out to all connected channels in an employee or brand group
        #[arg(short, long)]
        group: Option<String>,
        /// Attach media URL(s)
        #[arg(short, long = "media")]
        media: Vec<String>,
        /// Output raw JSON
        #[arg(long)]
        json: bool,
    },
    /// Inspect a single post by ID
    Get {
        /// Post ID
        id: String,
        /// Output raw JSON
        #[arg(long)]
        json: bool,
    },
    /// Trigger immediate publish of an existing draft or scheduled post
    Publish {
        /// Post ID
        id: String,
        /// Output raw JSON
        #[arg(long)]
        json: bool,
    },
    /// Fetch per-post engagement analytics
    Analytics {
        /// Post ID
        id: String,
        /// Lookback window in days (1..90)
        #[arg(long, default_value_t = 30)]
        days: u32,
        /// Output raw JSON
        #[arg(long)]
        json: bool,
    },
    /// Delete a post by ID
    #[command(visible_alias = "rm")]
    Delete {
        /// Post ID
        id: String,
    },
    /// Manage RSS / Atom / JSON Feed auto-publishing rules (`/v1/autopost`)
    Autopost {
        #[command(subcommand)]
        command: Option<AutopostCommand>,
        /// Output raw JSON
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand, Debug, Clone)]
pub enum AutopostCommand {
    /// List configured RSS auto-publishing feeds
    #[command(visible_alias = "ls")]
    List {
        /// Output raw JSON
        #[arg(long)]
        json: bool,
    },
    /// Create a new RSS auto-publishing rule
    Create {
        /// Feed title (e.g. "Hanzo Blog RSS")
        #[arg(short, long)]
        title: String,
        /// RSS 2.0 / Atom / JSON Feed URL (e.g. https://hanzo.blog/rss.xml)
        #[arg(short, long)]
        url: String,
        /// Target connected channel IDs (repeatable: `--account ch_1 --account ch_2`)
        #[arg(short, long = "account", required = true)]
        accounts: Vec<String>,
        /// Summarize feed entry via Zen AI before posting
        #[arg(long, default_value_t = true)]
        ai_summary: bool,
        /// Attach article OpenGraph image
        #[arg(long, default_value_t = true)]
        add_picture: bool,
        /// Output raw JSON
        #[arg(long)]
        json: bool,
    },
    /// Enable or pause an RSS auto-publishing rule
    Active {
        /// Autopost ID
        id: String,
        /// Set active (`true` or `false`)
        #[arg(long, default_value_t = true, action = clap::ArgAction::Set)]
        active: bool,
    },
    /// Delete an RSS auto-publishing rule
    #[command(visible_alias = "rm")]
    Delete {
        /// Autopost ID
        id: String,
    },
}

pub async fn run_channels(cfg: &mut Config, args: ChannelsArgs) -> Result<()> {
    match args.command {
        None => list_channels(cfg, args.group, args.provider, args.json).await,
        Some(ChannelsCommand::List { group, provider, json }) => {
            list_channels(cfg, group, provider, json).await
        }
        Some(ChannelsCommand::Groups { json }) => list_groups(cfg, json).await,
        Some(ChannelsCommand::Providers { json }) => list_providers(cfg, json).await,
        Some(ChannelsCommand::Group { id, group, json }) => {
            set_group(cfg, &id, &group, json).await
        }
        Some(ChannelsCommand::Analytics { id, days, json }) => {
            channel_analytics(cfg, &id, days, json).await
        }
        Some(ChannelsCommand::Disconnect { id }) => disconnect_channel(cfg, &id).await,
    }
}

pub async fn run_connect(cfg: &mut Config, args: ConnectArgs) -> Result<()> {
    let provider = args.provider.trim().to_lowercase();
    if provider.is_empty() {
        bail!("provider is required (e.g. `hanzo connect x --group \"Hanzo AI\" --open`)");
    }
    let mut body = json!({ "provider": provider });
    if let Some(g) = args.group.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        body["groupName"] = Value::String(g.to_string());
    }
    if let Some(r) = args.return_url.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        body["returnUrl"] = Value::String(r.to_string());
    }

    let data = request(cfg, Method::POST, "/v1/channel/connect", &[], Some(body)).await?;
    if args.json {
        println!("{}", serde_json::to_string_pretty(&data)?);
        return Ok(());
    }

    let url = data
        .get("authorizeUrl")
        .or_else(|| data.get("url"))
        .and_then(Value::as_str)
        .unwrap_or("");
    if url.is_empty() {
        println!("{}", serde_json::to_string_pretty(&data)?);
        return Ok(());
    }

    println!("{} Connect {} channel:", "→".cyan().bold(), provider.bold());
    if let Some(g) = data.get("groupName").and_then(Value::as_str).filter(|s| !s.is_empty()) {
        println!("  Group: {}", g.green());
    }
    println!("  Authorize URL: {}", url.underline());
    if args.open {
        if let Err(err) = webbrowser::open(url) {
            crate::warn(&format!("could not open browser automatically: {err}"));
        } else {
            println!("  {} Opened in default browser.", "✓".green());
        }
    } else {
        println!("  Tip: pass `--open` to launch directly in your browser.");
    }
    Ok(())
}

pub async fn run_post(cfg: &mut Config, args: PostArgs) -> Result<()> {
    match args.command {
        None => list_posts(cfg, args.status, args.channel, args.group, args.json).await,
        Some(PostCommand::List { status, channel, group, json }) => {
            list_posts(cfg, status, channel, group, json).await
        }
        Some(PostCommand::Create {
            content,
            channels,
            accounts,
            group,
            status,
            schedule_at,
            media,
            json,
        }) => {
            create_post(cfg, &content, &channels, &accounts, group.as_deref(), &status, schedule_at.as_deref(), &media, json).await
        }
        Some(PostCommand::Now { content, channels, accounts, group, media, json }) => {
            create_post(cfg, &content, &channels, &accounts, group.as_deref(), "now", None, &media, json).await
        }
        Some(PostCommand::Get { id, json }) => get_post(cfg, &id, json).await,
        Some(PostCommand::Publish { id, json }) => publish_post(cfg, &id, json).await,
        Some(PostCommand::Analytics { id, days, json }) => post_analytics(cfg, &id, days, json).await,
        Some(PostCommand::Delete { id }) => delete_post(cfg, &id).await,
        Some(PostCommand::Autopost { command, json }) => match command {
            None => list_autoposts(cfg, json).await,
            Some(AutopostCommand::List { json }) => list_autoposts(cfg, json).await,
            Some(AutopostCommand::Create { title, url, accounts, ai_summary, add_picture, json }) => {
                create_autopost(cfg, &title, &url, &accounts, ai_summary, add_picture, json).await
            }
            Some(AutopostCommand::Active { id, active }) => set_autopost_active(cfg, &id, active).await,
            Some(AutopostCommand::Delete { id }) => delete_autopost(cfg, &id).await,
        },
    }
}

async fn list_channels(
    cfg: &mut Config,
    group: Option<String>,
    provider: Option<String>,
    as_json: bool,
) -> Result<()> {
    let mut query = Vec::new();
    if let Some(g) = group.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        query.push(format!("group={g}"));
    }
    if let Some(p) = provider.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        query.push(format!("provider={p}"));
    }
    let data = request(cfg, Method::GET, "/v1/channel", &query, None).await?;
    if as_json {
        println!("{}", serde_json::to_string_pretty(&data)?);
        return Ok(());
    }

    let mut rows = data
        .as_array()
        .or_else(|| data.get("channels").and_then(Value::as_array))
        .cloned()
        .unwrap_or_default();
    if let Some(p) = provider.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        let p_lc = p.to_lowercase();
        rows.retain(|r| str_field(r, &["provider", "identifier"]).to_lowercase() == p_lc);
    }

    if rows.is_empty() {
        println!("No channels connected yet.");
        println!("Connect one with `hanzo connect <provider> --open` or at https://post.hanzo.ai");
        return Ok(());
    }

    println!(
        "{:<28} {:<16} {:<28} {:<20} {}",
        "ID".bold(),
        "PROVIDER".bold(),
        "HANDLE / NAME".bold(),
        "GROUP".bold(),
        "STATUS".bold()
    );
    for row in &rows {
        let id = str_field(row, &["id"]);
        let prov = str_field(row, &["provider", "identifier"]);
        let handle = str_field(row, &["handle", "name", "profile"]);
        let grp = row
            .get("groupName")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .or_else(|| {
                row.get("customer")
                    .and_then(|c| c.get("name"))
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
            })
            .unwrap_or("—");
        let refresh_needed = row.get("refreshNeeded").and_then(Value::as_bool).unwrap_or(false);
        let disabled = row.get("disabled").and_then(Value::as_bool).unwrap_or(false);
        let connected_flag = row.get("connected").and_then(Value::as_bool);
        let raw_status = str_field(row, &["status"]);
        let status_colored = if disabled {
            "disabled".dimmed().to_string()
        } else if refresh_needed || raw_status == "disconnected" || connected_flag == Some(false) {
            "reconnect".yellow().to_string()
        } else {
            "connected".green().to_string()
        };
        println!(
            "{:<28} {:<16} {:<28} {:<20} {}",
            trunc(&id, 27),
            trunc(&prov, 15),
            trunc(&handle, 27),
            trunc(grp, 19),
            status_colored
        );
    }
    println!("\n{} channel(s) listed.", rows.len());
    Ok(())
}

async fn list_groups(cfg: &mut Config, as_json: bool) -> Result<()> {
    let data = request(cfg, Method::GET, "/v1/channel/groups", &[], None).await?;
    if as_json {
        println!("{}", serde_json::to_string_pretty(&data)?);
        return Ok(());
    }
    let rows = data
        .as_array()
        .or_else(|| data.get("groups").and_then(Value::as_array))
        .cloned()
        .unwrap_or_default();
    if rows.is_empty() {
        println!("No channel groups defined yet.");
        return Ok(());
    }
    println!("{:<28} {:<10} {}", "GROUP".bold(), "CHANNELS".bold(), "ID / PROVIDERS".bold());
    for row in &rows {
        let name = str_field(row, &["name"]);
        let count = row
            .get("channels")
            .and_then(Value::as_i64)
            .map(|c| c.to_string())
            .unwrap_or_else(|| "—".to_string());
        let providers = row
            .get("providers")
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(", "))
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| str_field(row, &["id"]));
        println!("{:<28} {:<10} {}", trunc(&name, 27), count, providers);
    }
    Ok(())
}

async fn list_providers(cfg: &mut Config, as_json: bool) -> Result<()> {
    let data = request(cfg, Method::GET, "/v1/channel/providers", &[], None).await?;
    if as_json {
        println!("{}", serde_json::to_string_pretty(&data)?);
        return Ok(());
    }
    let rows = data
        .as_array()
        .or_else(|| data.get("providers").and_then(Value::as_array))
        .cloned()
        .unwrap_or_default();
    println!("{:<20} {:<14} {}", "PROVIDER".bold(), "READY".bold(), "NOTES".bold());
    for row in &rows {
        let prov = str_field(row, &["provider", "identifier"]);
        let ready = row
            .get("credentialsConfigured")
            .or_else(|| row.get("ready"))
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let missing = row
            .get("missingCredentials")
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(", "))
            .unwrap_or_default();
        let status = if ready { "yes".green().to_string() } else { "missing env".yellow().to_string() };
        println!("{:<20} {:<14} {}", prov, status, missing);
    }
    Ok(())
}

async fn set_group(cfg: &mut Config, id: &str, group: &str, as_json: bool) -> Result<()> {
    let path = format!("/v1/channel/{}/group", id.trim());
    let data = request(cfg, Method::PUT, &path, &[], Some(json!({ "name": group.trim() }))).await?;
    if as_json {
        println!("{}", serde_json::to_string_pretty(&data)?);
    } else {
        println!("{} Updated channel `{}` group -> `{}`", "✓".green(), id.trim(), group.trim());
    }
    Ok(())
}

async fn channel_analytics(cfg: &mut Config, id: &str, days: u32, as_json: bool) -> Result<()> {
    let path = format!("/v1/channel/{}/analytics", id.trim());
    let query = vec![format!("days={days}")];
    let data = request(cfg, Method::GET, &path, &query, None).await?;
    if as_json {
        println!("{}", serde_json::to_string_pretty(&data)?);
        return Ok(());
    }
    println!(
        "{} Analytics for {} ({}) — {}d window",
        "●".cyan(),
        str_field(&data, &["handle", "channelId"]).bold(),
        str_field(&data, &["provider"]),
        days
    );
    println!(
        "  Posts: {} total · {} published · {} scheduled · {} failed",
        data.get("posts").and_then(Value::as_i64).unwrap_or(0),
        data.get("published").and_then(Value::as_i64).unwrap_or(0),
        data.get("scheduled").and_then(Value::as_i64).unwrap_or(0),
        data.get("failed").and_then(Value::as_i64).unwrap_or(0),
    );
    if let Some(metrics) = data.get("metrics").and_then(Value::as_array) {
        for m in metrics {
            let label = str_field(m, &["label"]);
            let total: f64 = m
                .get("data")
                .and_then(Value::as_array)
                .map(|pts| {
                    pts.iter()
                        .filter_map(|p| p.get("total"))
                        .filter_map(|v| {
                            v.as_f64()
                                .or_else(|| v.as_str().and_then(|s| s.parse::<f64>().ok()))
                        })
                        .sum()
                })
                .unwrap_or(0.0);
            println!("  {}: {}", label, total);
        }
    }
    Ok(())
}

async fn disconnect_channel(cfg: &mut Config, id: &str) -> Result<()> {
    let path = format!("/v1/channel/{}", id.trim());
    let _ = request(cfg, Method::DELETE, &path, &[], None).await?;
    println!("{} Disconnected channel `{}`.", "✓".green(), id.trim());
    Ok(())
}

async fn list_posts(
    cfg: &mut Config,
    status: Option<String>,
    channel: Option<String>,
    group: Option<String>,
    as_json: bool,
) -> Result<()> {
    let now = chrono::Utc::now();
    let start_default = (now - chrono::Duration::days(30)).to_rfc3339();
    let end_default = (now + chrono::Duration::days(30)).to_rfc3339();
    let mut query = vec![
        format!("startDate={start_default}"),
        format!("endDate={end_default}"),
    ];
    if let Some(s) = status.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        query.push(format!("status={s}"));
    }
    if let Some(c) = channel.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        query.push(format!("channel={c}"));
    }
    if let Some(g) = group.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        query.push(format!("group={g}"));
    }
    let data = request(cfg, Method::GET, "/v1/post", &query, None).await?;
    if as_json {
        println!("{}", serde_json::to_string_pretty(&data)?);
        return Ok(());
    }

    let mut rows = data
        .as_array()
        .or_else(|| data.get("posts").and_then(Value::as_array))
        .cloned()
        .unwrap_or_default();

    if let Some(s) = status.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        let s_lc = s.to_lowercase();
        rows.retain(|r| post_status_str(r).to_lowercase().contains(&s_lc));
    }
    if let Some(c) = channel.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        let c_lc = c.to_lowercase();
        rows.retain(|r| post_channel_str(r).to_lowercase() == c_lc);
    }

    if rows.is_empty() {
        println!("No posts matched.");
        return Ok(());
    }

    println!(
        "{:<28} {:<14} {:<12} {:<22} {}",
        "ID".bold(),
        "CHANNEL".bold(),
        "STATUS".bold(),
        "SCHEDULE / DATE".bold(),
        "CONTENT".bold()
    );
    for row in &rows {
        let id = str_field(row, &["id"]);
        let chan = post_channel_str(row);
        let st = post_status_str(row);
        let st_colored = match st.as_str() {
            "published" | "PUBLISHED" => st.to_lowercase().green().to_string(),
            "scheduled" | "QUEUE" => "scheduled".cyan().to_string(),
            "failed" | "ERROR" => "failed".red().to_string(),
            _ => st.to_lowercase().dimmed().to_string(),
        };
        let when = post_when_str(row);
        let content = str_field(row, &["content"]).replace('\n', " ");
        println!(
            "{:<28} {:<14} {:<12} {:<22} {}",
            trunc(&id, 27),
            trunc(&chan, 13),
            st_colored,
            trunc(&when, 21),
            trunc(&content, 48)
        );
    }
    println!("\n{} post(s) listed.", rows.len());
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn create_post(
    cfg: &mut Config,
    content: &str,
    channels: &[String],
    accounts: &[String],
    group: Option<&str>,
    status: &str,
    schedule_at: Option<&str>,
    media: &[String],
    as_json: bool,
) -> Result<()> {
    let (resolved_ts, resolved_date) = if let Some(raw_at) = schedule_at.map(str::trim).filter(|s| !s.is_empty()) {
        let ts = parse_schedule(raw_at)?;
        let iso = chrono::DateTime::<chrono::Utc>::from_timestamp(ts, 0)
            .unwrap_or_else(chrono::Utc::now)
            .to_rfc3339();
        (Some(ts), iso)
    } else {
        (None, chrono::Utc::now().to_rfc3339())
    };
    let post_type = match status.trim().to_lowercase().as_str() {
        "now" | "publish" | "published" => "now",
        "draft" => "draft",
        _ => "schedule",
    };

    // Resolve target channel IDs (supporting integration IDs, provider slugs, or group names)
    let mut target_ids: Vec<String> = Vec::new();
    for a in accounts {
        let trimmed = a.trim();
        if !trimmed.is_empty() && !target_ids.iter().any(|x| x == trimmed) {
            target_ids.push(trimmed.to_string());
        }
    }
    let needs_lookup = channels.iter().any(|c| {
        let t = c.trim();
        !t.starts_with("cm") && !t.starts_with("int-")
    }) || (channels.is_empty() && accounts.is_empty() && group.is_some());

    if needs_lookup {
        if let Ok(list_val) = request(cfg, Method::GET, "/v1/channel", &[], None).await {
            let rows = list_val
                .as_array()
                .or_else(|| list_val.get("channels").and_then(Value::as_array))
                .cloned()
                .unwrap_or_default();
            for c in channels {
                let want = c.trim().to_lowercase();
                for r in &rows {
                    let id = str_field(r, &["id"]);
                    let prov = str_field(r, &["identifier", "provider"]).to_lowercase();
                    if id.to_lowercase() == want || prov == want {
                        if !target_ids.iter().any(|x| x == &id) {
                            target_ids.push(id);
                        }
                    }
                }
            }
            if let Some(g) = group.map(str::trim).filter(|s| !s.is_empty()) {
                let want_g = g.to_lowercase();
                for r in &rows {
                    let id = str_field(r, &["id"]);
                    let grp = r
                        .get("customer")
                        .and_then(|v| v.as_str().or_else(|| v.get("name").and_then(Value::as_str)))
                        .unwrap_or_default()
                        .to_lowercase();
                    let disabled = r.get("disabled").and_then(Value::as_bool).unwrap_or(false);
                    let refresh = r.get("refreshNeeded").and_then(Value::as_bool).unwrap_or(false);
                    if grp == want_g && !disabled && !refresh && !target_ids.iter().any(|x| x == &id) {
                        target_ids.push(id);
                    }
                }
            }
        }
    } else {
        for c in channels {
            let trimmed = c.trim();
            if !trimmed.is_empty() && !target_ids.iter().any(|x| x == trimmed) {
                target_ids.push(trimmed.to_string());
            }
        }
    }

    let images: Vec<Value> = media
        .iter()
        .map(|m| m.trim())
        .filter(|m| !m.is_empty())
        .map(|u| json!({ "id": u, "path": u }))
        .collect();

    let posts_items: Vec<Value> = target_ids
        .iter()
        .map(|id| {
            json!({
                "integration": { "id": id },
                "value": [{ "content": content, "image": images }],
                "settings": {
                    "who_can_reply_post": "everyone",
                    "channel": "announcements",
                    "title": content.lines().next().unwrap_or("Hanzo AI Update").chars().take(80).collect::<String>()
                }
            })
        })
        .collect();

    let mut body = json!({
        "type": post_type,
        "date": resolved_date,
        "shortLink": false,
        "tags": [],
        "posts": posts_items,
        "content": content,
        "status": status,
        "creationMethod": "CLI",
    });
    if channels.len() == 1 {
        body["channel"] = json!(channels[0]);
    } else if !channels.is_empty() {
        body["channels"] = json!(channels);
    }
    if accounts.len() == 1 {
        body["accountId"] = json!(accounts[0]);
    } else if !accounts.is_empty() {
        body["accountIds"] = json!(accounts);
    }
    if let Some(g) = group.map(str::trim).filter(|s| !s.is_empty()) {
        body["group"] = json!(g);
    }
    if let Some(ts) = resolved_ts {
        body["scheduleAt"] = json!(ts);
    }
    if !media.is_empty() {
        body["media"] = json!(media);
    }

    let data = request(cfg, Method::POST, "/v1/post", &[], Some(body)).await?;
    if as_json {
        println!("{}", serde_json::to_string_pretty(&data)?);
        return Ok(());
    }
    let id = if let Some(arr) = data.as_array() {
        arr.iter()
            .map(|r| str_field(r, &["id", "postId"]))
            .filter(|s| !s.is_empty() && s != "—")
            .collect::<Vec<_>>()
            .join(", ")
    } else {
        str_field(&data, &["id", "postId"])
    };
    println!("{} Post submitted (`{id}`).", "✓".green());
    if let Some(url) = data.get("releaseUrl").or_else(|| data.get("releaseURL")).and_then(Value::as_str).filter(|s| !s.is_empty()) {
        println!("  URL: {}", url.underline());
    }
    Ok(())
}

async fn get_post(cfg: &mut Config, id: &str, as_json: bool) -> Result<()> {
    let path = format!("/v1/post/{}", id.trim());
    let data = request(cfg, Method::GET, &path, &[], None).await?;
    if as_json {
        println!("{}", serde_json::to_string_pretty(&data)?);
        return Ok(());
    }
    println!("{}", serde_json::to_string_pretty(&data)?);
    Ok(())
}

async fn publish_post(cfg: &mut Config, id: &str, as_json: bool) -> Result<()> {
    let path = format!("/v1/post/{}/publish", id.trim());
    let data = request(cfg, Method::POST, &path, &[], Some(json!({}))).await?;
    if as_json {
        println!("{}", serde_json::to_string_pretty(&data)?);
    } else {
        println!("{} Published post `{}`.", "✓".green(), id.trim());
    }
    Ok(())
}

async fn post_analytics(cfg: &mut Config, id: &str, days: u32, as_json: bool) -> Result<()> {
    let path = format!("/v1/post/{}/analytics", id.trim());
    let query = vec![format!("days={days}")];
    let data = request(cfg, Method::GET, &path, &query, None).await?;
    println!("{}", serde_json::to_string_pretty(&data)?);
    let _ = as_json;
    Ok(())
}

async fn delete_post(cfg: &mut Config, id: &str) -> Result<()> {
    let path = format!("/v1/post/{}", id.trim());
    let _ = request(cfg, Method::DELETE, &path, &[], None).await?;
    println!("{} Deleted post `{}`.", "✓".green(), id.trim());
    Ok(())
}

async fn list_autoposts(cfg: &mut Config, as_json: bool) -> Result<()> {
    let data = request(cfg, Method::GET, "/v1/autopost", &[], None).await?;
    if as_json {
        println!("{}", serde_json::to_string_pretty(&data)?);
        return Ok(());
    }
    let rows = data
        .as_array()
        .or_else(|| data.get("autoposts").and_then(Value::as_array))
        .cloned()
        .unwrap_or_default();
    if rows.is_empty() {
        println!("No RSS auto-publishing rules configured.");
        return Ok(());
    }
    println!("{:<38} {:<10} {:<26} {}", "ID".bold(), "ACTIVE".bold(), "TITLE".bold(), "FEED URL".bold());
    for row in &rows {
        let id = str_field(row, &["id"]);
        let active = row.get("active").and_then(Value::as_bool).unwrap_or(false);
        let title = str_field(row, &["title"]);
        let url = str_field(row, &["url"]);
        let active_str = if active { "active".green().to_string() } else { "paused".yellow().to_string() };
        println!("{:<38} {:<10} {:<26} {}", id, active_str, trunc(&title, 25), url);
    }
    Ok(())
}

async fn create_autopost(
    cfg: &mut Config,
    title: &str,
    url: &str,
    accounts: &[String],
    ai_summary: bool,
    add_picture: bool,
    as_json: bool,
) -> Result<()> {
    let integrations: Vec<Value> = accounts.iter().map(|id| json!({ "id": id })).collect();
    let body = json!({
        "title": title,
        "url": url,
        "active": true,
        "onSlot": true,
        "syncLast": false,
        "generateContent": ai_summary,
        "addPicture": add_picture,
        "content": "",
        "integrations": integrations,
    });
    let data = request(cfg, Method::POST, "/v1/autopost", &[], Some(body)).await?;
    if as_json {
        println!("{}", serde_json::to_string_pretty(&data)?);
    } else {
        println!("{} Created RSS autopost rule `{}` -> {}", "✓".green(), title.bold(), url);
    }
    Ok(())
}

async fn set_autopost_active(cfg: &mut Config, id: &str, active: bool) -> Result<()> {
    let path = format!("/v1/autopost/{}/active", id.trim());
    let _ = request(cfg, Method::POST, &path, &[], Some(json!({ "active": active }))).await?;
    println!("{} Updated autopost `{}` active={active}.", "✓".green(), id.trim());
    Ok(())
}

async fn delete_autopost(cfg: &mut Config, id: &str) -> Result<()> {
    let path = format!("/v1/autopost/{}", id.trim());
    let _ = request(cfg, Method::DELETE, &path, &[], None).await?;
    println!("{} Deleted autopost `{}`.", "✓".green(), id.trim());
    Ok(())
}

fn parse_schedule(raw: &str) -> Result<i64> {
    if let Ok(ts) = raw.parse::<i64>() {
        return Ok(ts);
    }
    let dt = chrono::DateTime::parse_from_rfc3339(raw)
        .map_err(|e| anyhow::anyhow!("invalid `--at` value {raw:?} (pass unix seconds or RFC3339): {e}"))?;
    Ok(dt.timestamp())
}

fn post_origin() -> String {
    std::env::var("HANZO_POST_URL")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| DEFAULT_POST_ORIGIN.to_string())
}

fn is_unrouted_reply(status: u16, body: &Value) -> bool {
    status == 404
        || matches!(body, Value::String(s) if s.trim() == "App is running!" || s.trim().eq_ignore_ascii_case("not found"))
}

async fn request(
    cfg: &mut Config,
    method: Method,
    path: &str,
    query: &[String],
    body: Option<Value>,
) -> Result<Value> {
    let seam = Seam::open(cfg, DEFAULT_BRAND).await?;
    let mut reply = if let Ok(explicit) = std::env::var("HANZO_POST_URL") {
        let trimmed = explicit.trim();
        if !trimmed.is_empty() {
            seam.send_at(trimmed, method.clone(), path, query, body.clone()).await?
        } else {
            seam.send(method.clone(), path, query, body.clone()).await?
        }
    } else {
        seam.send(method.clone(), path, query, body.clone()).await?
    };

    if is_unrouted_reply(reply.status, &reply.body)
        && seam.is_http_origin()
        && std::env::var_os("HANZO_POST_URL").is_none()
    {
        let origin = post_origin();
        if let Ok(fallback_reply) = seam
            .send_at(&origin, method.clone(), path, query, body.clone())
            .await
        {
            if !is_unrouted_reply(fallback_reply.status, &fallback_reply.body) {
                reply = fallback_reply;
            } else if path.starts_with("/v1/") {
                let api_prefixed = format!("/api{path}");
                if let Ok(prefixed_reply) = seam
                    .send_at(&origin, method, &api_prefixed, query, body)
                    .await
                {
                    if !is_unrouted_reply(prefixed_reply.status, &prefixed_reply.body) {
                        reply = prefixed_reply;
                    }
                }
            }
        }
    }

    if (200..300).contains(&reply.status) {
        if let Some(msg) = envelope_error(&reply.body) {
            bail!("{path}: {msg}");
        }
        return Ok(reply.body.get("data").cloned().unwrap_or(reply.body));
    }
    let shown = match &reply.body {
        Value::Null => String::new(),
        Value::String(s) => s.trim().to_string(),
        v => v.to_string(),
    };
    if reply.status == 403 {
        if let Some(hint) = seam.hint {
            bail!("{path} -> {}: {shown}{hint}", reply.status);
        }
    }
    bail!("{path} -> {}: {shown}", reply.status)
}

fn post_channel_str(row: &Value) -> String {
    if let Some(c) = row.get("channel").and_then(Value::as_str).filter(|s| !s.is_empty()) {
        return c.to_string();
    }
    if let Some(intg) = row.get("integration") {
        let prov = str_field(intg, &["providerIdentifier", "identifier"]);
        let name = str_field(intg, &["name"]);
        if prov != "—" && name != "—" {
            return format!("{prov}:{name}");
        }
        if prov != "—" {
            return prov;
        }
    }
    "—".to_string()
}

fn post_status_str(row: &Value) -> String {
    let s = str_field(row, &["status", "state"]);
    match s.as_str() {
        "QUEUE" => "scheduled".to_string(),
        "PUBLISHED" => "published".to_string(),
        "DRAFT" => "draft".to_string(),
        "ERROR" => "failed".to_string(),
        other => other.to_string(),
    }
}

fn post_when_str(row: &Value) -> String {
    if let Some(s) = row.get("publishDate").and_then(Value::as_str).filter(|s| !s.is_empty()) {
        return s.chars().take(19).collect();
    }
    if let Some(ts) = row.get("scheduleAt").and_then(Value::as_i64).filter(|&t| t > 0) {
        if let Some(dt) = chrono::DateTime::from_timestamp(ts, 0) {
            return dt.format("%Y-%m-%d %H:%M UTC").to_string();
        }
    }
    if let Some(ts) = row.get("createdAt").and_then(Value::as_i64).filter(|&t| t > 0) {
        if let Some(dt) = chrono::DateTime::from_timestamp(ts, 0) {
            return dt.format("%Y-%m-%d %H:%M UTC").to_string();
        }
    }
    "—".to_string()
}

fn str_field(row: &Value, keys: &[&str]) -> String {
    for k in keys {
        if let Some(s) = row.get(*k).and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty()) {
            return s.to_string();
        }
    }
    "—".to_string()
}

fn trunc(s: &str, max: usize) -> String {
    let count = s.chars().count();
    if count <= max {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
        out.push('…');
        out
    }
}
