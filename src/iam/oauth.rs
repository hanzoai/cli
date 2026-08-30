//! The OIDC Authorization-Code-with-PKCE flow against Hanzo IAM (HIP-0111).
//!
//! `hanzo-cli` is a PUBLIC client (no secret): PKCE S256 is the proof. We bind
//! an ephemeral loopback port, send the browser to the brand's
//! `/v1/iam/oauth/authorize`, capture the redirect on `127.0.0.1`, then
//! exchange the code at `/v1/iam/oauth/token`. Only the explicit HIP-0111 paths
//! are ever used — no discovery, no legacy `/oauth/*`, no `/api/`.
//!
//! THE CODE COMES BACK TWO WAYS, and they race. `127.0.0.1` is only reachable
//! from the machine the CLI runs on, so a shell in a sandbox, a container or an
//! ssh session sends the browser to a loopback that belongs to a DIFFERENT
//! computer: the redirect lands on the desktop's own localhost, the tab shows a
//! connection error, and the CLI waits forever on a socket nothing will ever
//! dial. The person watching that has the code in their address bar the whole
//! time. So they can paste it — the SAME flow, the same client, the same PKCE
//! verifier, with the return leg switched from a socket to the keyboard.
//!
//! ONE command and no flag, because whichever way the code arrives it is the
//! same login and a person cannot tell in advance which will work: pressing
//! `--paste` is a decision they can only make correctly after the failure it
//! was meant to prevent.

use anyhow::{anyhow, bail, Context, Result};
use reqwest::Url;
use serde::Deserialize;
use std::io::IsTerminal;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;

use super::paths::{self, AUTHORIZE, REVOKE, TOKEN, USERINFO};
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

    // Say what happened, not what was attempted. On a machine with no browser
    // the open FAILS, and "Opening your browser..." above a prompt that never
    // returns is the CLI describing something the person can see did not occur.
    if webbrowser::open(authorize_url.as_str()).is_ok() {
        println!("Opening your browser to sign in to {brand}...");
        println!("If it does not open, visit:\n  {authorize_url}\n");
    } else {
        println!("Open this in a browser to sign in to {brand}:\n  {authorize_url}\n");
    }
    if std::io::stdin().is_terminal() {
        println!("Signed in on another machine? Paste the URL it lands on here.");
    }

    // Whichever leg answers first. The socket wins on a desktop, where it
    // returns before a person could paste anything; the keyboard wins where the
    // browser was somewhere else, which is the case that used to hang.
    let cb = tokio::select! {
        r = capture_callback(&listener, &state) => r?,
        r = paste_callback(&state) => r?,
    };
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
            "userinfo failed ({}): session may be expired — run `hanzo auth login`",
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

/// Exchange a refresh token for a fresh access token (RFC 6749 §6).
///
/// The access token IAM mints lives one hour. Without this the CLI holds a
/// refresh token it never spends, so every command an hour after login fails —
/// and fails CONFUSINGLY, because a stale token reads downstream as "X-Org-Id
/// required" or "a validated principal is required" rather than "log in again".
pub async fn refresh(origin: &str, refresh_token: &str) -> Result<TokenSet> {
    let resp = reqwest::Client::new()
        .post(paths::iam_url(origin, TOKEN))
        .form(&[
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token),
            ("client_id", CLIENT_ID),
        ])
        .send()
        .await
        .context("calling IAM token endpoint")?;
    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        bail!("token refresh failed ({status}): {body}");
    }
    serde_json::from_str::<TokenSet>(&body).context("parsing refresh response")
}

/// End the session AT THE SERVER (RFC 7009): revoking a refresh token deletes its
/// whole rotation family, so nothing can be minted from it again.
///
/// `logout` deletes the local copy; only this makes the credential stop working.
/// The distinction is not academic — the refresh token IAM issues this client
/// lives 30 days (provision `refreshExpireInHours: 720`), so a logout that only
/// forgets leaves a month of spendable access behind on a machine you signed out
/// of. Public client: `client_id` and the token are the whole request, which is
/// exactly what a client with no secret has to offer (RFC 6749 §3.2.1).
pub async fn revoke(origin: &str, refresh_token: &str) -> Result<()> {
    let resp = reqwest::Client::new()
        .post(paths::iam_url(origin, REVOKE))
        .form(&[
            ("token", refresh_token),
            ("token_type_hint", "refresh_token"),
            ("client_id", CLIENT_ID),
        ])
        .send()
        .await
        .context("calling IAM revocation endpoint")?;
    let status = resp.status();
    if !status.is_success() {
        bail!("revocation failed ({status}): {}", resp.text().await.unwrap_or_default());
    }
    Ok(())
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

/// The authorization code, off the keyboard.
///
/// It accepts either shape a person can copy: the WHOLE redirect URL out of the
/// address bar (which is where it already is when the loopback fails), or the
/// bare `code` value. A malformed line is answered and the read continues —
/// bailing would end a login the socket might still complete, and the paste is
/// the leg that was already having a bad time.
async fn paste_callback(state: &str) -> Result<Callback> {
    // A pipe is not a person. Its EOF arrives at once, and resolving this side
    // of the race on it would end the login before the browser could answer, so
    // a non-terminal stdin simply never returns and the socket decides.
    if !std::io::stdin().is_terminal() {
        return std::future::pending().await;
    }

    let mut lines = BufReader::new(tokio::io::stdin()).lines();
    while let Some(line) = lines.next_line().await? {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let cb = parse_pasted(line);
        if let Some(err) = cb.error {
            bail!("authorization denied: {err}");
        }
        // State rides along only when the whole URL did. Present, it MUST match
        // — that is the bind to this attempt. Absent, the person typed a code
        // into this process by hand and PKCE is what binds the exchange: the
        // verifier never left here, so a code lifted from someone else's login
        // cannot be spent by us and ours cannot be spent by them.
        match (&cb.state, &cb.code) {
            (Some(got), _) if got != state => {
                bail!("state mismatch — that code belongs to a different sign-in; aborting")
            }
            (_, Some(_)) => return Ok(cb),
            _ => println!("No code in that. Paste the whole URL from the browser, or just the code."),
        }
    }
    // stdin closed under us; leave the socket to it.
    std::future::pending().await
}

/// Pull `code`/`state`/`error` out of whatever a person pasted: a full URL, the
/// `/callback?...` target, a bare query string, or the code alone. Pure — no I/O.
fn parse_pasted(input: &str) -> Callback {
    // Quotes come along when a URL is copied out of some terminals and chats.
    let s = input.trim().trim_matches(|c| c == '"' || c == '\'');
    let query = match s.find('?') {
        Some(i) => &s[i + 1..],
        // No `?` at all: a bare query string still has `code=`, anything else is
        // the code itself.
        None if s.contains('=') => s,
        None => {
            return Callback {
                code: Some(s.to_string()),
                ..Callback::default()
            }
        }
    };
    let mut cb = Callback::default();
    for (k, v) in Url::parse(&format!("http://127.0.0.1/?{query}"))
        .iter()
        .flat_map(|u| u.query_pairs().map(|(k, v)| (k.into_owned(), v.into_owned())))
    {
        match k.as_str() {
            "code" => cb.code = Some(v),
            "state" => cb.state = Some(v),
            "error" => cb.error = Some(v),
            _ => {}
        }
    }
    cb
}

/// Accept exactly one loopback request, reply with a friendly page, and return
/// the parsed callback. Errors if the provider reported `error=...`, or if the
/// redirect does not carry back the `state` this login sent.
async fn capture_callback(listener: &TcpListener, state: &str) -> Result<Callback> {
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
    // A browser always sends back what we put in the authorize URL, so here —
    // unlike a hand-typed code — an absent state is as wrong as a wrong one.
    if cb.state.as_deref() != Some(state) {
        bail!("state mismatch — possible CSRF; aborting login");
    }
    Ok(cb)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn server_url_known_and_unknown() {
        assert_eq!(server_url("hanzo").unwrap(), "https://hanzo.id");
        assert_eq!(server_url("lux").unwrap(), "https://lux.id");
        assert_eq!(server_url("zoo").unwrap(), "https://zoo.id");
        assert!(server_url("bogus").is_err());
    }

    #[test]
    fn authorize_url_is_hip0111_pkce_s256() {
        let url = build_authorize_url(
            "https://hanzo.id",
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
        let server = tokio::spawn(async move { capture_callback(&listener, "xyz").await });

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

    // A browser sends back what we sent it, so on THIS leg an absent state is
    // as wrong as a wrong one.
    #[tokio::test]
    async fn loopback_refuses_a_state_that_is_not_ours() {
        use tokio::net::TcpStream;

        for target in ["/callback?code=abc&state=SOMEONE_ELSE", "/callback?code=abc"] {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();
            let server = tokio::spawn(async move { capture_callback(&listener, "xyz").await });

            let mut client = TcpStream::connect(addr).await.unwrap();
            client
                .write_all(format!("GET {target} HTTP/1.1\r\nHost: localhost\r\n\r\n").as_bytes())
                .await
                .unwrap();
            let mut sink = Vec::new();
            let _ = client.read_to_end(&mut sink).await;

            assert!(server.await.unwrap().is_err(), "{target} was accepted");
        }
    }

    // The four shapes a person can actually paste. The first is the one that
    // matters: it is what sits in the address bar when the loopback is on
    // another machine, which is the whole reason this leg exists.
    #[test]
    fn a_paste_is_read_from_every_shape_a_person_can_copy() {
        let whole = parse_pasted("http://127.0.0.1:51394/callback?code=abc&state=xyz");
        assert_eq!(whole.code.as_deref(), Some("abc"));
        assert_eq!(whole.state.as_deref(), Some("xyz"));

        let target = parse_pasted("/callback?code=abc&state=xyz");
        assert_eq!(target.code.as_deref(), Some("abc"));
        assert_eq!(target.state.as_deref(), Some("xyz"));

        let query = parse_pasted("code=abc&state=xyz");
        assert_eq!(query.code.as_deref(), Some("abc"));
        assert_eq!(query.state.as_deref(), Some("xyz"));

        // The code alone carries no state, and that is not an error — PKCE is
        // what binds it, and the verifier never left this process.
        let bare = parse_pasted("  abc  ");
        assert_eq!(bare.code.as_deref(), Some("abc"));
        assert!(bare.state.is_none());
    }

    #[test]
    fn a_paste_decodes_and_survives_copy_noise() {
        // Quotes ride along out of terminals and chat clients.
        let quoted = parse_pasted("\"http://127.0.0.1:1/callback?code=the%2Bcode&state=xyz\"");
        assert_eq!(quoted.code.as_deref(), Some("the+code")); // %2B -> +
        assert_eq!(quoted.state.as_deref(), Some("xyz"));

        // A denial pasted back is a denial, not a code.
        let denied = parse_pasted("http://127.0.0.1:1/callback?error=access_denied");
        assert_eq!(denied.error.as_deref(), Some("access_denied"));
        assert!(denied.code.is_none());
    }
}
