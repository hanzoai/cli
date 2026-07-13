//! The ecosystem-shared flat credential store at ~/.hanzo/credentials.json
//! (mode 0600).
//!
//! The OS keychain ([`super::token`]) is this CLI's primary, secret store. But
//! the Go `hanzo` CLI and `hanzo-dev` read a flat JSON session file at this
//! path — so on every successful login of the DEFAULT `hanzo` brand we ALSO
//! write it here, giving the whole ecosystem one shared session. The six fields
//! below are owned by the login flow; any OTHER key (platform_token, …) written
//! by another tool is preserved verbatim through every rewrite via `extra`.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::fs;
use std::io::Write;
use std::path::PathBuf;

use super::jwt;
use super::token::TokenSet;

fn is_zero(n: &i64) -> bool {
    *n == 0
}

/// The on-disk flat session. Unknown keys land in `extra` and are written back
/// unchanged, so this flow never clobbers another tool's keys.
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
    fn clear_owned(&mut self) {
        self.access_token.clear();
        self.refresh_token.clear();
        self.token_type.clear();
        self.expiry = 0;
        self.subject.clear();
        self.owner.clear();
    }
}

/// Build the owned fields of a flat session from a keychain [`TokenSet`],
/// decoding identity + expiry from the access token's JWT claims. `expires_in`
/// (token endpoint) wins for expiry; otherwise the JWT `exp` claim is used.
fn owned_from_tokens(tokens: &TokenSet) -> Credentials {
    let mut c = Credentials {
        access_token: tokens.access_token.clone(),
        refresh_token: tokens.refresh_token.clone().unwrap_or_default(),
        token_type: tokens.token_type.clone(),
        ..Default::default()
    };
    if let Ok(claims) = jwt::decode_claims(&tokens.access_token) {
        let email = jwt::claim_str(&claims, "email");
        c.subject = if email.is_empty() {
            jwt::claim_str(&claims, "sub")
        } else {
            email
        };
        c.owner = jwt::claim_str(&claims, "owner");
        let exp = jwt::claim_i64(&claims, "exp");
        if exp > 0 {
            c.expiry = exp;
        }
    }
    if let Some(expires_in) = tokens.expires_in {
        if expires_in > 0 {
            c.expiry = now_unix() + expires_in;
        }
    }
    c
}

/// Mirror a successful login into the shared flat store, preserving any keys
/// this flow does not own (platform_token, …).
pub fn mirror(tokens: &TokenSet) -> Result<()> {
    let mut creds = owned_from_tokens(tokens);
    creds.extra = Credentials::load()?.extra;
    creds.save()
}

/// Remove the login token from the shared store, returning whether a session
/// was actually cleared. If other tools' keys share the file, keep the file and
/// null out only the owned keys; otherwise remove the file entirely.
pub fn logout() -> Result<bool> {
    let mut creds = Credentials::load()?;
    let had_session = creds.logged_in();
    if creds.extra.is_empty() {
        let path = credentials_path()?;
        match fs::remove_file(&path) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e).with_context(|| format!("remove {}", path.display())),
        }
    } else {
        creds.clear_owned();
        creds.save()?;
    }
    Ok(had_session)
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

fn now_unix() -> i64 {
    chrono::Utc::now().timestamp()
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

    fn make_jwt(claims: serde_json::Value) -> String {
        use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
        let hdr = URL_SAFE_NO_PAD.encode(br#"{"alg":"none"}"#);
        let payload = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&claims).unwrap());
        format!("{hdr}.{payload}.sig")
    }

    #[test]
    fn serde_round_trip_preserves_unknown_keys() {
        let raw =
            r#"{"access_token":"a","owner":"hanzo","platform_token":"svc-1","build_token":"b"}"#;
        let creds: Credentials = serde_json::from_str(raw).unwrap();
        assert_eq!(creds.access_token, "a");
        assert_eq!(creds.owner, "hanzo");
        assert_eq!(creds.extra.get("platform_token").unwrap(), "svc-1");

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
    fn owned_from_tokens_extracts_identity_and_expires_in_wins() {
        let tok = make_jwt(
            serde_json::json!({"email": "z@hanzo.ai", "owner": "hanzo", "exp": 2_000_000_000i64}),
        );
        let c = owned_from_tokens(&TokenSet {
            access_token: tok,
            token_type: "Bearer".into(),
            refresh_token: Some("r".into()),
            id_token: None,
            expires_in: Some(3600),
            scope: None,
        });
        assert_eq!(c.subject, "z@hanzo.ai");
        assert_eq!(c.owner, "hanzo");
        assert_eq!(c.refresh_token, "r");
        assert_ne!(
            c.expiry, 2_000_000_000,
            "expires_in must win over exp claim"
        );
    }

    #[test]
    fn owned_from_tokens_falls_back_to_exp_claim() {
        let tok = make_jwt(serde_json::json!({"sub": "u-1", "exp": 2_000_000_000i64}));
        let c = owned_from_tokens(&TokenSet {
            access_token: tok,
            token_type: "Bearer".into(),
            refresh_token: None,
            id_token: None,
            expires_in: None,
            scope: None,
        });
        assert_eq!(c.subject, "u-1", "sub used when email absent");
        assert_eq!(c.expiry, 2_000_000_000, "exp claim used when no expires_in");
    }

    #[test]
    fn mirror_then_load_round_trips_and_is_0600() {
        with_sandbox(|| {
            let tok = make_jwt(serde_json::json!({"email": "z@hanzo.ai", "owner": "hanzo"}));
            mirror(&TokenSet {
                access_token: tok.clone(),
                token_type: "Bearer".into(),
                refresh_token: Some("r".into()),
                id_token: None,
                expires_in: Some(3600),
                scope: None,
            })
            .unwrap();

            let loaded = Credentials::load().unwrap();
            assert!(loaded.logged_in());
            assert_eq!(loaded.access_token, tok);
            assert_eq!(loaded.subject, "z@hanzo.ai");
            assert_eq!(loaded.owner, "hanzo");

            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let mode = fs::metadata(credentials_path().unwrap())
                    .unwrap()
                    .permissions()
                    .mode();
                assert_eq!(mode & 0o777, 0o600);
            }

            assert!(logout().unwrap(), "cleared an active session");
            assert!(!Credentials::load().unwrap().logged_in());
            assert!(!logout().unwrap(), "second logout clears nothing");
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

            assert!(logout().unwrap(), "cleared the owned session");

            let after = Credentials::load().unwrap();
            assert!(!after.logged_in(), "owned keys cleared");
            assert!(after.subject.is_empty());
            assert_eq!(
                after.extra.get("platform_token").unwrap(),
                "svc-1",
                "other tool's key preserved"
            );
        });
    }
}
