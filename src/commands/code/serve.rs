//! `hanzo code --serve` — run THIS machine as a run-target daemon (#48 half B).
//!
//! A signed-in machine registers itself as a run-target, mints a machine claim
//! key, then LONG-POLLS cloud for coding runs routed to it. Each claimed run is
//! executed HERE — clone → agent → commit → push — and streamed back into the run
//! its dispatcher already opened, so a routed run shows in mission-control exactly
//! like a local `hanzo code` run. The terminal result is reported so the durable
//! owner (cloud's RoutedRunWorkflow) can complete.
//!
//! SECURITY BOUNDARY. Claim + report carry TWO independent proofs: the hanzo.id
//! bearer (the org, derived server-side from the JWT `owner` — the CLI never sends
//! an org) AND the machine claim key (X-Target-Key), so this daemon can only ever
//! claim runs addressed to THIS target in THIS org. Every credential — the bearer,
//! the claim key, and the git token — rides in a header or the child's ENV, NEVER
//! argv or a log line.
//!
//! The loop is generic over a [`Claimer`] (the cloud transport) and an [`Executor`]
//! (the run body) so the security-critical control flow — claim → execute → report,
//! re-poll on no-work, report-error on a failed run — is unit-tested with fakes;
//! the live agent + git subprocesses are the one unproven seam, exactly as the
//! keystone's own runner is.

use anyhow::{anyhow, Context as _, Result};
use base64::Engine as _;
use reqwest::{Client, Method};
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use super::backend::{resolve, Backend, BackendKind, Mode, Spec};
use super::context::{self, Machine};
use super::session::SessionClient;
use super::target::{Register, TargetClient};
use crate::commands::network;
use crate::config::Config;
use crate::iam::store;

/// The claim-key header — the machine capability, distinct from the org bearer.
const CLAIM_KEY_HEADER: &str = "X-Target-Key";

/// One run claimed from cloud, addressed to this machine. It carries NO
/// credential — the machine authenticates git + model routing with its own.
#[derive(Debug, Clone, Deserialize)]
pub struct ClaimedRun {
    #[serde(rename = "sessionId")]
    pub session_id: String,
    pub repo: String,
    #[serde(default)]
    pub base: String,
    pub branch: String,
    pub prompt: String,
    #[serde(rename = "cloneUrl")]
    pub clone_url: String,
    #[serde(rename = "timeoutSeconds", default)]
    pub timeout_seconds: u64,
}

/// The agent-run budget for a claimed run: its own timeout, or a sane default.
fn run_budget(seconds: u64) -> Duration {
    Duration::from_secs(if seconds > 0 { seconds } else { 1200 })
}

/// The terminal result the machine reports back for a routed run.
#[derive(Debug, Clone, Default, Serialize, PartialEq)]
pub struct RunReport {
    pub ok: bool,
    pub changed: bool,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub branch: String,
    #[serde(rename = "commitSha", skip_serializing_if = "String::is_empty")]
    pub commit_sha: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub diffstat: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub error: String,
}

impl RunReport {
    fn failed(msg: impl Into<String>) -> RunReport {
        RunReport { ok: false, error: msg.into(), ..Default::default() }
    }
}

/// The cloud transport seam: claim the next routed run, report a terminal result.
#[allow(async_fn_in_trait)]
pub trait Claimer {
    /// Long-poll for the next run addressed to this machine. `Ok(None)` is a clean
    /// "no work right now" (re-poll); an error is a transient transport failure.
    async fn claim(&self) -> Result<Option<ClaimedRun>>;
    /// Report a routed run's terminal result to its durable owner.
    async fn report(&self, session_id: &str, report: &RunReport) -> Result<()>;
}

/// The run-body seam: execute one claimed run and return its terminal result.
#[allow(async_fn_in_trait)]
pub trait Executor {
    async fn execute(&self, run: &ClaimedRun) -> RunReport;
}

/// The daemon loop: claim → execute → report, forever (until `stop`). No-work is a
/// re-poll (the poll itself refreshes serving liveness server-side); a transport
/// error backs off briefly. A run always produces a report — a panicking or failed
/// execution reports an error rather than silently dropping the run.
pub async fn serve_loop<C: Claimer, E: Executor>(claimer: &C, executor: &E, stop: &AtomicBool) -> Result<()> {
    while !stop.load(Ordering::Relaxed) {
        match claimer.claim().await {
            Ok(Some(run)) => {
                let report = executor.execute(&run).await;
                if let Err(e) = claimer.report(&run.session_id, &report).await {
                    super::warn(&format!("could not report run {} ({e})", run.session_id));
                }
            }
            Ok(None) => {} // no work — re-poll immediately
            Err(e) => {
                super::warn(&format!("claim failed ({e}); retrying shortly"));
                tokio::time::sleep(Duration::from_secs(3)).await;
            }
        }
    }
    Ok(())
}

/// The cloud transport: `/v1/agents/targets/:id/{claim,runs/:id/report}` with the
/// bearer AND the machine claim key. The org is derived server-side from the JWT.
pub struct ServeClient {
    http: Client,
    api: String,
    token: String,
    target_id: String,
    claim_key: String,
}

impl ServeClient {
    pub fn new(api: &str, token: &str, target_id: &str, claim_key: &str) -> Result<Self> {
        // No client-wide timeout: a claim legitimately long-polls for ~25s.
        let http = Client::builder().build().context("building serve http client")?;
        Ok(Self {
            http,
            api: api.trim_end_matches('/').to_string(),
            token: token.to_string(),
            target_id: target_id.to_string(),
            claim_key: claim_key.to_string(),
        })
    }

    /// Mint (rotate) this target's claim key, returning it. The key is held only in
    /// memory for the daemon's lifetime — never persisted, never logged.
    pub async fn mint_key(api: &str, token: &str, target_id: &str) -> Result<String> {
        let http = Client::builder().timeout(Duration::from_secs(30)).build()?;
        let url = format!("{}/v1/agents/targets/{}/claim-key", api.trim_end_matches('/'), target_id);
        let resp = http.request(Method::POST, &url).bearer_auth(token).send().await.context("mint claim key")?;
        if !resp.status().is_success() {
            return Err(anyhow!("mint claim key: {} {}", resp.status(), resp.text().await.unwrap_or_default()));
        }
        let v: serde_json::Value = resp.json().await.context("parse claim key")?;
        v.get("claimKey")
            .and_then(|k| k.as_str())
            .map(str::to_string)
            .ok_or_else(|| anyhow!("claim-key response missing claimKey"))
    }
}

impl Claimer for ServeClient {
    async fn claim(&self) -> Result<Option<ClaimedRun>> {
        let url = format!("{}/v1/agents/targets/{}/claim", self.api, self.target_id);
        let resp = self
            .http
            .request(Method::POST, &url)
            .bearer_auth(&self.token)
            .header(CLAIM_KEY_HEADER, &self.claim_key)
            .send()
            .await
            .context("claim request")?;
        match resp.status().as_u16() {
            204 => Ok(None),
            200 => Ok(Some(resp.json::<ClaimedRun>().await.context("parse claimed run")?)),
            403 => Err(anyhow!("claim rejected (target/key not accepted) — re-mint the claim key")),
            other => Err(anyhow!("claim failed: {} {}", other, resp.text().await.unwrap_or_default())),
        }
    }

    async fn report(&self, session_id: &str, report: &RunReport) -> Result<()> {
        let url = format!("{}/v1/agents/targets/{}/runs/{}/report", self.api, self.target_id, session_id);
        let resp = self
            .http
            .request(Method::POST, &url)
            .bearer_auth(&self.token)
            .header(CLAIM_KEY_HEADER, &self.claim_key)
            .json(report)
            .send()
            .await
            .context("report request")?;
        if !resp.status().is_success() {
            return Err(anyhow!("report rejected: {}", resp.status()));
        }
        Ok(())
    }
}

/// The real run body: clone the org's repo with the machine's own git credential,
/// run the coding backend against it streaming into the assigned session, then
/// commit + push the branch and report the result. `git_cred` is env-only.
pub struct HostExecutor {
    api: String,
    bearer: String,
    backend: BackendKind,
    /// The git credential for clone/push, resolved from the machine's own store
    /// (a stored `hk-` key, else the bearer). Env-only — never argv or a log.
    git_cred: String,
    /// The model-routing decision reused for the headless agent run.
    cfg: Config,
}

impl HostExecutor {
    pub fn new(api: &str, bearer: &str, backend: BackendKind, git_cred: &str, cfg: Config) -> HostExecutor {
        HostExecutor { api: api.to_string(), bearer: bearer.to_string(), backend, git_cred: git_cred.to_string(), cfg }
    }
}

impl Executor for HostExecutor {
    async fn execute(&self, run: &ClaimedRun) -> RunReport {
        match self.run_inner(run).await {
            Ok(r) => r,
            Err(e) => RunReport::failed(e.to_string()),
        }
    }
}

impl HostExecutor {
    async fn run_inner(&self, run: &ClaimedRun) -> Result<RunReport> {
        println!("  executing {} on {} → {}", run.repo, run.branch, run.session_id);
        let root = tempfile::Builder::new().prefix("hanzo-routed-").tempdir().context("workdir")?;
        let repo_dir = root.path().join("repo");

        // Clone (credential env-only), starting from the base branch when given.
        let mut clone_args = vec!["clone", "--depth", "1"];
        if !run.base.is_empty() {
            clone_args.push("--branch");
            clone_args.push(&run.base);
        }
        clone_args.push(&run.clone_url);
        let repo_dir_s = repo_dir.to_string_lossy().to_string();
        clone_args.push(&repo_dir_s);
        self.git(root.path(), &clone_args).await.context("clone failed")?;
        // The working branch for the agent's changes.
        self.git(&repo_dir, &["checkout", "-b", &run.branch]).await.context("checkout branch")?;

        // Run the coding backend headless in the clone, streaming into the SAME
        // session the dispatcher opened. This reuses the keystone's exact
        // execution + streaming path (run_stream over the backend's structured
        // stream to the session client).
        let session = SessionClient::new(&self.api, &self.bearer).ok();
        let backend = resolve(self.backend);
        let routing = super::resolve_routing(&self.cfg, true, self.backend, &self.api, Some(&self.bearer))?;
        let spec = Spec {
            mode: Mode::Headless,
            task: Some(run.prompt.clone()),
            cwd: repo_dir.clone(),
            routing,
            mcp: None,
            structured: session.is_some(),
            preset_session: None,
            trust_project: false,
            resume: None,
            passthrough: Vec::new(),
        };
        // A routed run is bounded by its budget: a runaway agent is killed (its
        // child is reaped by kill_on_drop) rather than blocking the daemon forever.
        match tokio::time::timeout(run_budget(run.timeout_seconds), self.run_agent(&*backend, &spec, session, &run.session_id)).await {
            Ok(r) => r?,
            Err(_) => return Ok(RunReport::failed(format!("run exceeded its {}s budget", run_budget(run.timeout_seconds).as_secs()))),
        }

        // Stage everything and decide whether the agent changed anything.
        self.git(&repo_dir, &["add", "-A"]).await.context("stage")?;
        let porcelain = self.git_out(&repo_dir, &["status", "--porcelain"]).await?;
        if porcelain.trim().is_empty() {
            return Ok(RunReport { ok: true, changed: false, branch: run.branch.clone(), ..Default::default() });
        }
        self.git(&repo_dir, &["-c", "user.name=hanzo-agent", "-c", "user.email=agent@hanzo.ai", "commit", "-m", &commit_message(run)])
            .await
            .context("commit")?;
        // Push the branch (never --force, never a delete).
        self.git(&repo_dir, &["push", "origin", &format!("HEAD:refs/heads/{}", run.branch)]).await.context("push")?;
        let commit = self.git_out(&repo_dir, &["rev-parse", "HEAD"]).await?.trim().to_string();
        let diffstat = self.git_out(&repo_dir, &["show", "--stat", "--oneline", "HEAD"]).await.unwrap_or_default();

        Ok(RunReport {
            ok: true,
            changed: true,
            branch: run.branch.clone(),
            commit_sha: commit,
            diffstat: diffstat.chars().take(8 * 1024).collect(),
            error: String::new(),
        })
    }

    /// Spawn the backend headless, piping stdout through the keystone's `run_stream`
    /// so every event lands on the assigned cloud session. stdin/stderr are null:
    /// a routed run has no TTY.
    async fn run_agent(&self, backend: &dyn Backend, spec: &Spec, session: Option<SessionClient>, session_id: &str) -> Result<()> {
        let launch = backend.build(spec)?;
        let mut command = launch.command;
        command.stdin(Stdio::null()).stderr(Stdio::null()).kill_on_drop(true);
        let structured = session.is_some();
        if structured {
            command.stdout(Stdio::piped());
        } else {
            command.stdout(Stdio::null());
        }
        let mut child = command.spawn().map_err(|e| anyhow!("launch coding backend: {e}"))?;
        if structured {
            let stdout = child.stdout.take().ok_or_else(|| anyhow!("no stdout"))?;
            let reader = tokio::io::BufReader::new(stdout);
            let _ = super::run_stream(backend, reader, session, Some(session_id.to_string()), false).await;
        }
        let _ = child.wait().await;
        drop(launch.cleanup);
        Ok(())
    }

    /// Run `git` with a hardened, minimal env; the credential is injected ONLY via
    /// `http.extraHeader` in env (GIT_CONFIG_*), never argv, never a URL, never logged.
    async fn git(&self, cwd: &Path, args: &[&str]) -> Result<()> {
        let out = self.git_raw(cwd, args).await?;
        if !out.status.success() {
            return Err(anyhow!("git {} failed: {}", args.first().unwrap_or(&""), String::from_utf8_lossy(&out.stderr).trim().chars().take(2048).collect::<String>()));
        }
        Ok(())
    }

    async fn git_out(&self, cwd: &Path, args: &[&str]) -> Result<String> {
        let out = self.git_raw(cwd, args).await?;
        if !out.status.success() {
            return Err(anyhow!("git {} failed", args.first().unwrap_or(&"")));
        }
        Ok(String::from_utf8_lossy(&out.stdout).to_string())
    }

    async fn git_raw(&self, cwd: &Path, args: &[&str]) -> Result<std::process::Output> {
        let mut cmd = tokio::process::Command::new("git");
        cmd.args(args)
            .current_dir(cwd)
            .env_clear()
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        if let Some(path) = std::env::var_os("PATH") {
            cmd.env("PATH", path);
        }
        cmd.env("GIT_TERMINAL_PROMPT", "0")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            // Basic auth via an env-only extra header — matches the keystone's git
            // hop; the token never touches argv, the URL, or the git config file.
            .env("GIT_CONFIG_COUNT", "1")
            .env("GIT_CONFIG_KEY_0", "http.extraHeader")
            .env("GIT_CONFIG_VALUE_0", format!("Authorization: Basic {}", basic_auth("x-access-token", &self.git_cred)));
        cmd.output().await.context("spawn git")
    }
}

/// base64(user:pass) for HTTP Basic — matches the git edge's expected credential.
fn basic_auth(user: &str, pass: &str) -> String {
    base64::engine::general_purpose::STANDARD.encode(format!("{user}:{pass}"))
}

fn commit_message(run: &ClaimedRun) -> String {
    let first = run.prompt.lines().next().unwrap_or("agent changes").trim();
    let subject: String = first.chars().take(72).collect();
    if subject.is_empty() {
        "agent changes".to_string()
    } else {
        subject
    }
}

/// `hanzo code --serve` entry: sign-in gate, register this machine, mint a claim
/// key, resolve the git credential, then run the daemon loop.
pub async fn run(cfg: &mut Config, opts: &super::Options) -> Result<()> {
    let api = network::active(cfg).api;
    let (identity, bearer) = match store::active_token(cfg, &opts.brand)? {
        Some((id, t)) => (id, t.access_token),
        None => {
            return Err(anyhow!(
                "serving a run-target requires sign-in — run `hanzo login`. A daemon streams routed \
                 runs to your OWN org, derived from your token; there is no anonymous serve."
            ))
        }
    };
    let kind = BackendKind::parse(&opts.backend)?;

    // Register this machine as a run-target and get its id.
    let machine = Machine::capture().await;
    let host = context::Snapshot::capture(&std::env::current_dir().unwrap_or_default(), "", None).host;
    let tclient = TargetClient::new(&api, &bearer)?;
    let target_id = tclient
        .register(&Register::from_machine(&host, &machine))
        .await
        .context("registering this machine as a run-target")?;

    // Mint the machine claim key (held in memory only).
    let claim_key = ServeClient::mint_key(&api, &bearer, &target_id).await.context("minting the machine claim key")?;

    // The git credential for clone/push: a stored `hk-` key if present, else the
    // bearer (both env-only). Model routing is resolved per-run in the executor.
    let git_cred = crate::iam::provider::key(crate::iam::provider::Provider::Hanzo)?.unwrap_or_else(|| bearer.clone());

    println!(
        "hanzo code --serve · {} · target {} · streaming routed runs to {}",
        host,
        target_id,
        identity.owner
    );
    println!("  waiting for work… (Ctrl-C to stop)");

    let client = ServeClient::new(&api, &bearer, &target_id, &claim_key)?;
    let executor = HostExecutor::new(&api, &bearer, kind, &git_cred, cfg.clone());
    let stop = Arc::new(AtomicBool::new(false));
    let s = stop.clone();
    tokio::spawn(async move {
        let _ = tokio::signal::ctrl_c().await;
        s.store(true, Ordering::Relaxed);
    });
    serve_loop(&client, &executor, &stop).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    struct FakeClaimer {
        // runs to hand out, in order; None entries model a 204 no-work poll.
        queue: Mutex<Vec<Option<ClaimedRun>>>,
        reports: Mutex<Vec<(String, RunReport)>>,
        stop: Arc<AtomicBool>,
    }

    impl Claimer for FakeClaimer {
        async fn claim(&self) -> Result<Option<ClaimedRun>> {
            let mut q = self.queue.lock().unwrap();
            if q.is_empty() {
                self.stop.store(true, Ordering::Relaxed);
                return Ok(None);
            }
            Ok(q.remove(0))
        }
        async fn report(&self, session_id: &str, report: &RunReport) -> Result<()> {
            self.reports.lock().unwrap().push((session_id.to_string(), report.clone()));
            Ok(())
        }
    }

    struct FakeExecutor {
        result: RunReport,
        seen: Mutex<Vec<ClaimedRun>>,
    }
    impl Executor for FakeExecutor {
        async fn execute(&self, run: &ClaimedRun) -> RunReport {
            self.seen.lock().unwrap().push(run.clone());
            self.result.clone()
        }
    }

    fn a_run(session: &str) -> ClaimedRun {
        ClaimedRun {
            session_id: session.to_string(),
            repo: "api".into(),
            base: "main".into(),
            branch: "agent/1".into(),
            prompt: "add a test".into(),
            clone_url: "https://git.test/v1/git/acme/api.git".into(),
            timeout_seconds: 600,
        }
    }

    // A claimed run is executed and its result reported to the run's session.
    #[tokio::test]
    async fn claims_execute_and_report() {
        let stop = Arc::new(AtomicBool::new(false));
        let claimer = FakeClaimer {
            queue: Mutex::new(vec![Some(a_run("sess_1"))]),
            reports: Mutex::new(vec![]),
            stop: stop.clone(),
        };
        let exec = FakeExecutor { result: RunReport { ok: true, changed: true, commit_sha: "cafe".into(), ..Default::default() }, seen: Mutex::new(vec![]) };
        serve_loop(&claimer, &exec, &stop).await.unwrap();

        assert_eq!(exec.seen.lock().unwrap().len(), 1, "the run must be executed");
        let reports = claimer.reports.lock().unwrap();
        assert_eq!(reports.len(), 1);
        assert_eq!(reports[0].0, "sess_1", "report is addressed to the run's session");
        assert!(reports[0].1.ok && reports[0].1.changed && reports[0].1.commit_sha == "cafe");
    }

    // A no-work poll (204) does NOT execute or report — it simply re-polls.
    #[tokio::test]
    async fn no_work_does_not_execute() {
        let stop = Arc::new(AtomicBool::new(false));
        let claimer = FakeClaimer {
            queue: Mutex::new(vec![None]), // one 204 then the queue empties -> stop
            reports: Mutex::new(vec![]),
            stop: stop.clone(),
        };
        let exec = FakeExecutor { result: RunReport::default(), seen: Mutex::new(vec![]) };
        serve_loop(&claimer, &exec, &stop).await.unwrap();
        assert!(exec.seen.lock().unwrap().is_empty(), "204 must not execute anything");
        assert!(claimer.reports.lock().unwrap().is_empty(), "204 must not report anything");
    }

    // A failed execution is still REPORTED (as an error) — a run is never dropped.
    #[tokio::test]
    async fn a_failed_run_is_reported_as_error() {
        let stop = Arc::new(AtomicBool::new(false));
        let claimer = FakeClaimer {
            queue: Mutex::new(vec![Some(a_run("sess_err"))]),
            reports: Mutex::new(vec![]),
            stop: stop.clone(),
        };
        let exec = FakeExecutor { result: RunReport::failed("clone failed"), seen: Mutex::new(vec![]) };
        serve_loop(&claimer, &exec, &stop).await.unwrap();
        let reports = claimer.reports.lock().unwrap();
        assert_eq!(reports.len(), 1);
        assert!(!reports[0].1.ok && reports[0].1.error == "clone failed");
    }

    // The credential is base64(user:pass) — the Basic-auth form the git edge expects,
    // and the token itself is never rendered by the report/claim serializers.
    #[test]
    fn basic_auth_is_standard_base64() {
        // "x-access-token:tok" => known base64
        assert_eq!(basic_auth("x-access-token", "tok"), "eC1hY2Nlc3MtdG9rZW46dG9r");
    }

    // The run report serializes to the report wire contract; an empty field is omitted.
    #[test]
    fn report_serializes_to_the_wire_contract() {
        let r = RunReport { ok: true, changed: true, branch: "agent/1".into(), commit_sha: "abc".into(), ..Default::default() };
        let v = serde_json::to_value(&r).unwrap();
        assert_eq!(v["ok"], true);
        assert_eq!(v["changed"], true);
        assert_eq!(v["branch"], "agent/1");
        assert_eq!(v["commitSha"], "abc");
        assert!(v.get("diffstat").is_none(), "empty fields are omitted");
        assert!(v.get("error").is_none());
    }

    // A claimed run parses from the cloud claim response shape.
    #[test]
    fn claimed_run_parses_from_wire() {
        let v = serde_json::json!({
            "sessionId": "sess_1", "repo": "api", "branch": "agent/1",
            "prompt": "do it", "cloneUrl": "https://git.test/v1/git/acme/api.git",
            "base": "main", "timeoutSeconds": 900
        });
        let run: ClaimedRun = serde_json::from_value(v).unwrap();
        assert_eq!(run.session_id, "sess_1");
        assert_eq!(run.clone_url, "https://git.test/v1/git/acme/api.git");
        assert_eq!(run.timeout_seconds, 900);
    }
}
