//! `hanzo kms` — the secret store, from the CLI.
//!
//! The house law is "secrets live in KMS, nowhere else", and "devs don't touch
//! K8s". Until now the CLI had no path to KMS at all, so the law had no local
//! instrument. This is it: the core lifecycle — `list`, `get`, `set`, `rm` —
//! against cloud's `/v1/kms/orgs/{org}/secrets` and NOTHING more. Rotation and
//! version history are deliberately absent: that surface is real in the
//! standalone luxfi/kms SDK but is NOT mounted by cloud, and inventing a verb
//! the server cannot answer is worse than not having one.
//!
//! ## A secret value has exactly one way in and one way out
//! - IN: `set` reads the value from STDIN, always. There is no `--value` and no
//!   positional value — not as a default, but structurally, so that no argv, no
//!   shell history, no CI log and no `ps` listing can ever hold it. This is the
//!   same reason `hanzo login --token -` reads stdin.
//! - OUT: `get` writes the raw bytes to stdout and nothing else — no newline, no
//!   colour, no label — so it pipes byte-exactly into a file or a program. The
//!   bytes do not change based on where stdout points: a secrets tool that
//!   emits different bytes to a TTY than to a pipe is a tool you cannot trust.
//! - NEVER: a value is never logged, never written to disk, never put in the
//!   config (which is non-secret by construction), and never held beyond the
//!   call. `list` is structurally incapable of carrying one — the server's
//!   listing has no value field.
//!
//! ## The org is addressed, never asserted
//! Cloud's KMS routes name the org in the PATH (`/v1/kms/orgs/{org}/secrets`),
//! so unlike the agents plane this CLI cannot simply stay silent about it. It
//! sends the ACTIVE IDENTITY'S OWN `owner` — the claim on the very token it is
//! authenticating with, resolved through `iam::store::active_token` and never
//! chosen by the user (there is deliberately no `--org`). The server re-derives
//! the org from the JWT it verifies and refuses any mismatch (403), so this
//! segment is an ADDRESS, not a claim to be trusted: forging it can only produce
//! a 403 against yourself. `X-Org-Id` is never sent — the transport
//! (`crate::http`) sends the bearer and nothing else. `hanzo switch` therefore
//! moves the secret namespace exactly as it moves billing, with no new machinery.

use anyhow::{anyhow, bail, Context, Result};
use colored::*;
use reqwest::{Client, Method};
use serde_json::{json, Value};
use std::io::{Read, Write};

use crate::commands::network;
use crate::config::Config;
use crate::http::send_json;
use crate::iam::identity::Identity;
use crate::iam::{paths, store};

/// A secret's address: an optional `/`-separated sub-path under the org, plus
/// the name (the last segment). ONE parse serves all four verbs, so `ci/DB`
/// means the same thing to `get`, `set` and `rm` — the same split the server
/// performs on its wildcard (last segment = name, the rest = sub-path).
#[derive(Debug, PartialEq, Eq)]
pub struct Address {
    /// Sub-path under the org, `/`-separated. Empty = the org root.
    path: String,
    /// The secret's name — the last segment.
    name: String,
}

impl Address {
    /// Parse `[sub/path/]NAME`.
    ///
    /// Only the two rules the URL BUILDER needs are enforced here; the server
    /// stays the authority on the key shape (length, charset) and answers a 400
    /// of its own. We reject `.`/`..`/empty segments because a URL library
    /// NORMALISES them away — `../../evil/secrets/X` would silently re-address
    /// another org's namespace rather than asking for the secret the user typed.
    /// (The server rejects them too, so refusing here loses no reachable
    /// surface; it only stops us from asking a question we did not mean.)
    pub fn parse(raw: &str) -> Result<Self> {
        let segs: Vec<&str> = raw.trim_matches('/').split('/').collect();
        for s in &segs {
            match *s {
                "" => bail!("secret name is required, as `NAME` or `sub/path/NAME`"),
                "." | ".." => bail!("'{s}' is not a path segment — a secret address is literal"),
                _ => {}
            }
        }
        let (name, path) = segs.split_last().expect("split always yields one segment");
        Ok(Self { path: path.join("/"), name: (*name).to_string() })
    }

    /// The `secrets/*` wildcard: every segment percent-encoded, joined by the
    /// `/` that is the route's own separator. Encoding matters — the server
    /// accepts a name containing `?`, `#` or `%` (its only bans are `/` and
    /// control characters), and a raw `X?env=prod` in a URL would address the
    /// secret `X` in a different environment. Encode, and we ask for what the
    /// user typed or we get an honest 400 — never a different secret.
    fn wildcard(&self) -> String {
        let mut out = String::new();
        for seg in self.path.split('/').filter(|s| !s.is_empty()) {
            out.push_str(&enc(seg));
            out.push('/');
        }
        out.push_str(&enc(&self.name));
        out
    }
}

/// Percent-encode one URL path segment / query value: everything outside the
/// RFC 3986 unreserved set becomes `%XX` over its UTF-8 bytes. Used for BOTH
/// the path and `?env=` — the server bans neither `&` nor `=` in an env, so an
/// unencoded one would forge query parameters.
fn enc(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// One authenticated conversation with the org's secret store: WHERE (api),
/// WHO (the bearer) and WHOSE (the org addressed) resolved exactly once, so no
/// verb re-derives them and they cannot drift apart.
pub struct Session {
    api: String,
    org: String,
    token: String,
    http: Client,
}

impl Session {
    /// Bind a session to an identity. The org is READ OFF the identity — the
    /// `owner` claim of the token we are about to authenticate with — so a
    /// caller cannot address one org while presenting another's credential.
    fn new(api: String, id: &Identity, token: String) -> Self {
        Self { api, org: id.owner.clone(), token, http: Client::new() }
    }

    fn secrets_url(&self) -> String {
        format!("{}/v1/kms/orgs/{}/secrets", self.api.trim_end_matches('/'), enc(&self.org))
    }

    async fn call(&self, method: Method, url: &str, body: Option<&Value>) -> Result<Value> {
        send_json(&self.http, method, url, &self.token, body).await
    }
}

/// Resolve the ACTIVE identity and the active network into a session.
///
/// `iam::store::active_token` is the ONE way this command — like every other —
/// learns who it is. Not signed in means NOT SIGNED IN: it never falls through
/// to another identity you happen to hold, because reading the wrong org's
/// secrets is worse than reading none.
fn open(cfg: &mut Config) -> Result<Session> {
    let api = network::active(cfg).api;
    let (id, tok) = store::active_token(cfg, paths::DEFAULT_BRAND)?
        .ok_or_else(|| anyhow!("not signed in — run `hanzo login` first"))?;
    Ok(Session::new(api, &id, tok.access_token))
}

/// Read a secret value from `r` (stdin in the binary; a fixture in tests).
///
/// Exactly ONE trailing newline is stripped, because `echo v | hanzo kms set K`
/// is the dominant shape and storing the shell's line terminator would silently
/// corrupt the value for every consumer. Nothing else is touched: leading and
/// interior bytes — spaces, a PEM's inner newlines — are the value.
pub fn read_value(mut r: impl Read) -> Result<String> {
    let mut v = String::new();
    r.read_to_string(&mut v).context("reading the secret value from stdin")?;
    if let Some(s) = v.strip_suffix('\n') {
        v = s.strip_suffix('\r').unwrap_or(s).to_string();
    }
    if v.is_empty() {
        bail!("no value on stdin — pipe the secret in, e.g. `printf %s \"$V\" | hanzo kms set NAME --env prod`");
    }
    Ok(v)
}

// ---- the four verbs ---------------------------------------------------------

/// List the org's secret ADDRESSES at `path`/`env` — never values (the server's
/// listing has no value field, so this is structural, not a promise).
///
/// One address per line, in exactly the form `get` takes, so the listing
/// composes: `hanzo kms list | xargs -n1 hanzo kms get`.
pub async fn list(cfg: &mut Config, path: Option<String>, env: String) -> Result<()> {
    let s = open(cfg)?;
    let mut url = format!("{}?env={}", s.secrets_url(), enc(&env));
    if let Some(p) = path.as_deref().map(str::trim).filter(|p| !p.trim_matches('/').is_empty()) {
        url.push_str(&format!("&path={}", enc(p.trim_matches('/'))));
    }
    let resp = s.call(Method::GET, &url, None).await?;
    let rows = resp.get("secrets").and_then(Value::as_array).cloned().unwrap_or_default();
    if rows.is_empty() {
        eprintln!("{}", format!("no secrets at env={env}").dimmed());
        return Ok(());
    }
    // The server reports the FULL store path (/orgs/{org}/ci); print the address
    // relative to the org, which is what `get` accepts.
    let root = format!("/orgs/{}", s.org);
    for r in rows {
        let (Some(name), p) = (r.get("name").and_then(Value::as_str), r.get("path").and_then(Value::as_str).unwrap_or(""))
        else {
            continue;
        };
        let rel = p.strip_prefix(&root).unwrap_or(p).trim_matches('/');
        println!("{}", if rel.is_empty() { name.to_string() } else { format!("{rel}/{name}") });
    }
    Ok(())
}

/// Read one secret and write its RAW bytes to stdout.
///
/// This is the one verb whose entire purpose is to emit the value, so it emits
/// it: no confirmation, no `--reveal`. A gate here would be theatre — it is
/// passed reflexively, and whoever can read your terminal reads the value
/// either way. The defences that matter are structural and elsewhere: the value
/// never enters argv, a log, or the disk.
pub async fn get(cfg: &mut Config, name: String, env: String) -> Result<()> {
    let s = open(cfg)?;
    let addr = Address::parse(&name)?;
    let url = format!("{}/{}?env={}", s.secrets_url(), addr.wildcard(), enc(&env));
    let resp = s.call(Method::GET, &url, None).await?;
    let val = resp
        .get("value")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("no value in the KMS response for {name}"))?;
    let mut out = std::io::stdout();
    out.write_all(val.as_bytes())?;
    out.flush()?;
    Ok(())
}

/// Upsert a secret. The VALUE COMES FROM `value` (the caller reads stdin) — it
/// is never a parameter the shell could see.
///
/// `env` is required by the server with no default, and this mirrors it rather
/// than papering over it: a silent `default` would commit the write to a bucket
/// the env's readers never resolve — the exact split that once left a live
/// credential stale in production. Fail loud, write once.
pub async fn set(cfg: &mut Config, name: String, env: String, value: String) -> Result<()> {
    let s = open(cfg)?;
    let addr = Address::parse(&name)?;
    let body = json!({ "path": addr.path, "name": addr.name, "env": env, "value": value });
    s.call(Method::POST, &s.secrets_url(), Some(&body)).await?;
    // The confirmation names the address, never the value.
    println!("{} stored {} (env {})", "✓".green(), name.cyan().bold(), env.cyan());
    Ok(())
}

/// Delete one secret.
pub async fn rm(cfg: &mut Config, name: String, env: String) -> Result<()> {
    let s = open(cfg)?;
    let addr = Address::parse(&name)?;
    let url = format!("{}/{}?env={}", s.secrets_url(), addr.wildcard(), enc(&env));
    s.call(Method::DELETE, &url, None).await?;
    println!("{} removed {} (env {})", "✓".green(), name.cyan().bold(), env.cyan());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::code::testmock::MockCloud;
    use crate::iam::identity::testjwt::jwt;

    fn session(api: &str, owner: &str) -> (Session, String) {
        let tok = jwt(owner, "z");
        let id = Identity::from_access_token(&tok).unwrap();
        (Session::new(api.to_string(), &id, tok.clone()), tok)
    }

    // ---- the address: one parse, no traversal, faithful encoding ------------

    #[test]
    fn an_address_splits_the_name_off_the_subpath() {
        assert_eq!(Address::parse("DB").unwrap(), Address { path: "".into(), name: "DB".into() });
        assert_eq!(
            Address::parse("ci/DB").unwrap(),
            Address { path: "ci".into(), name: "DB".into() }
        );
        assert_eq!(
            Address::parse("/a/b/DB/").unwrap(),
            Address { path: "a/b".into(), name: "DB".into() }
        );
    }

    /// A URL library normalises `..` away, which would silently re-address a
    /// DIFFERENT org. The CLI must refuse to ask, not rely on the 403.
    #[test]
    fn traversal_is_refused_before_a_url_is_built() {
        for bad in ["../evil/X", "..", ".", "a/../../b", "a//b", "", "/"] {
            assert!(Address::parse(bad).is_err(), "{bad:?} must not parse into an address");
        }
    }

    /// The server bans only `/` and control chars in a name, so `?`/`#`/`%` are
    /// LEGAL names — they must be encoded or we would address a different
    /// secret (or forge a query parameter) instead of asking for theirs.
    #[test]
    fn url_structural_characters_in_a_name_are_encoded_not_interpreted() {
        assert_eq!(Address::parse("X?env=prod").unwrap().wildcard(), "X%3Fenv%3Dprod");
        assert_eq!(Address::parse("A#B").unwrap().wildcard(), "A%23B");
        assert_eq!(Address::parse("a b").unwrap().wildcard(), "a%20b");
        assert_eq!(Address::parse("%2F").unwrap().wildcard(), "%252F");
        // The route's own separator survives; only the segments are encoded.
        assert_eq!(Address::parse("ci/D B").unwrap().wildcard(), "ci/D%20B");
    }

    #[test]
    fn an_env_cannot_forge_a_query_parameter() {
        assert_eq!(enc("prod&path=/orgs/other"), "prod%26path%3D%2Forgs%2Fother");
    }

    // ---- the value: stdin in, raw out --------------------------------------

    #[test]
    fn exactly_one_trailing_newline_is_stripped() {
        assert_eq!(read_value(&b"hunter2\n"[..]).unwrap(), "hunter2");
        assert_eq!(read_value(&b"hunter2\r\n"[..]).unwrap(), "hunter2");
        assert_eq!(read_value(&b"hunter2"[..]).unwrap(), "hunter2");
        // Only ONE: a value that really ends in a blank line keeps it.
        assert_eq!(read_value(&b"hunter2\n\n"[..]).unwrap(), "hunter2\n");
        // Interior and leading bytes are the value, never trimmed.
        assert_eq!(read_value(&b"  a b\nc  \n"[..]).unwrap(), "  a b\nc  ");
    }

    #[test]
    fn an_empty_value_is_refused_rather_than_stored() {
        assert!(read_value(&b""[..]).is_err());
        assert!(read_value(&b"\n"[..]).is_err());
    }

    // ---- the wire: bearer only, org from the token's own claim -------------

    #[tokio::test]
    async fn every_verb_addresses_the_active_identitys_own_org_and_sends_no_org_header() {
        let mock = MockCloud::start().await;
        let (s, tok) = session(&mock.base_url(), "hanzo");

        let addr = Address::parse("ci/DB").unwrap();
        let url = format!("{}/{}?env=prod", s.secrets_url(), addr.wildcard());
        s.call(Method::GET, &url, None).await.unwrap();
        s.call(Method::DELETE, &url, None).await.unwrap();
        s.call(Method::GET, &format!("{}?env=prod", s.secrets_url()), None).await.unwrap();
        s.call(
            Method::POST,
            &s.secrets_url(),
            Some(&json!({"path":"ci","name":"DB","env":"prod","value":"v"})),
        )
        .await
        .unwrap();

        let reqs = mock.requests();
        assert_eq!(reqs.len(), 4);
        for r in &reqs {
            // The org rides the PATH the route defines — and it is the `owner`
            // claim of the very token we authenticate with, never a user choice.
            assert!(
                r.path.starts_with("/v1/kms/orgs/hanzo/secrets"),
                "must address its own org: {}",
                r.path
            );
            assert_eq!(r.header("authorization"), Some(format!("Bearer {tok}")));
            // ... and NOTHING asserts an org out of band.
            for (k, _) in &r.headers {
                assert!(
                    !k.to_ascii_lowercase().contains("org"),
                    "the CLI must never send an org header, found {k}"
                );
            }
        }
        assert!(reqs[0].path.ends_with("/secrets/ci/DB?env=prod"));
        assert_eq!(reqs[3].json()["value"], "v");
    }

    /// Switching identity moves the secret namespace with zero new machinery —
    /// the org is a projection of the token, so there is nothing else to move.
    #[tokio::test]
    async fn the_org_follows_the_identity_not_a_flag() {
        let mock = MockCloud::start().await;
        for owner in ["hanzo", "admin"] {
            let (s, _) = session(&mock.base_url(), owner);
            s.call(Method::GET, &format!("{}?env=prod", s.secrets_url()), None).await.unwrap();
            assert_eq!(s.secrets_url(), format!("{}/v1/kms/orgs/{owner}/secrets", mock.base_url()));
        }
        let reqs = mock.requests();
        assert!(reqs[0].path.starts_with("/v1/kms/orgs/hanzo/"));
        assert!(reqs[1].path.starts_with("/v1/kms/orgs/admin/"));
    }

    /// A non-2xx is an error, never a silent success — a `set` that reports
    /// success while the value never landed is the worst outcome a secrets tool
    /// has.
    #[tokio::test]
    async fn a_refused_call_is_an_error_not_a_silent_success() {
        let mock = MockCloud::start_status(403).await;
        let (s, _) = session(&mock.base_url(), "hanzo");
        let r = s.call(Method::POST, &s.secrets_url(), Some(&json!({"name":"X"}))).await;
        assert!(r.is_err(), "403 must surface");
    }

    /// `list` prints the address `get` takes, so the two compose.
    #[test]
    fn a_listing_row_renders_the_address_get_accepts() {
        let root = "/orgs/hanzo";
        for (path, name, want) in [
            ("/orgs/hanzo", "DB", "DB"),
            ("/orgs/hanzo/ci", "DB", "ci/DB"),
            ("/orgs/hanzo/a/b", "K", "a/b/K"),
        ] {
            let rel = path.strip_prefix(root).unwrap_or(path).trim_matches('/');
            let got = if rel.is_empty() { name.to_string() } else { format!("{rel}/{name}") };
            assert_eq!(got, want);
            // ... and it round-trips back through the one parser.
            assert!(Address::parse(&got).is_ok());
        }
    }
}
