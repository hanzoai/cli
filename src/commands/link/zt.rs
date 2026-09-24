//! The `zt` tunneler (hanzozt/zt): found on this machine, or installed at the
//! pinned release, and the argv every link runs it with.
//!
//! A tunnel signs in to the controller with an IAM access token that a command
//! prints (`--token-command`). zt runs that command WITHOUT a shell and splits
//! it on whitespace, so a token command here is split by the same rule.

use crate::commands::{launch, vm};
use anyhow::{anyhow, bail, Context, Result};
use serde_json::Value;
use std::path::PathBuf;

/// The Hanzo ZT controller every tunnel signs in to.
pub const CONTROLLER: &str = "https://zt-api.hanzo.ai";

/// The hanzozt/zt release this CLI installs when no `zt` is on the machine.
pub const VERSION: &str = "1.7.4";

/// sha256 of each release tarball at [`VERSION`]. Compiled in, so the download
/// is checked against what this build was cut with rather than against a digest
/// served beside the tarball it vouches for.
const DIGESTS: &[(&str, &str)] = &[
    ("darwin-amd64", "d264ed77b6147c70e758c8fcdda88d9dc19b84f88b52a450880f5b06a2761390"),
    ("darwin-arm64", "3f2205075a76a3b9767e9984ab944f03baf8624273a066a67f170749bc156d2f"),
    ("linux-amd64", "4a6d2b2f3dc8a581c7c1561df1d0ed2e5d1bdc612be131a177607ebddb96d9ed"),
    ("linux-arm64", "d107d2d55a2137992db6f94a501d8a86b65dbfe4c59be5d0c497a5028d30bfb7"),
];

/// What a tunnel does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Mode {
    /// Host every service this identity is bound to.
    Host,
    /// Listen on `port` (every interface — zt's proxy mode has no bind flag) and
    /// carry each connection to `service`.
    Proxy { service: String, port: u16 },
}

/// The tunnel's argv after the binary.
pub fn args(mode: &Mode, token_command: &str) -> Vec<String> {
    let mut a = vec!["tunnel".to_string()];
    match mode {
        Mode::Host => a.push("host".into()),
        Mode::Proxy { service, port } => {
            a.push("proxy".into());
            a.push(format!("{service}:{port}"));
        }
    }
    a.extend(["--controller", CONTROLLER, "--token-command", token_command].map(String::from));
    a
}

/// The role the platform's own fabric identities carry (universe
/// `infra/aws/k8s/link-fabric.sh`). Cloud's tenant surface never writes it — its
/// roles are `org-<org>` or `<label>.<org>` — so it marks an identity whose roles
/// are the platform's to set, never a link's.
const PLATFORM: &str = "platform";

/// A session on the controller's client API, signed in AS the identity a token
/// names — the reading that sees the identity whole, including roles and bind
/// policies the tenant surface never lists.
pub struct Session {
    http: reqwest::Client,
    api: String,
    token: String,
}

/// This identity as the controller holds it.
pub struct Me {
    pub name: String,
    roles: Vec<String>,
}

impl Me {
    /// Whether the platform holds this identity, so a link must leave its roles alone.
    pub fn platform(&self) -> bool {
        self.roles.iter().any(|r| r == PLATFORM)
    }
}

impl Session {
    /// Sign in with an IAM access token (ext-jwt). `None` when the controller
    /// answers 401: no identity on the fabric has the token's subject yet.
    pub async fn open(iam_token: &str) -> Result<Option<Session>> {
        let http = reqwest::Client::new();
        let api = format!("{CONTROLLER}/edge/client/v1");
        let answer = http
            .post(format!("{api}/authenticate?method=ext-jwt"))
            .bearer_auth(iam_token)
            .json(&serde_json::json!({}))
            .send()
            .await
            .context("signing in to the Hanzo ZT controller")?;
        if answer.status() == reqwest::StatusCode::UNAUTHORIZED {
            return Ok(None);
        }
        let login: Value = answer
            .error_for_status()
            .context("the Hanzo ZT controller refused the sign-in")?
            .json()
            .await
            .context("decoding the controller's session")?;
        let token = login["data"]["token"].as_str().context("the controller issued no session")?.to_string();
        Ok(Some(Session { http, api, token }))
    }

    async fn get(&self, path: &str) -> Result<Value> {
        Ok(self
            .http
            .get(format!("{}{path}", self.api))
            .header("zt-session", &self.token)
            .send()
            .await
            .with_context(|| format!("reading {path} from the Hanzo ZT controller"))?
            .error_for_status()?
            .json()
            .await?)
    }

    pub async fn me(&self) -> Result<Me> {
        let v = self.get("/current-identity").await?;
        let d = &v["data"];
        Ok(Me {
            name: d["name"].as_str().unwrap_or_default().to_string(),
            roles: d["roleAttributes"]
                .as_array()
                .map(|a| a.iter().filter_map(|r| r.as_str().map(str::to_string)).collect())
                .unwrap_or_default(),
        })
    }

    /// The services this identity may HOST, by id and name.
    ///
    /// A dial needs this because zt has no dial-only mode: `tunnel proxy` also
    /// hosts every service its identity may bind. A dialer that may bind becomes
    /// one more terminator for those services, forwarding to its OWN machine's
    /// `host:port`, and the fabric spreads connections across terminators — so the
    /// service breaks for every caller, and a dialer reaching its own service
    /// hangs every other circuit.
    pub async fn bindable(&self) -> Result<Vec<(String, String)>> {
        let mut out = Vec::new();
        let mut offset = 0;
        loop {
            let page = self.get(&format!("/services?limit=500&offset={offset}")).await?;
            let data = page["data"].as_array().map(Vec::as_slice).unwrap_or_default();
            out.extend(bound(data));
            offset += data.len();
            let total = page["meta"]["pagination"]["totalCount"].as_u64().unwrap_or(0) as usize;
            if data.is_empty() || offset >= total {
                return Ok(out);
            }
        }
    }

    /// How many terminators — live hosts — a service has on the fabric.
    async fn terminators(&self, service_id: &str) -> Result<usize> {
        let v = self.get(&format!("/services/{service_id}/terminators?limit=500")).await?;
        Ok(v["data"].as_array().map(Vec::len).unwrap_or(0))
    }

    /// Refuse a dial whose identity may host anything — see [`Session::bindable`].
    pub async fn dial_only(&self, who: &str) -> Result<()> {
        let hosts: Vec<String> = self.bindable().await?.into_iter().map(|(_, name)| name).collect();
        if !hosts.is_empty() {
            bail!(
                "{who} may host {} on Hanzo ZT, and zt's proxy mode hosts whatever its identity may: \
                 this dial would become a second host for them. Dial as an identity that hosts nothing \
                 (--token-command for a separate dial identity)",
                hosts.join(", ")
            );
        }
        Ok(())
    }

    /// End the session. It would expire on its own; ending it costs one request.
    pub async fn close(self) {
        let _ = self
            .http
            .delete(format!("{}/current-api-session", self.api))
            .header("zt-session", &self.token)
            .send()
            .await;
    }
}

/// The services a page grants `Bind` on, by id and name.
fn bound(services: &[Value]) -> Vec<(String, String)> {
    services
        .iter()
        .filter(|s| s["permissions"].as_array().is_some_and(|p| p.iter().any(|p| p == "Bind")))
        .filter_map(|s| Some((s["id"].as_str()?.to_string(), s["name"].as_str()?.to_string())))
        .collect()
}

/// A host's view of its own services on the fabric, held across polls: one
/// controller session, signed in again only when the controller drops it.
pub struct Watch {
    token_command: String,
    session: Option<Session>,
}

impl Watch {
    pub fn new(token_command: &str) -> Watch {
        Watch { token_command: token_command.to_string(), session: None }
    }

    /// The services this identity may host that have NO terminator right now.
    ///
    /// Whether a terminator is this process's own is not something the client
    /// API says, so the question is whether the service has a host at all: a
    /// service another host still carries is up, and one nobody carries is the
    /// outage a rebind ends. An `Err` is a controller that could not be read,
    /// which decides nothing.
    pub async fn unhosted(&mut self) -> Result<Vec<String>> {
        if self.session.is_none() {
            let token = super::network::run_token_command(&self.token_command).await?;
            self.session = Some(Session::open(&token).await?.context("this identity is not on Hanzo ZT")?);
        }
        let session = self.session.as_ref().expect("opened above");
        let read = async {
            let mut out = Vec::new();
            for (id, name) in session.bindable().await? {
                if session.terminators(&id).await? == 0 {
                    out.push(name);
                }
            }
            anyhow::Ok(out)
        };
        let answer = read.await;
        if answer.is_err() {
            // An expired or dropped session is the likeliest cause; the next poll
            // signs in again rather than asking with it forever.
            if let Some(s) = self.session.take() {
                s.close().await;
            }
        }
        answer
    }
}

/// `zt` on this machine: `HANZO_ZT_BIN`, then PATH, then `~/.local/bin/zt` —
/// and when none exists, the pinned release installed there.
pub async fn resolve_or_install() -> Result<PathBuf> {
    if let Some(bin) = launch::resolve("HANZO_ZT_BIN", &["zt"]) {
        return Ok(bin);
    }
    let local = local_bin()?;
    if local.is_file() {
        return Ok(local);
    }
    install(&local).await
}

fn local_bin() -> Result<PathBuf> {
    Ok(dirs::home_dir().context("no home directory")?.join(".local/bin/zt"))
}

async fn install(dest: &std::path::Path) -> Result<PathBuf> {
    let platform = platform(std::env::consts::OS, std::env::consts::ARCH).ok_or_else(|| {
        anyhow!(
            "no zt build for {}-{}. Get one from https://github.com/hanzozt/zt/releases \
             and put it on PATH",
            std::env::consts::OS,
            std::env::consts::ARCH
        )
    })?;
    let asset = format!("zt-{platform}.tar.gz");
    let url = format!("https://github.com/hanzozt/zt/releases/download/v{VERSION}/{asset}");
    eprintln!("installing zt v{VERSION} ({platform}) → {} …", dest.display());
    let tarball = vm::fetch(&reqwest::Client::new(), &url).await?;
    vm::verify_sha256(&tarball, digest(platform).expect("every platform has a digest"), &asset)?;
    let dir = dest.parent().context("zt's install path has no parent")?;
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    vm::extract(&tarball, "zt", dest)?;
    Ok(dest.to_path_buf())
}

/// The release asset's platform, as hanzozt/zt spells it.
fn platform(os: &str, arch: &str) -> Option<&'static str> {
    match (os, arch) {
        ("macos", "x86_64") => Some("darwin-amd64"),
        ("macos", "aarch64") => Some("darwin-arm64"),
        ("linux", "x86_64") => Some("linux-amd64"),
        ("linux", "aarch64") => Some("linux-arm64"),
        _ => None,
    }
}

fn digest(platform: &str) -> Option<&'static str> {
    DIGESTS.iter().find(|(p, _)| *p == platform).map(|(_, d)| *d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_host_tunnel_signs_in_to_the_controller_by_token_command() {
        assert_eq!(
            args(&Mode::Host, "/etc/hanzo/link/token"),
            [
                "tunnel",
                "host",
                "--controller",
                "https://zt-api.hanzo.ai",
                "--token-command",
                "/etc/hanzo/link/token"
            ]
        );
    }

    /// The third field of `svc:port:proto` is the protocol, never a bind address,
    /// so a proxy names exactly two.
    #[test]
    fn a_proxy_names_the_service_and_its_local_port() {
        let a = args(&Mode::Proxy { service: "k8s.hanzo".into(), port: 26443 }, "hanzo auth token");
        assert_eq!(a[..3], ["tunnel", "proxy", "k8s.hanzo:26443"]);
        assert_eq!(a.last().unwrap(), "hanzo auth token", "one argument: zt splits it itself");
    }

    /// Only `Bind` makes a host; `Dial` alone is what a dialer may hold.
    #[test]
    fn a_service_is_hostable_only_where_the_page_grants_bind() {
        let page = serde_json::json!([
            {"id": "1", "name": "k8s.hanzo", "permissions": ["Dial"]},
            {"id": "2", "name": "web.acme", "permissions": ["Dial", "Bind"]},
            {"id": "3", "name": "engine.hanzo", "permissions": ["Bind"]},
            {"id": "4", "name": "odd.acme"}
        ]);
        let names: Vec<String> = bound(page.as_array().unwrap()).into_iter().map(|(_, n)| n).collect();
        assert_eq!(names, ["web.acme", "engine.hanzo"]);
    }

    #[test]
    fn the_platform_role_marks_an_identity_a_link_leaves_alone() {
        let me = |roles: &[&str]| Me { name: "x".into(), roles: roles.iter().map(|r| r.to_string()).collect() };
        assert!(me(&["platform"]).platform());
        assert!(!me(&["org-hanzo", "web-host.hanzo"]).platform());
        assert!(!me(&["platform.hanzo"]).platform(), "a scoped tenant role is not the platform's");
    }

    #[test]
    fn every_platform_zt_ships_has_a_pinned_digest() {
        for (os, arch) in [("macos", "x86_64"), ("macos", "aarch64"), ("linux", "x86_64"), ("linux", "aarch64")] {
            let p = platform(os, arch).unwrap();
            let d = digest(p).unwrap();
            assert_eq!(d.len(), 64, "{p}");
            assert!(d.bytes().all(|b| b.is_ascii_hexdigit()), "{p}");
        }
        assert_eq!(platform("windows", "x86_64"), None);
    }

    /// The pinned digest is what the download is held to: a tarball that does
    /// not hash to it is refused.
    #[test]
    fn a_tarball_that_is_not_the_pinned_one_is_refused() {
        let err = vm::verify_sha256(b"not zt", digest("linux-arm64").unwrap(), "zt-linux-arm64.tar.gz")
            .unwrap_err();
        assert!(err.to_string().contains("sha256 mismatch"), "{err}");
    }
}
