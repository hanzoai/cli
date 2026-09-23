//! `hanzo net` — the org's zero-trust network (`/v1/network`).
//!
//! Cloud owns the ZT controller; this is the thin client. `ls` reads the network
//! view, `join` ensures the caller's identity, `up` runs the tunnel, `publish`
//! names a local service on the network's DNS, `rm` deletes an identity. Auth is
//! the seam every other cloud command uses — the active hanzo.id bearer against
//! the active network's api origin, over [`hanzo_client::Http`] — and the org is
//! the gateway's to derive from the JWT. Nothing is filed on disk: the tunnel
//! logs in with the same IAM token, and the controller knows the identity by its
//! externalId, the token's subject.
//!
//! The wire contract: `POST /v1/network/identities` takes `{name?, roles?}`,
//! ensures the identity whose externalId is the caller's subject (idempotent),
//! and answers `{id, name, externalId, roles}`; `POST /v1/network/services`
//! takes `{name, host, port}` and answers `{dns}`.

use crate::commands::{launch, network};
use crate::config::Config;
use crate::iam::{paths, store};
use anyhow::{anyhow, bail, Context, Result};
use colored::*;
use hanzo_client::{Http, Method, Request, Transport};
use serde::Deserialize;
use serde_json::{json, Value};

/// The controller the tunnel logs in to. It sits behind Cloudflare; the tunnel
/// reaches the edge router directly.
const CONTROLLER: &str = "https://zt-api.hanzo.ai";

/// The caller's network identity, known to the controller by its externalId.
#[derive(Debug, Deserialize)]
pub struct Identity {
    pub id: String,
    pub name: String,
    #[serde(rename = "externalId")]
    pub external_id: String,
    #[serde(default)]
    pub roles: Vec<String>,
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

/// One call, and its 2xx body; a non-2xx is the server's own status and words.
async fn send(http: &Http, method: Method, url: &str, token: &str, body: Option<Value>) -> Result<Value> {
    let mut request = Request::new(method, url).token(token);
    if let Some(body) = body {
        request = request.body(body);
    }
    Ok(http.send(request).await?.ok()?)
}

async fn read_view(http: &Http, api: &str, token: &str) -> Result<Value> {
    send(http, Method::GET, &format!("{api}/v1/network"), token, None).await
}

async fn ensure_identity(
    http: &Http,
    api: &str,
    token: &str,
    name: Option<&str>,
    roles: &[String],
) -> Result<Identity> {
    let mut body = json!({});
    // Both are optional on the wire; an absent name is the caller's subject, and
    // an empty list is not a statement.
    if let Some(name) = name {
        body["name"] = json!(name);
    }
    if !roles.is_empty() {
        body["roles"] = json!(roles);
    }
    let url = format!("{api}/v1/network/identities");
    let v = send(http, Method::POST, &url, token, Some(body)).await?;
    serde_json::from_value(v).context("decode network identity")
}

async fn create_service(
    http: &Http,
    api: &str,
    token: &str,
    name: &str,
    host: &str,
    port: u16,
) -> Result<String> {
    let body = json!({ "name": name, "host": host, "port": port });
    let url = format!("{api}/v1/network/services");
    let v = send(http, Method::POST, &url, token, Some(body)).await?;
    v.get("dns")
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| anyhow!("cloud answered without a dns name: {v}"))
}

async fn delete_identity(http: &Http, api: &str, token: &str, id: &str) -> Result<()> {
    let url = format!("{api}/v1/network/identities/{id}");
    send(http, Method::DELETE, &url, token, None).await?;
    Ok(())
}

// ---- names and targets --------------------------------------------------------

/// A service name, bounded before cloud sees it.
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

// ---- `hanzo net <verb>` ------------------------------------------------------

/// `hanzo net ls` — the network view, as cloud renders it.
pub async fn ls(cfg: &mut Config) -> Result<()> {
    let (api, tok) = signin(cfg).await?;
    let v = read_view(&Http::default(), &api, &tok).await?;
    println!("{}", serde_json::to_string_pretty(&v)?);
    Ok(())
}

/// `hanzo net join [--name N] [--roles r1,r2]` — ensure the caller's identity
/// and print it. Idempotent, and it files nothing: `hanzo net up` logs in with
/// the IAM token itself.
pub async fn join(cfg: &mut Config, name: Option<String>, roles: Vec<String>) -> Result<()> {
    let (api, tok) = signin(cfg).await?;
    let id = ensure_identity(&Http::default(), &api, &tok, name.as_deref(), &roles).await?;
    println!("{} identity {} ({})", "✓".green(), id.name.cyan().bold(), id.id);
    println!("  externalId {}", id.external_id);
    if !id.roles.is_empty() {
        println!("  roles {}", id.roles.join(", "));
    }
    println!("  run: {}", "hanzo net up".cyan());
    Ok(())
}

/// `hanzo net up [MODE]` — the ZT tunnel in the foreground, logged in by the
/// IAM token `hanzo auth token` prints, fetched again before it expires.
pub fn up(mode: String) -> Result<()> {
    let bin = launch::resolve("HANZO_ZT_BIN", &["zt"]).ok_or_else(|| {
        anyhow!(
            "zt not found. Set HANZO_ZT_BIN=/path/to/zt or put `zt` on PATH \
             (the hanzo.sh installer does not ship it yet)."
        )
    })?;
    launch::exec(&bin, &tunnel_args(&mode))
}

fn tunnel_args(mode: &str) -> Vec<String> {
    ["tunnel", mode, "--controller", CONTROLLER, "--token-command", "hanzo auth token"]
        .map(String::from)
        .to_vec()
}

/// `hanzo net publish <name> <host:port>` — name a service on the network's DNS.
pub async fn publish(cfg: &mut Config, name: String, target: String) -> Result<()> {
    if !valid_name(&name) {
        bail!("service name {name:?} — use letters, digits, `-`, `_`, `.` (max 64)");
    }
    let (host, port) = host_port(&target)?;
    let (api, tok) = signin(cfg).await?;
    let dns = create_service(&Http::default(), &api, &tok, &name, &host, port).await?;
    println!("{} {} → {}", "✓".green(), dns.cyan().bold(), target);
    Ok(())
}

/// `hanzo net rm <id>` — take an identity off this org's network. It leaves the
/// network entirely only when no other org still holds it, or when it is yours.
pub async fn rm(cfg: &mut Config, id: String) -> Result<()> {
    if id.is_empty() || !id.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_')) {
        bail!("identity id {id:?} is not an id this command will put in a url");
    }
    let (api, tok) = signin(cfg).await?;
    delete_identity(&Http::default(), &api, &tok, &id).await?;
    println!("{} {} is off this org's network", "✓".green(), id);
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
    /// the bearer, answered 201 with `{id, name, externalId, roles}`.
    #[tokio::test]
    async fn join_sends_name_and_roles_and_decodes_the_identity() {
        let fake = Fake::serve(
            201,
            r#"{"id":"idn_1","name":"box","externalId":"sub_1","roles":["k8s-host"]}"#,
        )
        .await;
        let id = ensure_identity(
            &Http::default(),
            &fake.base,
            "TOK",
            Some("box"),
            &["k8s-host".to_string()],
        )
        .await
        .unwrap();

        assert_eq!(id.id, "idn_1");
        assert_eq!(id.name, "box");
        assert_eq!(id.external_id, "sub_1");
        assert_eq!(id.roles, ["k8s-host"]);
        let (method, path, auth, body) = fake.one();
        assert_eq!(method, "POST");
        assert_eq!(path, "/v1/network/identities");
        assert_eq!(auth, "Bearer TOK");
        let v: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(v["name"], "box");
        assert_eq!(v["roles"], json!(["k8s-host"]));
    }

    /// Name and roles are optional on the wire: without them the body is `{}`,
    /// and cloud names the identity after the caller's subject.
    #[tokio::test]
    async fn join_omits_what_it_was_not_given() {
        let fake = Fake::serve(201, r#"{"id":"idn_2","name":"sub_1","externalId":"sub_1"}"#).await;
        let id = ensure_identity(&Http::default(), &fake.base, "TOK", None, &[]).await.unwrap();
        assert_eq!(id.name, "sub_1");
        assert!(id.roles.is_empty());
        let (_, _, _, body) = fake.one();
        assert_eq!(serde_json::from_str::<Value>(&body).unwrap(), json!({}));
    }

    /// `up` runs the tunnel against the production controller, logged in by the
    /// CLI's own IAM token.
    #[test]
    fn up_logs_the_tunnel_in_by_iam_token() {
        assert_eq!(
            tunnel_args("proxy"),
            [
                "tunnel",
                "proxy",
                "--controller",
                "https://zt-api.hanzo.ai",
                "--token-command",
                "hanzo auth token"
            ]
        );
    }

    /// `publish`'s wire shape: POST /v1/network/services with `{name, host,
    /// port}`, answered by `{dns}`.
    #[tokio::test]
    async fn publish_sends_the_service_and_returns_its_dns() {
        let fake = Fake::serve(200, r#"{"dns":"k8s-dev.org.hanzo"}"#).await;
        let dns = create_service(&Http::default(), &fake.base, "TOK", "k8s-dev", "127.0.0.1", 6443)
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
        delete_identity(&Http::default(), &fake.base, "TOK", "idn_1").await.unwrap();
        let (method, path, _, _) = fake.one();
        assert_eq!(method, "DELETE");
        assert_eq!(path, "/v1/network/identities/idn_1");

        let refusing = Fake::serve(403, r#"{"error":"not yours"}"#).await;
        let err = delete_identity(&Http::default(), &refusing.base, "TOK", "idn_1")
            .await
            .unwrap_err();
        assert!(err.to_string().contains("403"), "got: {err}");
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

    /// A service name is bounded to the alphabet cloud accepts.
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
