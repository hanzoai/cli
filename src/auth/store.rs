// store.rs — the credential store at ~/.hanzo/credentials.json (mode 0600).
// One flat JSON object. The six fields below are owned by the login flow; any
// OTHER key (platform_token, build_token, …) written by another tool is
// preserved verbatim through every rewrite via `extra`.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::fs;
use std::io::Write;
use std::path::PathBuf;

fn is_zero(n: &i64) -> bool {
    *n == 0
}

/// Credentials is the on-disk store. Unknown keys land in `extra` and are
/// written back unchanged, so this flow never clobbers another tool's keys.
#[derive(Debug, Default, Clone, Serialize, Deserialize, PartialEq)]
pub struct Credentials {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub access_token: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub refresh_token: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub token_type: String,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub expiry: i64, // unix seconds
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub subject: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub owner: String, // org slug from the token
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl Credentials {
    /// Load the store; a missing file yields an empty (logged-out) value.
    pub fn load() -> Result<Self> {
        let path = credentials_path()?;
        match fs::read(&path) {
            Ok(bytes) if bytes.is_empty() => Ok(Self::default()),
            Ok(bytes) => {
                serde_json::from_slice(&bytes).with_context(|| format!("parse {}", path.display()))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e).with_context(|| format!("read {}", path.display())),
        }
    }

    /// Persist the store atomically (temp file + rename) at mode 0600.
    pub fn save(&self) -> Result<()> {
        let path = credentials_path()?;
        let tmp = path.with_extension("json.tmp");
        let mut json = serde_json::to_vec_pretty(self)?;
        json.push(b'\n');
        write_private(&tmp, &json).with_context(|| format!("write {}", tmp.display()))?;
        fs::rename(&tmp, &path).with_context(|| format!("rename into {}", path.display()))?;
        Ok(())
    }

    /// True when a login token is present.
    pub fn logged_in(&self) -> bool {
        !self.access_token.is_empty()
    }

    /// Clear only the keys this flow owns, keeping everything in `extra`.
    pub fn clear_owned(&mut self) {
        self.access_token.clear();
        self.refresh_token.clear();
        self.token_type.clear();
        self.expiry = 0;
        self.subject.clear();
        self.owner.clear();
    }
}

/// Remove the login token. If other tools' keys share the file, keep the file
/// and null out only the owned keys; otherwise remove the file entirely.
pub fn logout() -> Result<()> {
    let mut creds = Credentials::load()?;
    if creds.extra.is_empty() {
        let path = credentials_path()?;
        match fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e).with_context(|| format!("remove {}", path.display())),
        }
    } else {
        creds.clear_owned();
        creds.save()
    }
}

/// ~/.hanzo (or $HANZO_HOME), created 0700 if missing.
fn hanzo_dir() -> Result<PathBuf> {
    let dir = match std::env::var_os("HANZO_HOME") {
        Some(h) => PathBuf::from(h),
        None => dirs::home_dir()
            .context("cannot determine home directory")?
            .join(".hanzo"),
    };
    fs::create_dir_all(&dir).with_context(|| format!("create {}", dir.display()))?;
    Ok(dir)
}

fn credentials_path() -> Result<PathBuf> {
    Ok(hanzo_dir()?.join("credentials.json"))
}

#[cfg(unix)]
fn write_private(path: &std::path::Path, bytes: &[u8]) -> Result<()> {
    use std::os::unix::fs::OpenOptionsExt;
    let mut f = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)?;
    f.write_all(bytes)?;
    f.sync_all()?;
    Ok(())
}

#[cfg(not(unix))]
fn write_private(path: &std::path::Path, bytes: &[u8]) -> Result<()> {
    let mut f = fs::File::create(path)?;
    f.write_all(bytes)?;
    f.sync_all()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    // HANZO_HOME is process-global; serialize the tests that lean on it.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    fn with_sandbox<T>(f: impl FnOnce() -> T) -> T {
        let _guard = ENV_LOCK.lock().unwrap();
        let dir = tempfile::tempdir().unwrap();
        std::env::set_var("HANZO_HOME", dir.path());
        let out = f();
        std::env::remove_var("HANZO_HOME");
        out
    }

    #[test]
    fn serde_round_trip_preserves_unknown_keys() {
        // A store another tool wrote, carrying keys this flow does not own.
        let raw =
            r#"{"access_token":"a","owner":"hanzo","platform_token":"svc-1","build_token":"b"}"#;
        let creds: Credentials = serde_json::from_str(raw).unwrap();
        assert_eq!(creds.access_token, "a");
        assert_eq!(creds.owner, "hanzo");
        assert_eq!(creds.extra.get("platform_token").unwrap(), "svc-1");
        assert_eq!(creds.extra.get("build_token").unwrap(), "b");

        // Re-serialize: unknown keys survive.
        let out = serde_json::to_string(&creds).unwrap();
        let reparsed: Credentials = serde_json::from_str(&out).unwrap();
        assert_eq!(creds, reparsed);
        assert!(out.contains("platform_token"));
    }

    #[test]
    fn empty_fields_are_omitted() {
        let out = serde_json::to_string(&Credentials {
            access_token: "a".into(),
            ..Default::default()
        })
        .unwrap();
        assert_eq!(out, r#"{"access_token":"a"}"#);
    }

    #[test]
    fn save_load_delete_full() {
        with_sandbox(|| {
            let creds = Credentials {
                access_token: "tok".into(),
                refresh_token: "r".into(),
                token_type: "Bearer".into(),
                expiry: 2_000_000_000,
                subject: "z@hanzo.ai".into(),
                owner: "hanzo".into(),
                extra: Map::new(),
            };
            creds.save().unwrap();

            let loaded = Credentials::load().unwrap();
            assert_eq!(loaded, creds);
            assert!(loaded.logged_in());

            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let mode = fs::metadata(credentials_path().unwrap())
                    .unwrap()
                    .permissions()
                    .mode();
                assert_eq!(mode & 0o777, 0o600);
            }

            logout().unwrap();
            // No extra keys → file removed → load is the empty value.
            assert!(!Credentials::load().unwrap().logged_in());
        });
    }

    #[test]
    fn logout_preserves_other_tools_keys() {
        with_sandbox(|| {
            let mut extra = Map::new();
            extra.insert("platform_token".into(), Value::String("svc-1".into()));
            Credentials {
                access_token: "tok".into(),
                subject: "z@hanzo.ai".into(),
                owner: "hanzo".into(),
                extra,
                ..Default::default()
            }
            .save()
            .unwrap();

            logout().unwrap();

            let after = Credentials::load().unwrap();
            assert!(!after.logged_in(), "owned keys cleared");
            assert!(after.subject.is_empty());
            assert!(after.owner.is_empty());
            assert_eq!(
                after.extra.get("platform_token").unwrap(),
                "svc-1",
                "other tool's key preserved"
            );
        });
    }
}
