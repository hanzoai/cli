//! Portable-session data: the "where it runs" context snapshot and the
//! machine-local resume store.
//!
//! PRIVACY (hard line): the snapshot is cwd / repo / ref / host / os / arch /
//! machine-id ONLY — it is BUILT from explicit fields, never from the process
//! environment, so no secret or token-bearing env var can leak into it. Git
//! remote URLs are scrubbed of embedded credentials before they are recorded.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::process::Command;

/// A repository's identity at snapshot time (all fields optional / best-effort).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Repo {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub root: Option<String>,
    /// Origin remote URL, credentials stripped.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remote: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub head: Option<String>,
}

impl Repo {
    pub fn is_empty(&self) -> bool {
        *self == Repo::default()
    }

    /// Best-effort git identity of `cwd` (empty when it is not a repo).
    pub fn capture(cwd: &Path) -> Repo {
        git_repo(cwd)
    }
}

/// The "where it runs" snapshot shown per session in mission-control. Contains
/// NO secrets — only location + identity fields.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Snapshot {
    #[serde(rename = "machineId")]
    pub machine_id: String,
    pub host: String,
    pub os: String,
    pub arch: String,
    pub cwd: String,
    pub backend: String,
    #[serde(rename = "backendVersion", skip_serializing_if = "Option::is_none")]
    pub backend_version: Option<String>,
    #[serde(skip_serializing_if = "Repo::is_empty")]
    pub repo: Repo,
}

impl Snapshot {
    /// Capture the snapshot for `cwd` running `backend` (`backend_version`
    /// best-effort). Reads only explicit sources — never the environment.
    pub fn capture(cwd: &Path, backend: &str, backend_version: Option<String>) -> Snapshot {
        Snapshot {
            machine_id: machine_id(),
            host: hostname(),
            os: std::env::consts::OS.to_string(),
            arch: std::env::consts::ARCH.to_string(),
            cwd: cwd.display().to_string(),
            backend: backend.to_string(),
            backend_version,
            repo: git_repo(cwd),
        }
    }

    /// The `status`-kind event payload: the snapshot tagged as the session's
    /// context record (optionally noting the session it was resumed from).
    pub fn context_payload(&self, resumed_from: Option<&str>) -> Value {
        let mut v = serde_json::to_value(self).unwrap_or_else(|_| json!({}));
        v["type"] = json!("context");
        if let Some(from) = resumed_from {
            v["resumedFrom"] = json!(from);
        }
        v
    }
}

/// Resolve a stable, privacy-clean machine id: a random id generated once and
/// cached under the Hanzo data dir. We deliberately do NOT read system machine
/// identifiers (which can be sensitive) — a per-install random id is enough to
/// group a machine's sessions and to gate same-machine resume.
pub fn machine_id() -> String {
    let path = data_dir().join("machine-id");
    if let Ok(existing) = std::fs::read_to_string(&path) {
        let id = existing.trim().to_string();
        if !id.is_empty() {
            return id;
        }
    }
    let mut bytes = [0u8; 16];
    rand::RngCore::fill_bytes(&mut rand::rngs::OsRng, &mut bytes);
    let id = hex(&bytes);
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = write_private(&path, id.as_bytes());
    id
}

fn hostname() -> String {
    if let Ok(out) = Command::new("hostname").output() {
        if out.status.success() {
            let h = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if !h.is_empty() {
                return h;
            }
        }
    }
    std::env::var("HOSTNAME")
        .or_else(|_| std::env::var("COMPUTERNAME"))
        .unwrap_or_else(|_| "unknown".to_string())
}

/// Best-effort git context for `cwd`. A non-repo yields an empty [`Repo`].
fn git_repo(cwd: &Path) -> Repo {
    let git = |args: &[&str]| -> Option<String> {
        // `git -C <cwd>` reads config from a possibly-untrusted working tree.
        // Neutralize the system/global gitconfig (a planted `core.fsmonitor`,
        // pager, alias or `include.path` there could otherwise run a command)
        // and never let git block on a credential/terminal prompt.
        let out = Command::new("git")
            .arg("-C")
            .arg(cwd)
            .args(args)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_TERMINAL_PROMPT", "0")
            .output()
            .ok()?;
        if !out.status.success() {
            return None;
        }
        let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
        (!s.is_empty()).then_some(s)
    };
    // Not a repo -> nothing to record.
    if git(&["rev-parse", "--is-inside-work-tree"]).as_deref() != Some("true") {
        return Repo::default();
    }
    Repo {
        root: git(&["rev-parse", "--show-toplevel"]),
        remote: git(&["remote", "get-url", "origin"]).map(|u| scrub_remote(&u)),
        branch: git(&["rev-parse", "--abbrev-ref", "HEAD"]),
        head: git(&["rev-parse", "HEAD"]),
    }
}

/// Strip any embedded credentials (`https://user:token@host/…`) from a remote
/// URL. scp-like remotes (`git@github.com:o/r.git`) carry no secret and are
/// left as-is. Never let a token ride along in a recorded/streamed remote.
pub fn scrub_remote(url: &str) -> String {
    match reqwest::Url::parse(url) {
        Ok(mut u) if !u.username().is_empty() || u.password().is_some() => {
            let _ = u.set_username("");
            let _ = u.set_password(None);
            u.to_string()
        }
        _ => url.to_string(),
    }
}

// ---- machine-local resume store ----

/// A resumable session's machine-local record. Non-secret (ids, paths, ref).
/// This is the machine's authoritative handle for a native `--resume`; the same
/// fields are mirrored onto the cloud session record (as status events) so a
/// future web "continue" control can re-dispatch. See the coordinator's seam.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ResumeRecord {
    #[serde(rename = "cloudSessionId")]
    pub cloud_session_id: String,
    pub backend: String,
    /// The backend's OWN session/thread id — what its native `--resume` needs.
    #[serde(rename = "backendSessionId")]
    pub backend_session_id: String,
    pub cwd: String,
    pub api: String,
    #[serde(rename = "machineId")]
    pub machine_id: String,
    #[serde(default, skip_serializing_if = "Repo::is_empty")]
    pub repo: Repo,
    /// Best-effort pointer to the backend's local transcript (bytes stay local;
    /// cross-machine upload is a documented follow-on).
    #[serde(rename = "transcriptPath", skip_serializing_if = "Option::is_none")]
    pub transcript_path: Option<String>,
    #[serde(rename = "createdAt")]
    pub created_at: i64,
}

impl ResumeRecord {
    /// The `status`-kind event payload mirroring the resume handle to cloud.
    pub fn resume_payload(&self) -> Value {
        json!({
            "type": "resume",
            "backend": self.backend,
            "backendSessionId": self.backend_session_id,
            "machineId": self.machine_id,
            "cwd": self.cwd,
            "transcriptPath": self.transcript_path,
        })
    }

    pub fn save(&self) -> Result<()> {
        let path = record_path(&self.cloud_session_id);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).context("creating resume store dir")?;
        }
        let json = serde_json::to_vec_pretty(self).context("serializing resume record")?;
        write_private(&path, &json).context("writing resume record")
    }

    pub fn load(cloud_session_id: &str) -> Result<Option<ResumeRecord>> {
        let path = record_path(cloud_session_id);
        match std::fs::read(&path) {
            Ok(bytes) => Ok(Some(
                serde_json::from_slice(&bytes).context("parsing resume record")?,
            )),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e).context("reading resume record"),
        }
    }
}

fn record_path(cloud_session_id: &str) -> PathBuf {
    // The id is cloud-minted (`sess_<hex>`); guard the filename regardless.
    let safe: String = cloud_session_id
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-')
        .collect();
    data_dir().join("code").join("sessions").join(format!("{safe}.json"))
}

/// The Hanzo per-user data dir (`~/.local/share/hanzo` on Linux, the platform
/// equivalent elsewhere). Non-secret runtime state only.
fn data_dir() -> PathBuf {
    dirs::data_local_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("hanzo")
}

/// Write a file with owner-only permissions where the platform supports it.
fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    std::fs::write(path, bytes)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_carries_no_secrets_and_only_allowlisted_keys() {
        let snap = Snapshot {
            machine_id: "m1".into(),
            host: "box".into(),
            os: "linux".into(),
            arch: "x86_64".into(),
            cwd: "/home/z/proj".into(),
            backend: "claude".into(),
            backend_version: Some("2.1.0".into()),
            repo: Repo {
                root: Some("/home/z/proj".into()),
                remote: Some("https://github.com/o/r.git".into()),
                branch: Some("main".into()),
                head: Some("abc123".into()),
            },
        };
        let v = snap.context_payload(None);
        let obj = v.as_object().unwrap();
        // Exactly the location/identity keys — nothing environment-derived.
        let allowed: std::collections::HashSet<&str> = [
            "type", "machineId", "host", "os", "arch", "cwd", "backend",
            "backendVersion", "repo",
        ]
        .into_iter()
        .collect();
        for k in obj.keys() {
            assert!(allowed.contains(k.as_str()), "unexpected snapshot key: {k}");
        }
        // No value looks like a secret.
        let blob = serde_json::to_string(&v).unwrap().to_lowercase();
        for bad in ["token", "password", "secret", "authorization", "bearer", "api_key"] {
            assert!(!blob.contains(bad), "snapshot leaked '{bad}': {blob}");
        }
        assert_eq!(v["type"], "context");
    }

    #[test]
    fn git_remote_credentials_are_scrubbed() {
        assert_eq!(
            scrub_remote("https://user:ghp_SECRETTOKEN@github.com/o/r.git"),
            "https://github.com/o/r.git"
        );
        assert_eq!(
            scrub_remote("https://x-access-token:AAA@gitlab.com/o/r.git"),
            "https://gitlab.com/o/r.git"
        );
        // scp-like + plain https carry no secret and are untouched.
        assert_eq!(scrub_remote("git@github.com:o/r.git"), "git@github.com:o/r.git");
        assert_eq!(scrub_remote("https://github.com/o/r.git"), "https://github.com/o/r.git");
    }

    #[test]
    fn resume_payload_shape_has_no_secret_fields() {
        let rec = ResumeRecord {
            cloud_session_id: "sess_1".into(),
            backend: "dev".into(),
            backend_session_id: "thread-uuid".into(),
            cwd: "/w".into(),
            api: "https://api.hanzo.ai".into(),
            machine_id: "m1".into(),
            repo: Repo::default(),
            transcript_path: None,
            created_at: 0,
        };
        let v = rec.resume_payload();
        assert_eq!(v["type"], "resume");
        assert_eq!(v["backendSessionId"], "thread-uuid");
        let blob = serde_json::to_string(&v).unwrap().to_lowercase();
        for bad in ["token", "password", "secret", "authorization", "bearer"] {
            assert!(!blob.contains(bad));
        }
    }

    #[test]
    fn resume_record_roundtrips_through_the_local_store() {
        let id = format!("sess_test_{}", std::process::id());
        let rec = ResumeRecord {
            cloud_session_id: id.clone(),
            backend: "claude".into(),
            backend_session_id: "claude-sid".into(),
            cwd: "/tmp".into(),
            api: "https://api.hanzo.ai".into(),
            machine_id: machine_id(),
            repo: Repo::default(),
            transcript_path: Some("/tmp/x.jsonl".into()),
            created_at: 123,
        };
        rec.save().unwrap();
        let loaded = ResumeRecord::load(&id).unwrap().unwrap();
        assert_eq!(loaded, rec);
        let _ = std::fs::remove_file(record_path(&id));
    }
}
