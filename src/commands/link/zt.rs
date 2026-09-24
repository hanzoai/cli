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

/// The services `token`'s identity may HOST, asked of the controller as that
/// identity — the one reading that sees the platform's bind policies as well as
/// an org's roles.
///
/// A dial needs this because zt has no dial-only mode: `tunnel proxy` also hosts
/// every service its identity may bind. A dialer that may bind becomes one more
/// terminator for those services, forwarding to its OWN machine's `host:port`, and
/// the fabric spreads connections across terminators — so the service breaks for
/// every caller, and a dialer reaching its own service hangs every other circuit.
pub async fn bindable(token: &str) -> Result<Vec<String>> {
    let http = reqwest::Client::new();
    let api = format!("{CONTROLLER}/edge/client/v1");
    let login: Value = http
        .post(format!("{api}/authenticate?method=ext-jwt"))
        .bearer_auth(token)
        .json(&serde_json::json!({}))
        .send()
        .await
        .context("signing in to the Hanzo ZT controller")?
        .error_for_status()
        .context("the Hanzo ZT controller refused this identity")?
        .json()
        .await
        .context("decoding the controller's session")?;
    let session = login["data"]["token"].as_str().context("the controller issued no session")?.to_string();
    let mut names = Vec::new();
    let mut offset = 0;
    loop {
        let page: Value = http
            .get(format!("{api}/services?limit=500&offset={offset}"))
            .header("zt-session", &session)
            .send()
            .await
            .context("listing this identity's services")?
            .error_for_status()?
            .json()
            .await
            .context("decoding this identity's services")?;
        let data = page["data"].as_array().map(Vec::as_slice).unwrap_or_default();
        names.extend(bound(data));
        offset += data.len();
        let total = page["meta"]["pagination"]["totalCount"].as_u64().unwrap_or(0) as usize;
        if data.is_empty() || offset >= total {
            break;
        }
    }
    // The session is this call's alone; leaving it to expire costs nothing but a row.
    let _ = http.delete(format!("{api}/current-api-session")).header("zt-session", &session).send().await;
    Ok(names)
}

/// The names of the services a page grants `Bind` on.
fn bound(services: &[Value]) -> Vec<String> {
    services
        .iter()
        .filter(|s| s["permissions"].as_array().is_some_and(|p| p.iter().any(|p| p == "Bind")))
        .filter_map(|s| s["name"].as_str().map(str::to_string))
        .collect()
}

/// Refuse a dial whose identity may host anything — see [`bindable`].
pub async fn dial_only(token: &str, who: &str) -> Result<()> {
    let hosts = bindable(token).await?;
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
            {"name": "k8s.hanzo", "permissions": ["Dial"]},
            {"name": "web.acme", "permissions": ["Dial", "Bind"]},
            {"name": "engine.hanzo", "permissions": ["Bind"]},
            {"name": "odd.acme"}
        ]);
        assert_eq!(bound(page.as_array().unwrap()), ["web.acme", "engine.hanzo"]);
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
