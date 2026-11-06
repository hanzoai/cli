//! Cloud execution uses the Go coding service, not the local session streamer.
use anyhow::{bail, Context, Result};
use hanzo_client::{Method, Reply};
use serde_json::{json, Value};
use std::{process::Command, time::Duration};

use super::product::{envelope_error, Seam};
use crate::config::Config;
use hanzo_gen::backend::{self, Selection};

/// Local agent arguments remain Gen's; cloud execution belongs to this CLI.
#[derive(clap::Args, Clone)]
#[command(mut_arg("cloud", |a| a.long("metered").help("Use Hanzo's metered models for a local coding session")))]
pub struct SessionArgs {
    #[command(flatten)]
    pub local: hanzo_gen::Args,
    #[command(flatten)]
    pub remote: CloudArgs,
}

impl std::ops::Deref for SessionArgs {
    type Target = hanzo_gen::Args;
    fn deref(&self) -> &Self::Target {
        &self.local
    }
}
impl std::ops::DerefMut for SessionArgs {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.local
    }
}

impl SessionArgs {
    pub fn dev(mut self, command: &str) -> Result<Self> {
        self.local = self.local.dev(command)?;
        Ok(self)
    }
    pub fn into_options(self, root: hanzo_gen::Root) -> Result<hanzo_gen::Options> {
        self.local.into_options(root)
    }
}

#[derive(clap::Args, Clone)]
pub struct CloudArgs {
    /// Run on a Hanzo cloud sandbox; without a task, list cloud coding sessions.
    #[arg(long = "cloud", conflicts_with_all = ["cloud", "link", "no_link", "no_mcp", "project_mcp", "ask", "no_sandbox", "runtime", "gpus", "theme", "passthrough"])]
    pub cloud_execution: bool,
    /// Cloud task. With a positional session id, continue that session's work.
    #[arg(short = 'p', long = "prompt", requires = "cloud_execution")]
    cloud_prompt: Option<String>,
    /// Repository (GitHub URL or Hanzo repository); defaults to origin.
    #[arg(long, requires = "cloud_execution")]
    repo: Option<String>,
    /// Cloud base branch; defaults to the current branch when using origin.
    #[arg(long, requires = "cloud_execution")]
    base: Option<String>,
    /// What the cloud run may do: build, plan, or setup.
    #[arg(long, requires = "cloud_execution", value_parser = ["build", "plan", "setup"])]
    mode: Option<String>,
    /// Follow cloud progress until the run finishes (Ctrl-C only detaches).
    #[arg(long, requires = "cloud_execution")]
    watch: bool,
    /// Fetch a cloud run's branch into this clean repository.
    #[arg(long, value_name = "SESSION_ID", conflicts_with_all = ["cloud_execution", "resume", "positional", "tail"])]
    pub teleport: Option<String>,
}

pub(crate) async fn run(cfg: &mut Config, input: SessionArgs, desktop: bool) -> Result<()> {
    let SessionArgs {
        local: args,
        remote,
    } = input;
    let seam = Seam::open(cfg, &args.brand).await?;
    if let Some(id) = &remote.teleport {
        return checkout(&seam, id).await;
    }
    let named = args.named_backend();
    let (backend, task, previous) = if let Some(prompt) = remote.cloud_prompt {
        if args.tail.is_some() || (args.resume.is_some() && args.positional.is_some()) {
            bail!("name the previous session once: hanzo -p 'task' --cloud SESSION_ID");
        }
        let (backend, _) = backend::select(Selection {
            named,
            positional: None,
            tail: None,
            configured: None,
        })?;
        (backend, Some(prompt), args.resume.or(args.positional))
    } else {
        let (backend, task) = backend::select(Selection {
            named,
            positional: args.positional,
            tail: args.tail,
            configured: None,
        })?;
        (backend, task, args.resume)
    };
    let Some(task) = task else {
        if let Some(id) = previous {
            return watch(&seam, &id, remote.watch).await;
        }
        let page = call(&seam, Method::GET, "/v1/agent/sessions?kind=coding", None).await?;
        let sessions = page
            .get("sessions")
            .and_then(Value::as_array)
            .context("cloud session list has no sessions array")?;
        if sessions.is_empty() {
            println!("No cloud coding sessions.");
        }
        for session in sessions {
            println!(
                "{}  {:8}  {}",
                field(session, "id"),
                field(session, "status"),
                field(session, "title")
            );
        }
        return Ok(());
    };
    if task.trim().is_empty() {
        bail!("a cloud task cannot be empty");
    }
    let mut body = json!({"prompt": task, "tool": backend.as_str(), "desktop": desktop});
    if let Some(id) = previous {
        let prior = detail(&seam, &id).await?;
        if !finished(field(&prior, "status")) && field(&prior, "status") != "paused" {
            bail!(
                "session {id} is still running; wait for it to finish before starting a follow-up"
            );
        }
        if field(&prior, "branch").is_empty() {
            bail!(
                "this run has no pushed branch to continue; start a new --cloud task with --repo"
            );
        }
        let repo = match remote.repo {
            Some(repo) => repo,
            None => {
                let origin = repository(
                    &git(&["remote", "get-url", "origin"])
                        .context("supply --repo to identify the previous run's repository host")?,
                )?;
                if !same_repo(&origin, field(&prior, "repo")) {
                    bail!("this checkout does not match the previous run; supply --repo");
                }
                origin
            }
        };
        body["repo"] = json!(repo);
        body["after"] = json!(id);
    } else if let Some(repo) = remote.repo {
        body["repo"] = json!(repo);
    } else {
        body["repo"] = json!(repository(
            &git(&["remote", "get-url", "origin"])
                .context("no origin remote; supply --repo with a cloud repository")?
        )?);
        if !git(&["status", "--porcelain"])?.is_empty() {
            crate::warn("uncommitted files stay local; the cloud clones the pushed branch");
        }
        let branch = git(&["symbolic-ref", "--quiet", "--short", "HEAD"])
            .context("detached HEAD; supply --repo and --base")?;
        if let Ok(ahead) = git(&[
            "rev-list",
            "--count",
            &format!("refs/remotes/origin/{branch}..HEAD"),
        ]) {
            if ahead != "0" {
                bail!("push your local commits before starting a cloud session");
            }
        } else {
            bail!("current branch has no upstream; push it with `git push -u origin HEAD` first, or name --repo and --base explicitly");
        }
        body["base"] = json!(branch);
    }
    for (key, value) in [
        ("base", remote.base),
        ("mode", remote.mode),
        ("model", args.model),
    ] {
        if let Some(value) = value {
            body[key] = json!(value);
        }
    }
    let accepted = call(&seam, Method::POST, "/v1/agent/coding", Some(body)).await?;
    let id = field(&accepted, "sessionId");
    session_id(id)?;
    println!(
        "Cloud coding session: {id}\nRepository: {}\nhttps://hanzo.bot/sessions/{id}",
        field(&accepted, "repo")
    );
    if !field(&accepted, "branch").is_empty() {
        println!(
            "Branch: {}\nFetch branch: hanzo --teleport {id}",
            field(&accepted, "branch")
        );
    }
    println!("Runs independently of this terminal.\nFollow: hanzo --cloud --resume {id} --watch");
    if remote.watch {
        watch(&seam, id, true).await?;
    }
    Ok(())
}

fn decode(Reply { status, body }: Reply) -> Result<Value> {
    if !(200..300).contains(&status) {
        bail!("cloud coding: HTTP {status}: {body}");
    }
    // A fetched session's `status: error` describes the run, not the GET.
    // Keep its events so watch can print the actual failure before exiting.
    let is_session = body.get("id").and_then(Value::as_str).is_some()
        && body.get("kind").and_then(Value::as_str) == Some("coding");
    if !is_session {
        if let Some(error) = envelope_error(&body) {
            bail!("cloud coding: {error}");
        }
    }
    // A policy approval is pending, not an accepted execution.
    if body.get("approval").is_some() || body.get("status").and_then(Value::as_str) == Some("held")
    {
        bail!("cloud request is awaiting approval: {body}");
    }
    Ok(body.get("data").cloned().unwrap_or(body))
}

async fn call(seam: &Seam, method: Method, path: &str, body: Option<Value>) -> Result<Value> {
    decode(seam.send(method, path, &[], body).await?)
}

fn session_id(id: &str) -> Result<()> {
    if id.is_empty()
        || !id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    {
        bail!("invalid cloud session id");
    }
    Ok(())
}

async fn detail(seam: &Seam, id: &str) -> Result<Value> {
    session_id(id)?;
    let value = call(seam, Method::GET, &format!("/v1/agent/sessions/{id}"), None).await?;
    if field(&value, "kind") != "coding" {
        bail!("{id} is not a cloud coding session");
    }
    Ok(value)
}

fn field<'a>(value: &'a Value, key: &str) -> &'a str {
    value.get(key).and_then(Value::as_str).unwrap_or("")
}

fn finished(status: &str) -> bool {
    matches!(status, "done" | "error" | "stopped")
}

fn same_repo(origin: &str, remote: &str) -> bool {
    origin == remote || origin.strip_prefix("github.com/") == Some(remote)
}

async fn watch(seam: &Seam, id: &str, follow: bool) -> Result<()> {
    let mut last = 0;
    let mut status = String::new();
    loop {
        let info = detail(seam, id).await?;
        if let Some(events) = info.get("recentEvents").and_then(Value::as_array) {
            for event in events {
                let seq = event.get("seq").and_then(Value::as_i64).unwrap_or(0);
                if seq > last {
                    if seq > last + 1 {
                        println!("Earlier events omitted; see the full session in the browser.");
                    }
                    print_event(event);
                    last = seq;
                }
            }
        }
        let current = field(&info, "status");
        if current != status {
            println!("Session {id}: {current}");
            status = current.into();
        }
        if !field(&info, "pr").is_empty() {
            println!("Pull request: {}", field(&info, "pr"));
        }
        if current == "error" {
            bail!("cloud coding session {id} failed");
        }
        if !follow || finished(current) || current == "paused" {
            return Ok(());
        }
        tokio::select! {
            _ = tokio::time::sleep(Duration::from_secs(2)) => {},
            result = tokio::signal::ctrl_c() => {
                result?;
                println!("Detached; cloud session {id} continues running.");
                return Ok(());
            }
        }
    }
}

fn print_event(event: &Value) {
    let payload = &event["payload"];
    for key in ["plan", "error", "message", "text"] {
        if let Some(text) = payload.get(key).and_then(Value::as_str) {
            if !text.trim().is_empty() {
                println!("{text}");
            }
            return;
        }
    }
    if field(payload, "type") == "item.completed" {
        let item = &payload["item"];
        for key in ["text", "aggregated_output"] {
            if let Some(text) = item.get(key).and_then(Value::as_str) {
                if !text.trim().is_empty() {
                    println!("{text}");
                }
                return;
            }
        }
    }
    if field(payload, "type") == "item.started" {
        let command = field(&payload["item"], "command");
        if !command.is_empty() {
            println!("$ {command}");
        }
    }
}

fn git(args: &[&str]) -> Result<String> {
    let output = Command::new("git")
        .args(args)
        .output()
        .context("running git")?;
    if !output.status.success() {
        bail!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(String::from_utf8(output.stdout)?.trim().to_owned())
}

/// Normalize only known hosting addresses; never send embedded credentials.
fn repository(remote: &str) -> Result<String> {
    let value = if let Some(path) = remote.strip_prefix("git@github.com:") {
        format!("github.com/{path}")
    } else if let Some(path) = remote.strip_prefix("git@git.hanzo.ai:") {
        path.to_owned()
    } else {
        let url = reqwest::Url::parse(remote)
            .context("origin must be on GitHub or git.hanzo.ai; use --repo")?;
        if !url.username().is_empty() && !(url.scheme() == "ssh" && url.username() == "git")
            || url.password().is_some()
        {
            bail!("origin contains credentials; use --repo with a credential-free repository name");
        }
        match url.host_str() {
            Some("github.com") => format!("github.com{}", url.path()),
            Some("git.hanzo.ai") => url.path().trim_start_matches('/').to_owned(),
            _ => bail!(
                "origin is not on GitHub or git.hanzo.ai; use --repo with a supported repository"
            ),
        }
    };
    let value = value.trim_end_matches('/').trim_end_matches(".git");
    let path = value.strip_prefix("github.com/").unwrap_or(value);
    let parts: Vec<_> = path.split('/').collect();
    if parts.len() != 2
        || parts.iter().any(|s| {
            s.is_empty()
                || *s == "."
                || *s == ".."
                || !s
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
        })
    {
        bail!("origin must name one owner/repository");
    }
    Ok(value.to_owned())
}

async fn checkout(seam: &Seam, id: &str) -> Result<()> {
    let info = detail(seam, id).await?;
    if !finished(field(&info, "status")) && field(&info, "status") != "paused" {
        bail!("wait for the cloud session to finish or pause before fetching its work");
    }
    let origin = repository(&git(&["remote", "get-url", "origin"])?)?;
    let remote_repo = field(&info, "repo");
    if !same_repo(&origin, remote_repo) {
        bail!("this checkout's origin does not match the cloud repository {remote_repo}");
    }
    if !git(&["status", "--porcelain"])?.is_empty() {
        bail!("working tree is not clean; commit or stash before --teleport");
    }
    let branch = field(&info, "branch");
    if branch.is_empty() || branch.starts_with('-') {
        bail!("session has no valid branch to fetch");
    }
    git(&["check-ref-format", "--branch", branch])?;
    git(&["fetch", "origin", &format!("refs/heads/{branch}")])?;
    git(&["switch", "--create", &format!("cloud/{id}"), "FETCH_HEAD"])?;
    println!("Checked out cloud/{id}.\nTranscript: https://hanzo.bot/sessions/{id}\nContinue locally: hanzo");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn origins_are_normalized_without_credentials() {
        for input in [
            "git@github.com:acme/app.git",
            "https://github.com/acme/app.git",
            "ssh://git@github.com/acme/app.git",
        ] {
            assert_eq!(repository(input).unwrap(), "github.com/acme/app");
        }
        assert_eq!(
            repository("git@git.hanzo.ai:acme/app.git").unwrap(),
            "acme/app"
        );
        for input in [
            "https://secret@github.com/a/b",
            "https://github.com.evil/a/b",
            "/tmp/repo",
            "git@github.com:../app",
            "https://github.com/a/b/tree/main",
        ] {
            assert!(repository(input).is_err(), "{input}");
        }
    }
    #[test]
    fn errors_and_approvals_are_not_success() {
        assert!(decode(Reply {
            status: 200,
            body: json!({"id":"sess_failed", "kind":"coding", "status":"error", "recentEvents":[]}),
        })
        .is_ok());
        assert!(decode(Reply {
            status: 503,
            body: json!({"error":"capacity"})
        })
        .is_err());
        assert!(decode(Reply {
            status: 202,
            body: json!({"status":"held", "approval": {"id":"a"}})
        })
        .is_err());
        assert_eq!(
            decode(Reply {
                status: 202,
                body: json!({"sessionId":"sess_x"})
            })
            .unwrap()["sessionId"],
            "sess_x"
        );
        for id in ["", "../x", "s?other=x", "s/x"] {
            assert!(session_id(id).is_err());
        }
    }
}
