//! `hanzo net` — the org's zero-trust network (`/v1/network`).
//!
//! Cloud owns the controller (OpenZiti behind the gateway); this is the thin
//! client. `ls` reads the network view, `join` mints an identity and files its
//! enrollment JWT under `~/.hanzo/net/`, `publish` names a local service on the
//! network's DNS, `rm` deletes an identity. Auth is the seam every other cloud
//! command uses — the active hanzo.id bearer against the active network's api
//! origin, over [`crate::http`] — and the org is the gateway's to derive from
//! the JWT.
//!
//! The wire contract (`k3s-link` cloud branch): `POST /v1/network/identities`
//! takes `{name, roles?}` and answers `{id, name, enrollment: {jwt, expiresAt}}`;
//! `POST /v1/network/services` takes `{name, host, port}` and answers `{dns}`.

use crate::commands::network;
use crate::config::Config;
use crate::http;
use crate::iam::{paths, store};
use anyhow::{anyhow, bail, Context, Result};
use colored::*;
use reqwest::{Client, Method};
use serde::Deserialize;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

/// What `join` mints: an identity and the one-time enrollment it carries.
#[derive(Debug, Deserialize)]
pub struct Identity {
    pub id: String,
    pub name: String,
    pub enrollment: Enrollment,
}

#[derive(Debug, Deserialize)]
pub struct Enrollment {
    pub jwt: String,
    #[serde(rename = "expiresAt")]
    pub expires_at: String,
}

/// The active api origin and a live bearer — the same two facts every cloud
/// command starts from.
async fn signin(cfg: &mut Config) -> Result<(String, String)> {
    let api = network::active(cfg).api.trim_end_matches('/').to_string();
    let (_id, tok) = store::active_token(cfg, paths::DEFAULT_BRAND)
        .await?
        .ok_or_else(|| anyhow!("not signed in — run `hanzo auth login` first"))?;
    Ok((api, tok.access_token))
}

// ---- the wire calls, separated from the sign-in so a test can aim them at a
// ---- fake server with a token of its own ------------------------------------

async fn read_view(http: &Client, api: &str, token: &str) -> Result<Value> {
    http::send_json::<Value>(http, Method::GET, &format!("{api}/v1/network"), token, None).await
}

async fn create_identity(
    http: &Client,
    api: &str,
    token: &str,
    name: &str,
    roles: &[String],
) -> Result<Identity> {
    let mut body = json!({ "name": name });
    // `roles` is optional on the wire; an empty list is not a statement.
    if !roles.is_empty() {
        body["roles"] = json!(roles);
    }
    let url = format!("{api}/v1/network/identities");
    let v = http::send_json(http, Method::POST, &url, token, Some(&body)).await?;
    serde_json::from_value(v).context("decode network identity")
}

async fn create_service(
    http: &Client,
    api: &str,
    token: &str,
    name: &str,
    host: &str,
    port: u16,
) -> Result<String> {
    let body = json!({ "name": name, "host": host, "port": port });
    let url = format!("{api}/v1/network/services");
    let v = http::send_json(http, Method::POST, &url, token, Some(&body)).await?;
    v.get("dns")
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| anyhow!("cloud answered without a dns name: {v}"))
}

async fn delete_identity(http: &Client, api: &str, token: &str, id: &str) -> Result<()> {
    let url = format!("{api}/v1/network/identities/{id}");
    http::send_json::<Value>(http, Method::DELETE, &url, token, None).await?;
    Ok(())
}

// ---- names, targets and the credential on disk -------------------------------

/// A name is a filename and a network identity at once, so it is bounded to the
/// runes both accept BEFORE either sees it.
fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && !name.starts_with('.')
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
}

/// `host:port`, split at the LAST colon so a dotted host survives.
fn host_port(s: &str) -> Result<(String, u16)> {
    let (host, port) = s
        .rsplit_once(':')
        .ok_or_else(|| anyhow!("want host:port, got {s:?}"))?;
    if host.is_empty() {
        bail!("want host:port, got {s:?}");
    }
    let port: u16 = port.parse().with_context(|| format!("port in {s:?}"))?;
    if port == 0 {
        bail!("port out of range: 0");
    }
    Ok((host.to_string(), port))
}

/// Where an identity's enrollment JWT is filed: `~/.hanzo/net/<name>.jwt`.
fn jwt_path(name: &str) -> Result<PathBuf> {
    let home = dirs::home_dir().ok_or_else(|| anyhow!("no home directory"))?;
    Ok(home.join(".hanzo").join("net").join(format!("{name}.jwt")))
}

/// Write the JWT owner-only (0600): it is a credential, not a note.
fn write_jwt(path: &Path, jwt: &str) -> Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    }
    std::fs::write(path, jwt).with_context(|| format!("writing {}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
            .with_context(|| format!("chmod 600 {}", path.display()))?;
    }
    Ok(())
}

// ---- `hanzo net <verb>` ------------------------------------------------------

/// `hanzo net ls` — the network view, as cloud renders it.
pub async fn ls(cfg: &mut Config) -> Result<()> {
    let (api, tok) = signin(cfg).await?;
    let v = read_view(&Client::new(), &api, &tok).await?;
    println!("{}", serde_json::to_string_pretty(&v)?);
    Ok(())
}

/// `hanzo net join [--name N] [--roles r1,r2]` — mint an identity, file its
/// enrollment JWT, and say how to spend it. Returns the JWT's path so
/// `hanzo up --link` can reuse the whole act.
pub async fn join(cfg: &mut Config, name: Option<String>, roles: Vec<String>) -> Result<PathBuf> {
    let name = name.unwrap_or_else(crate::commands::code::context::hostname);
    if !valid_name(&name) {
        bail!("identity name {name:?} — use letters, digits, `-`, `_`, `.` (max 64)");
    }
    let (api, tok) = signin(cfg).await?;
    let id = create_identity(&Client::new(), &api, &tok, &name, &roles).await?;
    let path = jwt_path(&id.name)?;
    write_jwt(&path, &id.enrollment.jwt)?;
    println!("{} identity {} ({})", "✓".green(), id.name.cyan().bold(), id.id);
    println!("  jwt {} (expires {})", path.display(), id.enrollment.expires_at.dimmed());
    println!(
        "  enroll: {}",
        format!("zt edge enroll --jwt {}", path.display()).cyan()
    );
    Ok(path)
}

/// `hanzo net publish <name> <host:port>` — name a service on the network's DNS.
/// Returns the dns name so `hanzo up --link` can report it.
pub async fn publish(cfg: &mut Config, name: String, target: String) -> Result<String> {
    if !valid_name(&name) {
        bail!("service name {name:?} — use letters, digits, `-`, `_`, `.` (max 64)");
    }
    let (host, port) = host_port(&target)?;
    let (api, tok) = signin(cfg).await?;
    let dns = create_service(&Client::new(), &api, &tok, &name, &host, port).await?;
    println!("{} {} → {}", "✓".green(), dns.cyan().bold(), target);
    Ok(dns)
}

/// `hanzo net rm <id>` — delete an identity.
pub async fn rm(cfg: &mut Config, id: String) -> Result<()> {
    if id.is_empty() || !id.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_')) {
        bail!("identity id {id:?} is not an id this command will put in a url");
    }
    let (api, tok) = signin(cfg).await?;
    delete_identity(&Client::new(), &api, &tok, &id).await?;
    println!("{} removed identity {}", "✓".green(), id);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    /// One request the fake observed: method, path, authorization, body.
    type Seen = (String, String, String, String);

    /// A canned `/v1/network` plane — hand-rolled over TCP, the same approach as
    /// `code::testmock`, so no test-only HTTP dependency is pulled in.
    struct Fake {
        base: String,
        seen: Arc<Mutex<Vec<Seen>>>,
    }

    impl Fake {
        async fn serve(status: u16, body: &'static str) -> Fake {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let base = format!("http://{}", listener.local_addr().unwrap());
            let seen: Arc<Mutex<Vec<Seen>>> = Arc::new(Mutex::new(Vec::new()));
            let record = seen.clone();
            tokio::spawn(async move {
                loop {
                    let Ok((mut sock, _)) = listener.accept().await else { return };
                    let record = record.clone();
                    tokio::spawn(async move {
                        let mut buf = vec![0u8; 65536];
                        let mut read = 0;
                        loop {
                            let Ok(n) = sock.read(&mut buf[read..]).await else { return };
                            if n == 0 {
                                return;
                            }
                            read += n;
                            let text = String::from_utf8_lossy(&buf[..read]).into_owned();
                            let Some(head_end) = text.find("\r\n\r\n") else { continue };
                            let head = &text[..head_end];
                            let header = |name: &str| {
                                head.lines().find_map(|l| {
                                    let (k, v) = l.split_once(':')?;
                                    k.eq_ignore_ascii_case(name).then(|| v.trim().to_string())
                                })
                            };
                            let len: usize = header("content-length")
                                .and_then(|v| v.parse().ok())
                                .unwrap_or(0);
                            if text.len() < head_end + 4 + len {
                                continue;
                            }
                            let mut req = head.lines().next().unwrap_or_default().split_whitespace();
                            record.lock().unwrap().push((
                                req.next().unwrap_or_default().to_string(),
                                req.next().unwrap_or_default().to_string(),
                                header("authorization").unwrap_or_default(),
                                text[head_end + 4..head_end + 4 + len].to_string(),
                            ));
                            let resp = format!(
                                "HTTP/1.1 {status} X\r\ncontent-type: application/json\r\n\
                                 content-length: {}\r\nconnection: close\r\n\r\n{body}",
                                body.len()
                            );
                            let _ = sock.write_all(resp.as_bytes()).await;
                            return;
                        }
                    });
                }
            });
            Fake { base, seen }
        }

        fn one(&self) -> Seen {
            let seen = self.seen.lock().unwrap();
            assert_eq!(seen.len(), 1, "one call, one request: {seen:?}");
            seen[0].clone()
        }
    }

    /// `join`'s wire shape: POST /v1/network/identities with `{name, roles}` and
    /// the bearer, decoded to `{id, name, enrollment: {jwt, expiresAt}}`.
    #[tokio::test]
    async fn join_sends_name_and_roles_and_decodes_the_enrollment() {
        let fake = Fake::serve(
            200,
            r#"{"id":"idn_1","name":"box","enrollment":{"jwt":"J.W.T","expiresAt":"2026-09-02T00:00:00Z"}}"#,
        )
        .await;
        let id = create_identity(
            &Client::new(),
            &fake.base,
            "TOK",
            "box",
            &["k8s-host".to_string()],
        )
        .await
        .unwrap();

        assert_eq!(id.id, "idn_1");
        assert_eq!(id.enrollment.jwt, "J.W.T");
        assert_eq!(id.enrollment.expires_at, "2026-09-02T00:00:00Z");
        let (method, path, auth, body) = fake.one();
        assert_eq!(method, "POST");
        assert_eq!(path, "/v1/network/identities");
        assert_eq!(auth, "Bearer TOK");
        let v: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(v["name"], "box");
        assert_eq!(v["roles"], json!(["k8s-host"]));
    }

    /// `roles` is optional on the wire: an empty list sends NO key rather than an
    /// empty statement the controller has to interpret.
    #[tokio::test]
    async fn join_omits_empty_roles() {
        let fake = Fake::serve(
            200,
            r#"{"id":"idn_2","name":"box","enrollment":{"jwt":"J","expiresAt":"e"}}"#,
        )
        .await;
        create_identity(&Client::new(), &fake.base, "TOK", "box", &[]).await.unwrap();
        let (_, _, _, body) = fake.one();
        let v: Value = serde_json::from_str(&body).unwrap();
        assert!(v.get("roles").is_none(), "empty roles must be omitted: {v}");
    }

    /// `publish`'s wire shape: POST /v1/network/services with `{name, host,
    /// port}`, answered by `{dns}`.
    #[tokio::test]
    async fn publish_sends_the_service_and_returns_its_dns() {
        let fake = Fake::serve(200, r#"{"dns":"k8s-dev.org.hanzo"}"#).await;
        let dns = create_service(&Client::new(), &fake.base, "TOK", "k8s-dev", "127.0.0.1", 6443)
            .await
            .unwrap();

        assert_eq!(dns, "k8s-dev.org.hanzo");
        let (method, path, _, body) = fake.one();
        assert_eq!(method, "POST");
        assert_eq!(path, "/v1/network/services");
        let v: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(v["name"], "k8s-dev");
        assert_eq!(v["host"], "127.0.0.1");
        assert_eq!(v["port"], 6443);
    }

    /// `rm` deletes by id, and a refusal is an error, never a silent success.
    #[tokio::test]
    async fn rm_deletes_the_identity_and_a_refusal_is_an_error() {
        let fake = Fake::serve(200, "{}").await;
        delete_identity(&Client::new(), &fake.base, "TOK", "idn_1").await.unwrap();
        let (method, path, _, _) = fake.one();
        assert_eq!(method, "DELETE");
        assert_eq!(path, "/v1/network/identities/idn_1");

        let refusing = Fake::serve(403, r#"{"error":"not yours"}"#).await;
        let err = delete_identity(&Client::new(), &refusing.base, "TOK", "idn_1")
            .await
            .unwrap_err();
        assert!(err.to_string().contains("403"), "got: {err}");
    }

    /// The enrollment JWT is a credential: filed owner-only.
    #[test]
    fn the_jwt_is_filed_owner_only() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("net").join("box.jwt");
        write_jwt(&path, "J.W.T").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "J.W.T");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600, "mode {mode:o}");
        }
    }

    #[test]
    fn targets_parse_as_host_port() {
        assert_eq!(host_port("127.0.0.1:6443").unwrap(), ("127.0.0.1".into(), 6443));
        assert_eq!(host_port("db.local:5432").unwrap(), ("db.local".into(), 5432));
        assert!(host_port("6443").is_err());
        assert!(host_port(":6443").is_err());
        assert!(host_port("x:0").is_err());
        assert!(host_port("x:notaport").is_err());
    }

    /// A name is a filename and an identity at once; both alphabets bound it.
    #[test]
    fn names_are_bounded_to_the_shared_alphabet() {
        assert!(valid_name("k8s-dev-host"));
        assert!(valid_name("box_1.internal"));
        assert!(!valid_name(""));
        assert!(!valid_name(".hidden"));
        assert!(!valid_name("a/b"));
        assert!(!valid_name("a b"));
        assert!(!valid_name(&"x".repeat(65)));
    }
}
