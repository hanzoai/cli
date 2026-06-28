//! The OIDC Authorization-Code-with-PKCE flow against Hanzo IAM (HIP-0111).
//!
//! `hanzo-cli` is a PUBLIC client (no secret): PKCE S256 is the proof. We bind
//! an ephemeral loopback port, send the browser to the brand's
//! `/v1/iam/oauth/authorize`, capture the redirect on `127.0.0.1`, then
//! exchange the code at `/v1/iam/oauth/token`. Only the explicit HIP-0111 paths
//! are ever used — no discovery, no legacy `/oauth/*`, no `/api/`.

use anyhow::{anyhow, bail, Context, Result};
use reqwest::Url;
use serde::Deserialize;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

use super::paths::{self, AUTHORIZE, TOKEN, USERINFO};
use super::pkce;
use super::token::TokenSet;

/// The CLI's registered IAM client id (`<org>-<app>`). Public client.
pub const CLIENT_ID: &str = "hanzo-cli";
/// OIDC scopes — identity only.
pub const SCOPE: &str = "openid profile email";

/// The subset of OIDC UserInfo (§5.3) the CLI displays.
#[derive(Debug, Deserialize)]
pub struct UserInfo {
    pub sub: String,
    #[serde(default)]
    pub email: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub preferred_username: Option<String>,
}

/// Resolve a brand to its IAM origin, or error with the known set.
pub fn server_url(brand: &str) -> Result<&'static str> {
    paths::server_url_for_brand(brand).ok_or_else(|| {
        anyhow!("unknown brand '{brand}' (expected one of: hanzo, lux, zoo, pars, bootnode)")
    })
}

/// Run the full interactive login flow for `brand` and return the tokens.
pub async fn login(brand: &str) -> Result<TokenSet> {
    let origin = server_url(brand)?;
    let pkce = pkce::generate_pkce();
    let state = pkce::generate_state();

    // Bind the loopback callback FIRST so the port is known for redirect_uri.
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .context("binding loopback callback server")?;
    let port = listener.local_addr()?.port();
    let redirect_uri = format!("http://127.0.0.1:{port}/callback");

    let authorize_url = build_authorize_url(origin, &redirect_uri, &pkce.challenge, &state)?;

    println!("Opening your browser to sign in to {brand}...");
    println!("If it does not open, visit:\n  {authorize_url}\n");
    let _ = webbrowser::open(authorize_url.as_str());

    let cb = capture_callback(&listener).await?;
    if cb.state.as_deref() != Some(state.as_str()) {
        bail!("state mismatch — possible CSRF; aborting login");
    }
    let code = cb
        .code
        .ok_or_else(|| anyhow!("no authorization code in callback"))?;

    exchange_code(origin, &code, &redirect_uri, &pkce.verifier).await
}

/// Fetch the userinfo profile for an access token.
pub async fn userinfo(brand: &str, access_token: &str) -> Result<UserInfo> {
    let origin = server_url(brand)?;
    let resp = reqwest::Client::new()
        .get(paths::iam_url(origin, USERINFO))
        .bearer_auth(access_token)
        .send()
        .await
        .context("calling IAM userinfo")?;
    if !resp.status().is_success() {
        bail!(
            "userinfo failed ({}): session may be expired — run `hanzo login`",
            resp.status()
        );
    }
    resp.json::<UserInfo>()
        .await
        .context("parsing userinfo response")
}

/// Build the `/v1/iam/oauth/authorize` URL with PKCE S256 query parameters.
/// Split out from [`login`] so the URL shape is unit-testable without I/O.
fn build_authorize_url(
    origin: &str,
    redirect_uri: &str,
    challenge: &str,
    state: &str,
) -> Result<Url> {
    Url::parse_with_params(
        &paths::iam_url(origin, AUTHORIZE),
        &[
            ("response_type", "code"),
            ("client_id", CLIENT_ID),
            ("redirect_uri", redirect_uri),
            ("scope", SCOPE),
            ("state", state),
            ("code_challenge", challenge),
            ("code_challenge_method", "S256"),
        ],
    )
    .context("building authorize URL")
}

/// Exchange an authorization code for tokens (RFC 6749 §4.1.3 + PKCE §4.5).
async fn exchange_code(
    origin: &str,
    code: &str,
    redirect_uri: &str,
    verifier: &str,
) -> Result<TokenSet> {
    let resp = reqwest::Client::new()
        .post(paths::iam_url(origin, TOKEN))
        .form(&[
            ("grant_type", "authorization_code"),
            ("code", code),
            ("redirect_uri", redirect_uri),
            ("client_id", CLIENT_ID),
            ("code_verifier", verifier),
        ])
        .send()
        .await
        .context("calling IAM token endpoint")?;
    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        bail!("token exchange failed ({status}): {body}");
    }
    serde_json::from_str::<TokenSet>(&body).context("parsing token response")
}

/// The OAuth parameters carried back on the loopback redirect.
#[derive(Debug, Default)]
struct Callback {
    code: Option<String>,
    state: Option<String>,
    error: Option<String>,
}

/// Parse `code`/`state`/`error` from a redirect target like
/// `/callback?code=...&state=...` (handles percent-decoding). Pure — no I/O.
fn parse_callback(target: &str) -> Result<Callback> {
    let parsed =
        Url::parse(&format!("http://127.0.0.1{target}")).context("parsing callback URL")?;
    let mut cb = Callback::default();
    for (k, v) in parsed.query_pairs() {
        match k.as_ref() {
            "code" => cb.code = Some(v.into_owned()),
            "state" => cb.state = Some(v.into_owned()),
            "error" => cb.error = Some(v.into_owned()),
            _ => {}
        }
    }
    Ok(cb)
}

/// Accept exactly one loopback request, reply with a friendly page, and return
/// the parsed callback. Errors if the provider reported `error=...`.
async fn capture_callback(listener: &TcpListener) -> Result<Callback> {
    let (mut stream, _) = listener
        .accept()
        .await
        .context("accepting loopback callback")?;

    // The request line (`GET /callback?... HTTP/1.1`) is always in the first
    // segment of a browser navigation, so a single read suffices.
    let mut buf = vec![0u8; 8192];
    let n = stream.read(&mut buf).await?;
    let request = String::from_utf8_lossy(&buf[..n]);
    let target = request
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .ok_or_else(|| anyhow!("malformed callback request"))?;

    let cb = parse_callback(target)?;

    let (status_line, message) = if let Some(err) = &cb.error {
        ("400 Bad Request", format!("Sign-in failed: {err}."))
    } else if cb.code.is_some() {
        ("200 OK", "Signed in to Hanzo.".to_string())
    } else {
        ("400 Bad Request", "Missing authorization code.".to_string())
    };
    let html = format!(
        "<!doctype html><meta charset=utf-8><title>Hanzo</title>\
         <body style=\"font-family:system-ui;text-align:center;padding-top:3rem\">\
         <h2>{message}</h2><p>You can close this tab.</p></body>"
    );
    let response = format!(
        "HTTP/1.1 {status_line}\r\nContent-Type: text/html; charset=utf-8\r\n\
         Content-Length: {}\r\nConnection: close\r\n\r\n{html}",
        html.len()
    );
    let _ = stream.write_all(response.as_bytes()).await;
    let _ = stream.flush().await;

    if let Some(err) = cb.error {
        bail!("authorization denied: {err}");
    }
    Ok(cb)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn server_url_known_and_unknown() {
        assert_eq!(server_url("hanzo").unwrap(), "https://iam.hanzo.ai");
        assert_eq!(server_url("lux").unwrap(), "https://lux.id");
        assert_eq!(server_url("zoo").unwrap(), "https://zoo.id");
        assert!(server_url("bogus").is_err());
    }

    #[test]
    fn authorize_url_is_hip0111_pkce_s256() {
        let url = build_authorize_url(
            "https://iam.hanzo.ai",
            "http://127.0.0.1:54321/callback",
            "CHALLENGE",
            "STATE",
        )
        .unwrap();
        // Exact HIP-0111 path — never /api/, never legacy /oauth/authorize.
        assert_eq!(url.path(), "/v1/iam/oauth/authorize");
        let q: HashMap<_, _> = url.query_pairs().into_owned().collect();
        assert_eq!(q["response_type"], "code");
        assert_eq!(q["client_id"], CLIENT_ID);
        assert_eq!(q["code_challenge_method"], "S256");
        assert_eq!(q["code_challenge"], "CHALLENGE");
        assert_eq!(q["state"], "STATE");
        assert_eq!(q["scope"], SCOPE);
        assert_eq!(q["redirect_uri"], "http://127.0.0.1:54321/callback");
    }

    #[test]
    fn parse_callback_decodes_code_and_state() {
        let cb = parse_callback("/callback?code=the%2Bcode&state=xyz").unwrap();
        assert_eq!(cb.code.as_deref(), Some("the+code")); // %2B -> +
        assert_eq!(cb.state.as_deref(), Some("xyz"));
        assert!(cb.error.is_none());
    }

    #[test]
    fn parse_callback_surfaces_provider_error() {
        let cb = parse_callback("/callback?error=access_denied").unwrap();
        assert_eq!(cb.error.as_deref(), Some("access_denied"));
        assert!(cb.code.is_none());
    }

    // Drive the real loopback server over a TCP socket: it must extract the
    // code/state from the redirect and reply with a 200 the browser can show.
    #[tokio::test]
    async fn loopback_captures_code_and_replies_ok() {
        use tokio::net::TcpStream;

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { capture_callback(&listener).await });

        let mut client = TcpStream::connect(addr).await.unwrap();
        client
            .write_all(b"GET /callback?code=abc&state=xyz HTTP/1.1\r\nHost: localhost\r\n\r\n")
            .await
            .unwrap();

        let mut response = Vec::new();
        client.read_to_end(&mut response).await.unwrap();
        let response = String::from_utf8_lossy(&response);
        assert!(response.starts_with("HTTP/1.1 200 OK"), "got: {response}");

        let cb = server.await.unwrap().unwrap();
        assert_eq!(cb.code.as_deref(), Some("abc"));
        assert_eq!(cb.state.as_deref(), Some("xyz"));
    }
}
