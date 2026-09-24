//! The org's Hanzo ZT network as cloud serves it (`/v1/network`): who is on it,
//! and what it publishes.
//!
//! Cloud owns the controller; this is the thin client. A caller is ON the network
//! as its IAM subject — `POST /v1/network/identities` makes sure the identity
//! whose externalId is the token's `sub` exists, carrying the org's role — and the
//! tunnel then signs in with that same token. A service is `<name>.<org>` on the
//! fabric, answering at `<name>.<org>.zt`; hosting it takes the `<name>-host`
//! role. Publishing, unpublishing and taking a role are a STEWARD's acts (an
//! admin of the org, or the org's own machine client), and a refusal says so.
//!
//! Services and identities the platform holds for itself (they carry the
//! `platform` role, not the org's) are not listed here and cannot be changed
//! here. A link can still host or dial them; the fabric decides.

use crate::commands::network;
use crate::config::Config;
use crate::iam::identity::{self, Identity};
use crate::iam::{paths, store};
use anyhow::{anyhow, bail, Context, Result};
use hanzo_client::{Http, Method, Request, Transport};
use serde::Deserialize;
use serde_json::{json, Value};

/// Who a link speaks as, and where: the api origin, a bearer, the org it acts
/// in and the IAM subject the fabric knows it by.
pub struct Caller {
    pub api: String,
    pub token: String,
    /// The org selected with `--as`, else the token's own `owner`.
    pub org: String,
    /// `--as`, sent as `X-Org-Id` when set.
    selected: Option<String>,
    pub who: Identity,
    pub sub: String,
    http: Http,
}

impl Caller {
    /// Sign in with `token_command`'s output when given (a machine's own IAM
    /// client), else with the active hanzo.id session.
    pub async fn sign_in(cfg: &mut Config, token_command: Option<&str>) -> Result<Caller> {
        let api = network::active(cfg).api.trim_end_matches('/').to_string();
        let token = match token_command {
            Some(cmd) => run_token_command(cmd).await?,
            None => {
                store::active_token(cfg, paths::DEFAULT_BRAND)
                    .await?
                    .ok_or_else(|| anyhow!("not signed in — run `hanzo auth login` first"))?
                    .1
                    .access_token
            }
        };
        Caller::new(api, token, cfg.org.clone())
    }

    fn new(api: String, token: String, selected: Option<String>) -> Result<Caller> {
        let who = Identity::from_access_token(&token)?;
        let sub = identity::subject(&token).context("the access token names no subject")?;
        let org = selected.clone().unwrap_or_else(|| who.owner.clone());
        Ok(Caller { api, token, org, selected, who, sub, http: Http::default() })
    }

    fn request(&self, method: Method, url: String) -> Request {
        let request = Request::new(method, url).token(&self.token);
        match &self.selected {
            Some(org) => request.org(org),
            None => request,
        }
    }

    /// One call, and its 2xx body; a non-2xx is the server's own status and words.
    async fn send(&self, method: Method, path: &str, body: Option<Value>) -> Result<Value> {
        let mut request = self.request(method, format!("{}{path}", self.api));
        if let Some(body) = body {
            request = request.body(body);
        }
        Ok(self.http.send(request).await?.ok()?)
    }

    /// The org's published services.
    pub async fn services(&self) -> Result<Vec<Service>> {
        let v = self.send(Method::GET, "/v1/network/services", None).await?;
        let list: ServiceList = serde_json::from_value(v).context("decode network services")?;
        Ok(list.services)
    }

    /// The org's identities on the network: every one for a steward, the
    /// caller's own for a member.
    pub async fn identities(&self) -> Result<Vec<NetIdentity>> {
        let v = self.send(Method::GET, "/v1/network/identities", None).await?;
        let list: IdentityList = serde_json::from_value(v).context("decode network identities")?;
        Ok(list.identities)
    }

    /// Put the caller on the org's network as its IAM subject, with `roles` added
    /// (each scoped to the org by cloud). Idempotent.
    pub async fn ensure(&self, roles: &[String]) -> Result<NetIdentity> {
        let body = if roles.is_empty() { json!({}) } else { json!({ "roles": roles }) };
        let v = self
            .send(Method::POST, "/v1/network/identities", Some(body))
            .await
            .map_err(|e| steward(e, "taking a role on the network", &self.org))?;
        serde_json::from_value(v).context("decode network identity")
    }

    /// Publish `name` on the org's network, forwarding to `host:port` on whichever
    /// identity carries its host role.
    pub async fn publish(&self, name: &str, host: &str, port: u16) -> Result<Published> {
        let body = json!({ "name": name, "host": host, "port": port });
        let v = self
            .send(Method::POST, "/v1/network/services", Some(body))
            .await
            .map_err(|e| steward(e, "publishing a service", &self.org))?;
        serde_json::from_value(v).context("decode published service")
    }

    /// Take a published service off the org's network, by id.
    ///
    /// `DELETE /v1/network/services/{id}` is served (cloud f4a951db74) and not yet
    /// in the document `spec/cloud.json` pins, so the url is built from the origin
    /// rather than read by driftgate as a route that document declares.
    pub async fn unpublish(&self, id: &str) -> Result<()> {
        if !url_id(id) {
            bail!("{id:?} is not an id this command will put in a url");
        }
        let request = self.request(Method::DELETE, format!("{}/v1/network/services/{id}", self.api));
        self.http
            .send(request)
            .await?
            .ok()
            .map_err(|e| steward(e.into(), "taking a service off the network", &self.org))?;
        Ok(())
    }

    /// The fabric name of a service: a bare label is the caller's org's, a dotted
    /// name is taken as the fabric spells it.
    pub fn scope(&self, service: &str) -> Result<String> {
        scope(service, &self.org)
    }
}

/// A 403 from the network is the steward rule; say who may, beside what the
/// server said.
fn steward(e: anyhow::Error, what: &str, org: &str) -> anyhow::Error {
    match e.downcast_ref::<hanzo_client::Error>() {
        Some(hanzo_client::Error::Api { status: 403, .. }) => anyhow!(
            "{e}\n{what} on {org}'s network is for a steward: an admin of {org}, or {org}'s own \
             machine client (pass its token with --token-command)"
        ),
        _ => e,
    }
}

/// Run a token command the way zt does — no shell, split on whitespace — and
/// return what it printed. The token is never echoed.
pub async fn run_token_command(cmd: &str) -> Result<String> {
    let argv: Vec<&str> = cmd.split_whitespace().collect();
    let (bin, args) = argv.split_first().ok_or_else(|| anyhow!("the token command is empty"))?;
    let out = tokio::process::Command::new(bin)
        .args(args)
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::inherit())
        .output()
        .await
        .with_context(|| format!("running the token command `{cmd}`"))?;
    if !out.status.success() {
        bail!("the token command `{cmd}` exited with {}", out.status);
    }
    let token = String::from_utf8(out.stdout)
        .with_context(|| format!("the token command `{cmd}` printed something that is not text"))?
        .trim()
        .to_string();
    if token.is_empty() {
        bail!("the token command `{cmd}` printed no token");
    }
    Ok(token)
}

#[derive(Debug, Deserialize)]
struct ServiceList {
    #[serde(default)]
    services: Vec<Service>,
}

/// One published service, as the org's list shows it.
#[derive(Debug, Deserialize)]
pub struct Service {
    pub id: String,
    /// The fabric name, `<name>.<org>`.
    pub service: String,
}

/// A service as published: the name the fabric answers for it.
#[derive(Debug, Deserialize)]
pub struct Published {
    pub dns: String,
}

#[derive(Debug, Deserialize)]
struct IdentityList {
    #[serde(default)]
    identities: Vec<NetIdentity>,
}

/// An identity on the org's network, known to the controller by its externalId
/// — the IAM subject it signs in as.
#[derive(Debug, Deserialize)]
pub struct NetIdentity {
    pub name: String,
    #[serde(rename = "externalId")]
    pub external_id: String,
    #[serde(default)]
    pub roles: Vec<String>,
}

/// A name cloud puts on the fabric and into DNS: a DNS label, lower-cased —
/// cloud's own rule, so a refusal happens here with the reason rather than there.
pub fn label(s: &str) -> Result<String> {
    let l = s.trim().to_ascii_lowercase();
    let inner = |i: usize| i > 0 && i + 1 < l.len();
    if l.is_empty()
        || l.len() > 63
        || !l.bytes().enumerate().all(|(i, b)| b.is_ascii_lowercase() || b.is_ascii_digit() || (b == b'-' && inner(i)))
    {
        bail!("{s:?} is not a service name: 1-63 lowercase letters, digits and inner hyphens");
    }
    Ok(l)
}

/// A service's fabric name: `label` is the org's (`<label>.<org>`), a dotted
/// name is the fabric's own spelling, each part a label.
pub fn scope(service: &str, org: &str) -> Result<String> {
    if service.contains('.') {
        let parts = service.split('.').map(label).collect::<Result<Vec<_>>>()?;
        return Ok(parts.join("."));
    }
    Ok(format!("{}.{org}", label(service)?))
}

/// `host:port`, split at the LAST colon so a dotted host survives.
pub fn host_port(s: &str) -> Result<(String, u16)> {
    let (host, port) = s.rsplit_once(':').ok_or_else(|| anyhow!("want host:port, got {s:?}"))?;
    if host.is_empty() {
        bail!("want host:port, got {s:?}");
    }
    let port: u16 = port.parse().with_context(|| format!("port in {s:?}"))?;
    if port == 0 {
        bail!("port out of range: 0");
    }
    Ok((host.to_string(), port))
}

/// An id the controller mints — letters, digits, `-`, `_`, `.` — and never one
/// of dots alone, which a url would read as a path step.
fn url_id(id: &str) -> bool {
    !id.is_empty()
        && !id.bytes().all(|b| b == b'.')
        && id.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    /// One request the fake observed: method, path, authorization, x-org-id, body.
    type Seen = (String, String, String, String, String);

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
                            let len: usize =
                                header("content-length").and_then(|v| v.parse().ok()).unwrap_or(0);
                            if text.len() < head_end + 4 + len {
                                continue;
                            }
                            let mut req = head.lines().next().unwrap_or_default().split_whitespace();
                            record.lock().unwrap().push((
                                req.next().unwrap_or_default().to_string(),
                                req.next().unwrap_or_default().to_string(),
                                header("authorization").unwrap_or_default(),
                                header("x-org-id").unwrap_or_default(),
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

    /// A token whose claims name owner `acme`, user `box` and subject `sub_1`.
    fn token() -> String {
        identity::testjwt::claims_jwt(r#"{"owner":"acme","name":"box","sub":"sub_1"}"#)
    }

    fn caller(base: &str, selected: Option<&str>) -> Caller {
        Caller::new(base.to_string(), token(), selected.map(str::to_string)).unwrap()
    }

    #[test]
    fn a_caller_acts_in_its_own_org_unless_it_selects_one() {
        let c = caller("http://x", None);
        assert_eq!((c.org.as_str(), c.sub.as_str(), c.who.name.as_str()), ("acme", "sub_1", "box"));
        assert_eq!(caller("http://x", Some("zoo")).org, "zoo");
    }

    /// The identity's wire shape: POST /v1/network/identities, the roles only
    /// when there are some, the bearer, and `--as` as X-Org-Id.
    #[tokio::test]
    async fn ensure_sends_the_roles_and_decodes_the_identity() {
        let fake = Fake::serve(
            201,
            r#"{"id":"idn_1","name":"box.acme","externalId":"sub_1","roles":["org-acme","web-host.acme"]}"#,
        )
        .await;
        let id = caller(&fake.base, Some("acme")).ensure(&["web-host".into()]).await.unwrap();
        assert_eq!((id.name.as_str(), id.external_id.as_str()), ("box.acme", "sub_1"));
        assert_eq!(id.roles, ["org-acme", "web-host.acme"]);
        let (method, path, auth, org, body) = fake.one();
        assert_eq!((method.as_str(), path.as_str()), ("POST", "/v1/network/identities"));
        assert_eq!(auth, format!("Bearer {}", token()));
        assert_eq!(org, "acme");
        assert_eq!(serde_json::from_str::<Value>(&body).unwrap(), json!({"roles": ["web-host"]}));
    }

    #[tokio::test]
    async fn joining_without_roles_sends_an_empty_body_and_no_org() {
        let fake = Fake::serve(201, r#"{"id":"idn_2","name":"sub_1","externalId":"sub_1"}"#).await;
        caller(&fake.base, None).ensure(&[]).await.unwrap();
        let (_, _, _, org, body) = fake.one();
        assert_eq!(org, "", "no --as, no X-Org-Id");
        assert_eq!(serde_json::from_str::<Value>(&body).unwrap(), json!({}));
    }

    #[tokio::test]
    async fn publish_sends_the_service_and_returns_its_dns() {
        let fake = Fake::serve(201, r#"{"id":"svc_1","name":"web","dns":"web.acme.zt"}"#).await;
        let p = caller(&fake.base, None).publish("web", "127.0.0.1", 8080).await.unwrap();
        assert_eq!(p.dns, "web.acme.zt");
        let (method, path, _, _, body) = fake.one();
        assert_eq!((method.as_str(), path.as_str()), ("POST", "/v1/network/services"));
        assert_eq!(
            serde_json::from_str::<Value>(&body).unwrap(),
            json!({"name": "web", "host": "127.0.0.1", "port": 8080})
        );
    }

    /// A member publishing is refused by cloud with a 403; the error keeps the
    /// server's words and says who may.
    #[tokio::test]
    async fn a_refused_publish_names_the_steward_rule() {
        let fake = Fake::serve(
            403,
            r#"{"error":"publishing a service is for an admin of the org or its own machine client"}"#,
        )
        .await;
        let err = caller(&fake.base, None).publish("web", "127.0.0.1", 8080).await.unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("403"), "{msg}");
        assert!(msg.contains("is for a steward: an admin of acme"), "{msg}");
        assert!(msg.contains("--token-command"), "{msg}");
    }

    #[tokio::test]
    async fn services_and_identities_decode_the_org_lists() {
        let fake = Fake::serve(200, r#"{"services":[{"id":"s1","service":"web.acme","mtls":"required","status":"active"}]}"#).await;
        let s = caller(&fake.base, None).services().await.unwrap();
        assert_eq!((s[0].id.as_str(), s[0].service.as_str()), ("s1", "web.acme"));
        assert_eq!(fake.one().1, "/v1/network/services");

        let fake = Fake::serve(200, r#"{"identities":[]}"#).await;
        assert!(caller(&fake.base, None).identities().await.unwrap().is_empty());
        assert_eq!(fake.one().1, "/v1/network/identities");
    }

    #[tokio::test]
    async fn unpublish_deletes_by_id_and_a_refusal_is_an_error() {
        let fake = Fake::serve(200, "{}").await;
        caller(&fake.base, None).unpublish("7OLM0dO4Y5VMsPoIuf2kST").await.unwrap();
        let (method, path, ..) = fake.one();
        assert_eq!((method.as_str(), path.as_str()), ("DELETE", "/v1/network/services/7OLM0dO4Y5VMsPoIuf2kST"));

        let fake = Fake::serve(403, r#"{"error":"not yours"}"#).await;
        let err = caller(&fake.base, None).unpublish("svc_1").await.unwrap_err();
        assert!(err.to_string().contains("is for a steward"), "{err}");

        assert!(caller("http://x", None).unpublish("..").await.is_err());
    }

    #[tokio::test]
    async fn a_token_command_runs_without_a_shell_and_is_trimmed() {
        assert_eq!(run_token_command("echo  tok").await.unwrap(), "tok");
        // No shell: `$HOME` is an argument, not an expansion.
        assert_eq!(run_token_command("echo $HOME").await.unwrap(), "$HOME");
        assert!(run_token_command("false").await.unwrap_err().to_string().contains("exited"));
        assert!(run_token_command("true").await.unwrap_err().to_string().contains("printed no token"));
        assert!(run_token_command("   ").await.is_err());
    }

    #[test]
    fn a_service_name_is_a_dns_label() {
        assert_eq!(label("Web-1").unwrap(), "web-1");
        for bad in ["", "-a", "a-", "a_b", "a.b", "a b", &"x".repeat(64)] {
            assert!(label(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn a_bare_service_is_the_orgs_and_a_dotted_one_is_the_fabrics() {
        assert_eq!(scope("web", "acme").unwrap(), "web.acme");
        assert_eq!(scope("k8s.hanzo", "acme").unwrap(), "k8s.hanzo");
        assert!(scope("k8s..hanzo", "acme").is_err());
        assert!(scope("a/b", "acme").is_err());
    }

    #[test]
    fn targets_parse_as_host_port() {
        assert_eq!(host_port("127.0.0.1:6443").unwrap(), ("127.0.0.1".into(), 6443));
        assert_eq!(host_port("db.local:5432").unwrap(), ("db.local".into(), 5432));
        for bad in ["6443", ":6443", "x:0", "x:notaport"] {
            assert!(host_port(bad).is_err(), "{bad}");
        }
    }
}
