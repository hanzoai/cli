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
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

use super::identity::{self, Identity};
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

/// Who the signed-in page names. `label` is the address a person recognises
/// (an email, else a name); `id` is `owner/name`, the billing identity.
#[derive(Clone)]
pub(crate) struct Shown {
    pub label: String,
    pub id: String,
}

/// The loopback, still listening, after the signed-in page has been written.
/// Login keeps answering retries on it while the identity is saved, then hands
/// it to a short-lived process for the account menu.
pub(crate) struct Held {
    listener: TcpListener,
    origin: String,
    brand: String,
    page: String,
}

pub(crate) enum Done {
    Pasted(TokenSet),
    Browser(TokenSet, Held),
}

/// Run the full interactive login flow for `brand` and return the tokens.
/// `choose` asks IAM to let the person pick among the accounts signed in on
/// this browser, or sign in to another, rather than reusing the latest one.
/// `others` are identities already on this machine; the signed-in page lists
/// them so a person can see which account this one is.
pub async fn login(brand: &str, choose: bool, others: &[Shown]) -> Result<Done> {
    let origin = server_url(brand)?;
    let pkce = pkce::generate_pkce();
    let state = pkce::generate_state();

    // Bind the loopback callback FIRST so the port is known for redirect_uri.
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .context("binding loopback callback server")?;
    let port = listener.local_addr()?.port();
    let redirect_uri = format!("http://127.0.0.1:{port}/callback");

    let authorize_url =
        build_authorize_url(origin, &redirect_uri, &pkce.challenge, &state, choose)?;

    // Print the link BEFORE asking the OS to open it. `open` on this machine
    // can sit there until a browser answers, and that wait used to be the
    // whole of `hanzo login`: no line, no URL, nothing to paste.
    println!("Opening your browser to sign in to {brand}...");
    println!("If it does not open, visit:\n  {authorize_url}\n");
    if std::io::stdin().is_terminal() {
        println!("Signed in on another machine? Paste the URL it lands on here.");
    }
    let _ = std::io::Write::flush(&mut std::io::stdout());
    let url_for_open = authorize_url.to_string();
    std::thread::spawn(move || {
        let _ = webbrowser::open(&url_for_open);
    });

    // Whichever leg answers first. The socket wins on a desktop, where it
    // returns before a person could paste anything; the keyboard wins where the
    // browser was somewhere else, which is the case that used to hang.
    // The browser's document request is held until the token names the account:
    // answering before that is how the tab could only say "signed in" with no
    // one attached to it.
    let first = tokio::select! {
        r = capture_callback(&listener, &state, origin) => Arrival::Browser(r?),
        r = paste_callback(&state) => Arrival::Pasted(r?),
    };
    let (stream, code, verifier) = match first {
        Arrival::Pasted(cb) => {
            let code = cb
                .code
                .ok_or_else(|| anyhow!("no authorization code in callback"))?;
            return Ok(Done::Pasted(
                exchange_code(origin, &code, &redirect_uri, &pkce.verifier).await?,
            ));
        }
        Arrival::Browser((cb, stream)) => {
            let code = cb
                .code
                .ok_or_else(|| anyhow!("no authorization code in callback"))?;
            (stream, code, pkce.verifier)
        }
    };

    // The token exchange takes a network round trip. Safari does not sit on the
    // first socket for that long: it retries the redirect, and the page has to
    // land on whichever connection the tab is actually waiting on. Answering
    // the first one and then closing the port is the "Can't connect to the
    // server" page.
    let mut waiting = vec![stream];
    let mut exchange = std::pin::pin!(exchange_code(origin, &code, &redirect_uri, &verifier));
    let tokens = loop {
        tokio::select! {
            result = &mut exchange => {
                break match result {
                    Ok(tokens) => tokens,
                    Err(err) => {
                        let why = err.to_string();
                        let failure = conclusion(origin, Some(&why));
                        for stream in &mut waiting {
                            reply(stream, &failure).await;
                        }
                        return Err(err);
                    }
                };
            }
            incoming = listener.accept() => {
                let Ok((mut extra, _)) = incoming else { continue };
                if hold_for_page(&mut extra).await {
                    waiting.push(extra);
                }
            }
        }
    };
    // Claims only. Userinfo is another round trip, and holding the document
    // for it is what made Safari abandon the tab before any byte arrived.
    let who = shown_account(&tokens);
    let page = html_ok(&signed_in_page(origin, who.as_ref(), others, None, true));
    for stream in &mut waiting {
        reply(stream, &page).await;
    }
    Ok(Done::Browser(
        tokens,
        Held {
            listener,
            origin: origin.to_string(),
            brand: brand.to_string(),
            page,
        },
    ))
}

/// Answer reloads of the signed-in page while `work` runs (saving the
/// identity), then hand the port to the menu process. `biased` so a finished
/// save is not stuck behind a browser that keeps connecting.
pub(crate) async fn cover_then_detach<T>(
    held: Held,
    work: impl std::future::Future<Output = Result<T>>,
) -> Result<T> {
    let mut work = std::pin::pin!(work);
    let out = loop {
        tokio::select! {
            biased;
            result = &mut work => break result?,
            incoming = held.listener.accept() => {
                let Ok((mut stream, _)) = incoming else { continue };
                answer_held(&mut stream, &held.page).await;
            }
        }
    };
    detach_menu(held.listener, &held.origin, &held.brand);
    Ok(out)
}

async fn answer_held(stream: &mut TcpStream, page: &str) {
    let request = match tokio::time::timeout(
        std::time::Duration::from_millis(400),
        read_request(stream),
    )
    .await
    {
        Ok(Ok(body)) => body,
        _ => {
            let _ = stream.shutdown().await;
            return;
        }
    };
    let target = request_target(&request).unwrap_or("");
    if target.starts_with("/callback") || target.starts_with("/switch") {
        reply(stream, page).await;
    } else {
        reply(stream, EMPTY_OK).await;
    }
}

/// Identities already on this machine, labeled from each token's own email
/// claim. No network: the login page has to paint without another round trip.
pub(crate) fn known_accounts(cfg: &crate::config::Config, brand: &str) -> Vec<Shown> {
    super::store::list(cfg, brand)
        .into_iter()
        .map(|id| {
            let label = super::store::token_for(cfg, brand, &id)
                .ok()
                .flatten()
                .map(|tokens| account_label(&tokens, &id))
                .unwrap_or_else(|| id.to_string());
            Shown {
                label,
                id: id.to_string(),
            }
        })
        .collect()
}

enum Arrival {
    Browser((Callback, TcpStream)),
    Pasted(Callback),
}

/// The account a token belongs to, from the token's own claims. No network:
/// the browser tab is waiting on the socket this returns for.
fn shown_account(tokens: &TokenSet) -> Option<Shown> {
    let id = Identity::from_access_token(&tokens.access_token).ok()?;
    let label = account_label(tokens, &id);
    Some(Shown { label, id: id.to_string() })
}

/// What a person recognises this token as: its email, else its display name,
/// else the `owner/name` the claims themselves carry.
fn account_label(tokens: &TokenSet, id: &Identity) -> String {
    identity::email(&tokens.access_token)
        .or_else(|| identity::display(&tokens.access_token))
        .unwrap_or_else(|| id.to_string())
}

/// A socket that arrived while the token exchange was still in flight. The
/// login redirect is held so the signed-in page can be written on it. Anything
/// else (the favicon) is answered now so the tab can stop spinning.
async fn hold_for_page(stream: &mut TcpStream) -> bool {
    let request = match tokio::time::timeout(
        std::time::Duration::from_millis(400),
        read_request(stream),
    )
    .await
    {
        Ok(Ok(body)) if !body.is_empty() => body,
        _ => {
            let _ = stream.shutdown().await;
            return false;
        }
    };
    let target = request_target(&request).unwrap_or("");
    if target.starts_with("/callback") {
        return true;
    }
    reply(stream, EMPTY_OK).await;
    false
}

/// Keep answering the loopback until `for_how_long` has passed, measured from
/// the call. A `/callback` gets the signed-in page again. Anything else is an
/// empty 204. New connections do not extend the clock.
#[cfg(test)]
async fn serve_signed_in(listener: &TcpListener, page: &str, for_how_long: std::time::Duration) {
    let deadline = tokio::time::Instant::now() + for_how_long;
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            break;
        }
        let accepted = tokio::time::timeout(remaining, listener.accept()).await;
        let Ok(Ok((mut stream, _))) = accepted else { break };
        let request = match tokio::time::timeout(
            std::time::Duration::from_millis(400),
            read_request(&mut stream),
        )
        .await
        {
            Ok(Ok(body)) => body,
            _ => {
                let _ = stream.shutdown().await;
                continue;
            }
        };
        let target = request_target(&request).unwrap_or("");
        if target.starts_with("/callback") {
            reply(&mut stream, page).await;
        } else {
            reply(&mut stream, EMPTY_OK).await;
        }
    }
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
    choose: bool,
) -> Result<Url> {
    let mut q = vec![
        ("response_type", "code"),
        ("client_id", CLIENT_ID),
        ("redirect_uri", redirect_uri),
        ("scope", SCOPE),
        ("state", state),
        ("code_challenge", challenge),
        ("code_challenge_method", "S256"),
    ];
    if choose {
        q.push(("prompt", "select_account"));
    }
    Url::parse_with_params(&paths::iam_url(origin, AUTHORIZE), &q).context("building authorize URL")
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
        bail!(
            "revocation failed ({status}): {}",
            resp.text().await.unwrap_or_default()
        );
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

    // THE READ ENDS WITH THE LOGIN. On a desktop the socket wins and this side
    // is dropped mid-read; `tokio::io::stdin()` left that read parked on the
    // runtime's blocking pool, and the runtime waits for its blocking threads
    // before the process can exit — so `hanzo login` printed "Signed in" and
    // then sat there until someone pressed Enter, and that Enter was swallowed.
    // `typed` reads only a line that is already waiting, on its own thread, and
    // `_stop` ends it the moment this future is dropped.
    let (tx, mut lines) = tokio::sync::mpsc::unbounded_channel();
    let stop = Arc::new(AtomicBool::new(false));
    let _stop = Stop(Arc::clone(&stop));
    std::thread::spawn(move || typed(&stop, &tx));
    while let Some(line) = lines.recv().await {
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
            _ => {
                println!("No code in that. Paste the whole URL from the browser, or just the code.")
            }
        }
    }
    // stdin closed under us; leave the socket to it.
    std::future::pending().await
}

/// Sets its flag when dropped: the paste leg is over, whoever won.
struct Stop(Arc<AtomicBool>);

impl Drop for Stop {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Relaxed);
    }
}

/// Lines typed at the terminal, until `stop` is set or stdin ends.
///
/// A line is read only once the terminal says one is waiting. A canonical-mode
/// tty is readable when a whole line is, so the read returns at once, and no read
/// is ever left pending for the next thing that asks the terminal for a line.
fn typed(stop: &AtomicBool, tx: &tokio::sync::mpsc::UnboundedSender<String>) {
    let stdin = std::io::stdin();
    while !stop.load(Ordering::Relaxed) {
        if !waiting(std::time::Duration::from_millis(100)) {
            continue;
        }
        let mut line = String::new();
        match stdin.read_line(&mut line) {
            Ok(0) | Err(_) => return,
            Ok(_) => {
                if tx.send(line).is_err() {
                    return;
                }
            }
        }
    }
}

/// Whether stdin has a line to read within `wait`.
#[cfg(unix)]
fn waiting(wait: std::time::Duration) -> bool {
    let mut fd = libc::pollfd { fd: libc::STDIN_FILENO, events: libc::POLLIN, revents: 0 };
    // SAFETY: one pollfd on the stack, for the length we pass.
    unsafe { libc::poll(&mut fd, 1, wait.as_millis() as libc::c_int) > 0 }
}

/// Without poll the read blocks, on this thread alone; the stop flag is seen
/// after the next line, and the runtime never waits on it.
#[cfg(not(unix))]
fn waiting(_: std::time::Duration) -> bool {
    true
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
        .flat_map(|u| {
            u.query_pairs()
                .map(|(k, v)| (k.into_owned(), v.into_owned()))
        })
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

/// An empty, finished response. Anything that is not the login redirect (the
/// favicon, above all) gets this, so the tab can stop spinning.
const EMPTY_OK: &str = "HTTP/1.1 204 No Content\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";

/// Read one HTTP request, stopping at the header terminator. A browser writes
/// the request in one segment almost always; the loop is for the time it does not.
async fn read_request(stream: &mut TcpStream) -> Result<String> {
    let mut buf = Vec::new();
    let mut tmp = [0u8; 2048];
    loop {
        let n = stream.read(&mut tmp).await?;
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&tmp[..n]);
        if buf.windows(4).any(|w| w == b"\r\n\r\n") || buf.len() > 16_384 {
            break;
        }
    }
    Ok(String::from_utf8_lossy(&buf).into_owned())
}

fn request_target(request: &str) -> Option<&str> {
    request.lines().next()?.split_whitespace().nth(1)
}

/// Accept loopback requests until the login redirect arrives, and answer that
/// redirect before returning. A favicon (or anything else) that shows up first
/// is answered and skipped: taking it for the callback is how the real
/// navigation was left hanging. Errors if the provider reported `error=...`,
/// if there is no code, or if the redirect does not carry back the `state`
/// this login sent.
async fn capture_callback(
    listener: &TcpListener,
    state: &str,
    origin: &str,
) -> Result<(Callback, TcpStream)> {
    loop {
        let (mut stream, _) = listener
            .accept()
            .await
            .context("accepting loopback callback")?;
        // A connection that never sends a request (a speculative socket) must
        // not sit here for the life of the login. Drop it and keep waiting
        // for the redirect.
        let request = match tokio::time::timeout(std::time::Duration::from_secs(3), read_request(&mut stream)).await {
            Ok(Ok(body)) => body,
            _ => {
                let _ = stream.shutdown().await;
                continue;
            }
        };
        let target = request_target(&request).unwrap_or("");
        if !target.starts_with("/callback") {
            reply(&mut stream, EMPTY_OK).await;
            continue;
        }

        let cb = parse_callback(target)?;

        // A browser always sends back what we put in the authorize URL, so here —
        // unlike a hand-typed code — an absent state is as wrong as a wrong one.
        let refusal = if let Some(err) = &cb.error {
            Some(format!("authorization denied: {err}"))
        } else if cb.code.is_none() {
            Some("no authorization code in callback".to_string())
        } else if cb.state.as_deref() != Some(state) {
            Some("state mismatch — possible CSRF; aborting login".to_string())
        } else {
            None
        };
        if let Some(why) = refusal {
            reply(&mut stream, &conclusion(origin, Some(&why))).await;
            bail!(why);
        }
        // The document stays unanswered until the token says who this is.
        // The caller writes the page onto this stream.
        return Ok((cb, stream));
    }
}

/// Write one HTTP response and close. A browser that has gone away is not an
/// error the login needs to hear about.
async fn reply(stream: &mut TcpStream, response: &str) {
    let _ = stream.write_all(response.as_bytes()).await;
    let _ = stream.flush().await;
    // FIN, not a drop the process gets around to later. A browser that received
    // Content-Length still keeps the tab spinning until the connection closes.
    let _ = stream.shutdown().await;
}

/// How the browser tab ends a login. Success stays on the loopback and shows
/// a finished page: the account home that used to receive this redirect renders
/// as a blank white sheet, and a person who just signed in from the terminal
/// wants confirmation they can close, not another site. Failure stays here too
/// and says why. Both are in the brand's colours. Pure — no I/O.
fn html_ok(body: &str) -> String {
    format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\n\
         Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
}

fn conclusion(origin: &str, failure: Option<&str>) -> String {
    let origin_trim = origin.trim_end_matches('/');
    let host = escape(origin_trim.split("://").nth(1).unwrap_or(origin_trim));
    match failure {
        None => {
            let body = signed_in_page(origin, None, &[] as &[Shown], None, false);
            html_ok(&body)
        }
        Some(why) => {
            let why = escape(why);
            let body = format!(
                "<!doctype html><meta charset=utf-8><meta name=color-scheme content=dark>\
                 <title>Sign-in failed — {host}</title>\
                 <body style=\"margin:0;min-height:100vh;display:grid;place-items:center;background:#000;color:#fff;\
                 font-family:Inter,ui-sans-serif,system-ui,sans-serif\">\
                 <main style=\"max-width:28rem;padding:2rem;text-align:center\">\
                 <p style=\"font-size:.875rem;letter-spacing:.08em;text-transform:uppercase;color:#a1a1aa\">{host}</p>\
                 <h1 style=\"font-size:1.5rem;font-weight:600;margin:.5rem 0 1rem\">Sign-in failed</h1>\
                 <p style=\"color:#d4d4d8;line-height:1.5\">{why}</p>\
                 <p style=\"color:#a1a1aa\">Run <code style=\"color:#fff\">hanzo login</code> again.</p></main></body>"
            );
            format!(
                "HTTP/1.1 400 Bad Request\r\nContent-Type: text/html; charset=utf-8\r\n\
                 Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            )
        }
    }
}

/// The Hanzo block-H at the size hanzo.id draws it in the footer: 24px, no
/// baked-in square, so it reads white on this page's dark ground.
const HANZO_MARK: &str = concat!(
    r##"<svg width="24" height="24" viewBox="0 0 67 67" fill="#f4f4f5" aria-hidden="true">"##,
    r#"<path d="M22.21 67V44.6369H0V67H22.21Z"/>"#,
    r#"<path d="M66.7038 22.3184H22.2534L0.0878906 44.6367H44.4634L66.7038 22.3184Z"/>"#,
    r#"<path d="M22.21 0H0V22.3184H22.21V0Z"/>"#,
    r#"<path d="M66.7198 0H44.5098V22.3184H66.7198V0Z"/>"#,
    r#"<path d="M66.7198 67V44.6369H44.5098V67H66.7198Z"/>"#,
    "</svg>"
);

/// "lux.id" → "Lux ID". hanzo.id does not use this: that corner says "Hanzo AI".
fn id_label(host: &str) -> String {
    let short = host.split('.').next().unwrap_or(host);
    let mut chars = short.chars();
    match chars.next() {
        Some(first) => format!("{}{} ID", first.to_uppercase(), chars.as_str()),
        None => "ID".to_string(),
    }
}

/// The page a finished `hanzo login` leaves in the browser. Same placement as
/// hanzo.id: a name absolute at the top left, the message in the middle, the
/// block-H centered at the bottom. On this loopback the name is "Hanzo AI" and
/// it opens https://hanzo.ai — this page is not the ID product. The account
/// sits only in the top right. Clicking it opens the other identities on
/// this machine; choosing one is `/switch`. The center message does not
/// repeat the name. Self-contained; the loopback has no stylesheet.
fn signed_in_page(
    origin: &str,
    who: Option<&Shown>,
    others: &[Shown],
    confirm_remove: Option<&str>,
    manage: bool,
) -> String {
    let origin = origin.trim_end_matches('/');
    let host = escape(origin.split("://").nth(1).unwrap_or(origin));
    let (corner, mark) = if host == "hanzo.id" {
        (
            "<a href=\"https://hanzo.ai\" style=\"position:absolute;top:24px;left:24px;margin:0;\
             font-size:19px;font-weight:600;letter-spacing:-0.4px;line-height:1;color:#f4f4f5;\
             text-decoration:none;cursor:pointer\">Hanzo AI</a>"
                .to_string(),
            format!(
                "<a href=\"https://hanzo.ai\" style=\"display:inline-flex;color:inherit;text-decoration:none;cursor:pointer\">{HANZO_MARK}</a>"
            ),
        )
    } else {
        (
            format!(
                "<p style=\"position:absolute;top:24px;left:24px;margin:0;font-size:19px;font-weight:600;\
                 letter-spacing:-0.4px;line-height:1\">{}</p>",
                id_label(&host)
            ),
            String::new(),
        )
    };
    let account = match who {
        Some(who) => account_menu(who, others, confirm_remove),
        None if manage => add_account_button(),
        None => String::new(),
    };
    let (heading, detail) = if who.is_none() && manage {
        ("Signed out", "Add an account to keep going.")
    } else {
        (
            "You're signed in",
            "Return to the terminal.<br>You can close this tab.",
        )
    };
    format!(
        "<!doctype html><html lang=en><meta charset=utf-8><meta name=color-scheme content=dark>\
         <meta name=viewport content=\"width=device-width,initial-scale=1\"><title>Signed in — {host}</title>\
         <body style=\"margin:0;min-height:100vh;min-height:100dvh;background:#070709;color:#f4f4f5;position:relative;\
         display:flex;flex-direction:column;font-family:Zen,Inter,ui-sans-serif,system-ui,-apple-system,sans-serif\">\
         <style>summary::-webkit-details-marker,summary::marker{{display:none}}summary{{list-style:none}}.hz summary:hover .pill{{background:#18181b}}.row:hover{{background:#1c1c21}}</style>\
         {corner}{account}\
         <main style=\"flex:1;display:flex;flex-direction:column;justify-content:center;align-items:center;\
         text-align:center;padding:5.5rem 1.5rem 1.5rem\">\
         <h1 style=\"margin:0 0 .75rem;font-size:1.5rem;font-weight:600;letter-spacing:-.025em\">{heading}</h1>\
         <p style=\"margin:0;color:#a1a1aa;line-height:1.55\">{detail}</p>\
         </main>\
         <footer style=\"display:flex;justify-content:center;padding:0 0 32px\">{mark}</footer>\
         </body></html>"
    )
}

const CHEVRON: &str = r##"<svg width="12" height="12" viewBox="0 0 12 12" aria-hidden="true"><path d="M2.5 4.5 6 8l3.5-3.5" fill="none" stroke="#a1a1aa" stroke-width="1.4" stroke-linecap="round" stroke-linejoin="round"/></svg>"##;

/// Top-right account control. Closed, it is the account in use. Open, it lists
/// every identity on this machine, grouped by the owner claim on that identity
/// when more than one owner is present, and offers add and remove. Choosing a
/// row switches. Nothing in the center repeats it.
fn account_menu(who: &Shown, others: &[Shown], confirm_remove: Option<&str>) -> String {
    let mut rows: Vec<&Shown> = vec![who];
    for other in others {
        if other.id != who.id {
            rows.push(other);
        }
    }
    let summary = account_face(who, true);
    let mut owners: Vec<&str> = Vec::new();
    for account in &rows {
        let owner = owner_of(&account.id);
        if !owners.contains(&owner) {
            owners.push(owner);
        }
    }
    let mut body = String::new();
    for owner in &owners {
        let mut group = String::new();
        for account in &rows {
            if owner_of(&account.id) == *owner {
                group.push_str(&account_row(
                    account,
                    account.id == who.id,
                    confirm_remove,
                    owners.len() == 1,
                ));
            }
        }
        if owners.len() > 1 {
            body.push_str(&section(&escape(owner), &group));
        } else {
            body.push_str(&group);
        }
    }
    format!(
        "<details class=\"hz\" style=\"position:absolute;top:16px;right:16px\">\
         <summary style=\"list-style:none;cursor:pointer\">{summary}</summary>\
         <div style=\"position:absolute;right:0;margin-top:8px;min-width:280px;background:#111113;\
         border:1px solid #2e2e33;border-radius:14px;padding:6px 6px 4px;text-align:left;\
         box-shadow:0 16px 40px rgba(0,0,0,.45)\">{body}\
         <a href=\"/add\" style=\"display:block;margin-top:6px;padding:10px;border-top:1px solid #2a2a2e;\
         color:#f4f4f5;text-decoration:none;font-size:13px;font-weight:600\">Add account</a>\
         </div></details>"
    )
}

/// The owner half of `owner/name`. An identity that is not in that shape is
/// shown under itself.
fn owner_of(id: &str) -> &str {
    id.split_once('/').map(|(owner, _)| owner).unwrap_or(id)
}

fn section(title: &str, rows: &str) -> String {
    if rows.is_empty() {
        return String::new();
    }
    format!(
        "<div style=\"padding:8px 10px 2px;font-size:11px;font-weight:600;color:#71717a\">{title}</div>{rows}"
    )
}

fn account_row(who: &Shown, current: bool, confirm_remove: Option<&str>, show_owner: bool) -> String {
    let face = account_lines(who, show_owner);
    let enc = url_encode(&who.id);
    let confirm = confirm_remove == Some(who.id.as_str());
    let mark = if current {
        "<span style=\"margin-left:8px;color:#f4f4f5\">✓</span>"
    } else {
        ""
    };
    let switch = if current {
        format!("<div style=\"display:flex;align-items:center;gap:10px;flex:1\">{face}{mark}</div>")
    } else {
        format!(
            "<a href=\"/switch?id={enc}\" style=\"display:flex;align-items:center;gap:10px;flex:1;color:#f4f4f5;text-decoration:none\">{face}</a>"
        )
    };
    let action = if confirm {
        format!(
            "<span style=\"display:flex;flex-direction:column;align-items:flex-end;gap:4px;flex:none\">\
             <a href=\"/remove?id={enc}&amp;yes=1\" style=\"color:#fca5a5;font-size:12px;font-weight:600;text-decoration:none\">Yes, remove</a>\
             <a href=\"/callback\" style=\"color:#a1a1aa;font-size:12px;text-decoration:none\">Cancel</a></span>"
        )
    } else {
        format!(
            "<a href=\"/remove?id={enc}\" style=\"color:#71717a;font-size:12px;text-decoration:none;flex:none\">Remove</a>"
        )
    };
    let bg = if current { "background:#1c1c21;" } else { "" };
    format!(
        "<div class=\"row\" style=\"display:flex;align-items:center;gap:8px;margin-top:2px;padding:8px 10px;border-radius:8px;{bg}\">{switch}{action}</div>"
    )
}

fn add_account_button() -> String {
    "<a href=\"/add\" style=\"position:absolute;top:16px;right:16px;padding:8px 14px;\
      border:1px solid #2a2a2e;border-radius:999px;background:#101012;color:#f4f4f5;\
      text-decoration:none;font-size:13px;font-weight:600\">Add account</a>"
        .to_string()
}

fn account_face(who: &Shown, openable: bool) -> String {
    let chevron = if openable {
        format!("<span style=\"display:inline-flex;margin-left:2px\">{CHEVRON}</span>")
    } else {
        String::new()
    };
    format!(
        "<span class=\"pill\" style=\"display:inline-flex;align-items:center;gap:8px;padding:4px 10px 4px 4px;\
         border:1px solid #2a2a2e;border-radius:999px;background:#101012\">{lines}{chevron}</span>",
        lines = account_lines(who, true)
    )
}

fn account_lines(who: &Shown, show_owner: bool) -> String {
    let letter = who
        .label
        .chars()
        .find(|c| c.is_alphanumeric())
        .map(|c| c.to_uppercase().to_string())
        .unwrap_or_else(|| "?".to_string());
    let letter = escape(&letter);
    let avatar = format!(
        "<span style=\"width:28px;height:28px;border-radius:999px;background:#27272a;display:inline-flex;\
         align-items:center;justify-content:center;font-size:12px;font-weight:600;flex:none\">{letter}</span>"
    );
    let name = who.id.split_once('/').map(|(_, name)| name).unwrap_or(who.id.as_str());
    let title = if who.label == who.id { name } else { who.label.as_str() };
    let sub = owner_of(&who.id);
    let text = if show_owner {
        format!(
            "<span style=\"display:flex;flex-direction:column;line-height:1.2;text-align:left\">\
             <span style=\"font-size:13px;font-weight:600\">{}</span>\
             <span style=\"margin-top:2px;font-size:11px;color:#a1a1aa\">{}</span></span>",
            escape(title),
            escape(sub)
        )
    } else {
        format!(
            "<span style=\"font-size:13px;font-weight:600;line-height:1.2;text-align:left\">{}</span>",
            escape(title)
        )
    };
    format!("{avatar}{text}")
}

fn url_encode(text: &str) -> String {
    let mut out = String::new();
    for b in text.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char);
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// Hand the loopback to a short-lived copy of this binary and return. The
/// child answers a reload and a click on another account. This process does
/// not wait for that click.
fn detach_menu(listener: TcpListener, origin: &str, brand: &str) {
    #[cfg(unix)]
    {
        use std::os::fd::AsRawFd;
        let fd = listener.as_raw_fd();
        unsafe {
            let flags = libc::fcntl(fd, libc::F_GETFD);
            if flags >= 0 {
                libc::fcntl(fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC);
            }
        }
        if let Ok(exe) = std::env::current_exe() {
            let _ = std::process::Command::new(exe)
                .env("HANZO_LOGIN_FD", fd.to_string())
                .env("HANZO_LOGIN_ORIGIN", origin)
                .env("HANZO_LOGIN_BRAND", brand)
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn();
        }
    }
    #[cfg(not(unix))]
    let _ = (origin, brand);
    drop(listener);
}

/// The loopback after `hanzo login` has returned to the shell. A fixed three
/// minutes, then the port closes. Clicks switch, remove, or start another
/// sign-in. A new connection does not extend the clock; starting an add
/// gives a fresh three minutes for that browser round trip.
pub async fn serve_detached_menu() -> Result<()> {
    let fd: i32 = std::env::var("HANZO_LOGIN_FD")
        .context("HANZO_LOGIN_FD")?
        .parse()
        .context("HANZO_LOGIN_FD")?;
    let origin = std::env::var("HANZO_LOGIN_ORIGIN").context("HANZO_LOGIN_ORIGIN")?;
    let brand = std::env::var("HANZO_LOGIN_BRAND").context("HANZO_LOGIN_BRAND")?;
    let std_listener = inherited_listener(fd)?;
    std_listener
        .set_nonblocking(true)
        .context("loopback nonblocking")?;
    let listener = TcpListener::from_std(std_listener).context("loopback listener")?;
    let mut deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(180);
    let mut pending: Option<PendingAdd> = None;
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            break;
        }
        let accepted = tokio::time::timeout(remaining, listener.accept()).await;
        let Ok(Ok((mut stream, _))) = accepted else { break };
        let request = match tokio::time::timeout(
            std::time::Duration::from_millis(400),
            read_request(&mut stream),
        )
        .await
        {
            Ok(Ok(body)) => body,
            _ => {
                let _ = stream.shutdown().await;
                continue;
            }
        };
        let target = request_target(&request).unwrap_or("").to_string();
        if target == "/add" || target.starts_with("/add?") {
            let pkce = pkce::generate_pkce();
            let state = pkce::generate_state();
            let port = listener.local_addr().map(|a| a.port()).unwrap_or(0);
            let redirect = format!("http://127.0.0.1:{port}/callback");
            if let Ok(url) = build_authorize_url(&origin, &redirect, &pkce.challenge, &state, true) {
                pending = Some(PendingAdd { verifier: pkce.verifier, state });
                deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(180);
                reply(&mut stream, &see_other(url.as_str())).await;
                continue;
            }
        }
        if target.starts_with("/callback") {
            if let Some(add) = pending.as_ref() {
                if let Ok(cb) = parse_callback(&target) {
                    if cb.state.as_deref() == Some(add.state.as_str()) {
                        if let Some(code) = cb.code.clone() {
                            let verifier = add.verifier.clone();
                            pending = None;
                            let port = listener.local_addr().map(|a| a.port()).unwrap_or(0);
                            let redirect = format!("http://127.0.0.1:{port}/callback");
                            if let Ok(tokens) = exchange_code(&origin, &code, &redirect, &verifier).await {
                                if let Ok(mut cfg) = crate::config::Config::load(None) {
                                    let _ = super::store::add(&mut cfg, &brand, &tokens);
                                }
                            }
                        }
                    }
                }
            }
        }
        if let Some(query) = target.strip_prefix("/remove?") {
            let id = query_id(query);
            let yes = query.split('&').any(|p| p == "yes=1");
            if yes {
                if let Some(id) = id {
                    drop_account(&origin, &brand, &id).await;
                }
                reply(&mut stream, &menu_page(&origin, &brand, None)).await;
                continue;
            }
            if let Some(id) = id {
                reply(&mut stream, &menu_page(&origin, &brand, Some(&id))).await;
                continue;
            }
        }
        if let Some(id) = target.strip_prefix("/switch?").and_then(query_id) {
            if let Ok(mut cfg) = crate::config::Config::load(None) {
                if let Ok(sel) = id.parse::<super::identity::Selector>() {
                    let _ = super::store::switch(&mut cfg, &brand, Some(sel));
                }
            }
        }
        if target.starts_with("/callback") || target.starts_with("/switch") {
            reply(&mut stream, &menu_page(&origin, &brand, None)).await;
        } else {
            reply(&mut stream, EMPTY_OK).await;
        }
    }
    Ok(())
}

/// The loopback the parent kept open. On Unix the child is given the listening
/// socket; this OS is the one `detach_menu` actually hands off.
fn inherited_listener(fd: i32) -> Result<std::net::TcpListener> {
    #[cfg(unix)]
    {
        Ok(unsafe { <std::net::TcpListener as std::os::fd::FromRawFd>::from_raw_fd(fd) })
    }
    #[cfg(not(unix))]
    {
        let _ = fd;
        bail!("the signed-in account menu is handed off through a file descriptor")
    }
}

struct PendingAdd {
    verifier: String,
    state: String,
}

fn see_other(url: &str) -> String {
    format!(
        "HTTP/1.1 303 See Other\r\nLocation: {url}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
    )
}

fn menu_page(origin: &str, brand: &str, confirm_remove: Option<&str>) -> String {
    let (who, others) = menu_accounts(brand);
    html_ok(&signed_in_page(origin, who.as_ref(), &others, confirm_remove, true))
}

/// Remove one stored identity and tell IAM to drop its refresh token.
/// If that identity was the one in use, the next one on this machine becomes
/// current so the menu still has someone to show.
async fn drop_account(origin: &str, brand: &str, id: &str) {
    let Ok(sel) = id.parse::<super::identity::Selector>() else {
        return;
    };
    let Ok(mut cfg) = crate::config::Config::load(None) else {
        return;
    };
    let was_active = super::store::active(&cfg, brand).map(|i| i.to_string()).as_deref() == Some(id);
    let Ok(removed) = super::store::remove(&mut cfg, brand, Some(sel)) else {
        return;
    };
    if let Some(rt) = removed.refresh_token.as_deref() {
        let _ = revoke(origin, rt).await;
    }
    if !was_active {
        return;
    }
    let Ok(mut cfg) = crate::config::Config::load(None) else {
        return;
    };
    if super::store::active(&cfg, brand).is_some() {
        return;
    }
    if let Some(next) = super::store::list(&cfg, brand).into_iter().next() {
        let _ = super::store::switch(&mut cfg, brand, Some(super::identity::Selector::Exact(next)));
    }
}

fn menu_accounts(brand: &str) -> (Option<Shown>, Vec<Shown>) {
    let Ok(cfg) = crate::config::Config::load(None) else {
        return (None, Vec::new());
    };
    let accounts = known_accounts(&cfg, brand);
    let who = super::store::active(&cfg, brand).and_then(|id| {
        let id = id.to_string();
        accounts.iter().find(|account| account.id == id).cloned()
    });
    (who, accounts)
}

fn query_id(query: &str) -> Option<String> {
    for pair in query.split('&') {
        let (key, value) = pair.split_once('=')?;
        if key == "id" {
            return Some(percent_decode(value));
        }
    }
    None
}

fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(byte) = u8::from_str_radix(
                std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or(""),
                16,
            ) {
                out.push(byte);
                i += 3;
                continue;
            }
        }
        out.push(if bytes[i] == b'+' { b' ' } else { bytes[i] });
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Text into HTML. The provider's `error` rides in on the query string, so it
/// is never markup.
fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
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
            false,
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
        assert!(
            !q.contains_key("prompt"),
            "a plain login reuses the browser's session"
        );

        let chosen = build_authorize_url(
            "https://hanzo.id",
            "http://127.0.0.1:1/callback",
            "C",
            "S",
            true,
        )
        .unwrap();
        let q: HashMap<_, _> = chosen.query_pairs().into_owned().collect();
        assert_eq!(q["prompt"], "select_account");
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

    // Drive the real loopback server over a TCP socket. The redirect is answered
    // before we return, and a favicon that arrives first does not eat that
    // response — that request is what left the tab spinning.
    #[tokio::test]
    async fn loopback_captures_code_then_shows_the_signed_in_page() {
        use tokio::net::TcpStream;

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (cb, mut stream) = capture_callback(&listener, "xyz", "https://hanzo.id")
                .await
                .unwrap();
            reply(&mut stream, &conclusion("https://hanzo.id", None)).await;
            cb
        });

        let mut favicon = TcpStream::connect(addr).await.unwrap();
        favicon
            .write_all(b"GET /favicon.ico HTTP/1.1\r\nHost: localhost\r\n\r\n")
            .await
            .unwrap();
        let mut icon = Vec::new();
        favicon.read_to_end(&mut icon).await.unwrap();
        let icon = String::from_utf8_lossy(&icon);
        assert!(icon.starts_with("HTTP/1.1 204"), "got: {icon}");

        let mut client = TcpStream::connect(addr).await.unwrap();
        client
            .write_all(b"GET /callback?code=abc&state=xyz HTTP/1.1\r\nHost: localhost\r\n\r\n")
            .await
            .unwrap();

        let cb = server.await.unwrap();
        assert_eq!(cb.code.as_deref(), Some("abc"));
        assert_eq!(cb.state.as_deref(), Some("xyz"));

        let mut response = Vec::new();
        client.read_to_end(&mut response).await.unwrap();
        let response = String::from_utf8_lossy(&response);
        assert!(response.starts_with("HTTP/1.1 200 OK"), "got: {response}");
        assert!(!response.contains("Location:"), "got: {response}");
        assert!(response.contains("You're signed in"), "got: {response}");
        assert!(response.contains("Hanzo AI"), "got: {response}");
        assert_eq!(response.matches("href=\"https://hanzo.ai\"").count(), 2, "got: {response}");
        assert!(!response.contains("Hanzo ID"), "got: {response}");
        assert!(response.contains("top:24px;left:24px"), "got: {response}");
        assert!(response.contains("M22.21 67V44.6369H0V67H22.21Z"), "got: {response}");
        assert!(!response.contains("<circle"), "got: {response}");
        assert!(response.contains("background:#070709"), "got: {response}");
    }

    #[test]
    fn signed_in_page_names_the_account_and_offers_a_switch() {
        let who = Shown {
            label: "z@hanzo.ai".into(),
            id: "hanzo/z".into(),
        };
        let page = signed_in_page(
            "https://hanzo.id",
            Some(&who),
            &[
                Shown {
                    label: "a@lux.id".into(),
                    id: "lux/a".into(),
                },
                Shown {
                    label: "hanzo/Zach Kelling".into(),
                    id: "hanzo/Zach Kelling".into(),
                },
                Shown {
                    label: "hanzo".into(),
                    id: "hanzo/hanzo".into(),
                },
                who.clone(),
            ],
            None,
            true,
        );
        let main = page.split("<main").nth(1).unwrap_or("");
        assert!(!main.contains("z@hanzo.ai"), "{page}");
        assert!(!main.contains("lux/a"), "{page}");
        assert!(page.contains("z@hanzo.ai"), "{page}");
        assert!(page.contains("Zach Kelling"), "{page}");
        assert!(!page.contains("Personal"), "{page}");
        assert!(!page.contains("Organizations"), "{page}");
        assert!(!page.contains("Organization"), "{page}");
        assert!(page.contains(">hanzo</div>"), "{page}");
        assert!(page.contains(">lux</div>"), "{page}");
        assert!(page.contains(">Add account<"), "{page}");
        assert!(page.contains("/remove?id=hanzo%2Fz"), "{page}");
        assert!(page.contains("top:16px;right:16px"), "{page}");
        assert!(page.contains("a@lux.id"), "{page}");
        assert!(page.contains("/switch?id=lux%2Fa"), "{page}");
        assert!(page.contains("/switch?id=hanzo%2FZach%20Kelling"), "{page}");
        assert!(page.contains("<details"), "{page}");
        assert!(page.contains('✓'), "{page}");
        assert!(!page.contains("Also on this machine"), "{page}");
        assert!(!page.contains("hanzo auth use"), "{page}");
        assert_eq!(page.matches("href=\"https://hanzo.ai\"").count(), 2, "{page}");
        let confirming = signed_in_page("https://hanzo.id", Some(&who), &[who.clone()], Some("hanzo/z"), true);
        assert!(confirming.contains("Yes, remove"), "{confirming}");
        assert!(confirming.contains("/remove?id=hanzo%2Fz&amp;yes=1"), "{confirming}");
    }

    // The first write is not the tab's only try. Safari retries the redirect,
    // and the process has to be there for it, then gone — a listener that
    // stays up is the hang.
    #[tokio::test]
    async fn the_signed_in_page_answers_a_retry_then_the_port_closes() {
        use std::time::Duration;
        use tokio::net::TcpStream;

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let page = html_ok("<html>You're signed in</html>");
        let server = tokio::spawn(async move {
            serve_signed_in(&listener, &page, Duration::from_millis(600)).await;
        });

        let mut client = TcpStream::connect(addr).await.unwrap();
        client
            .write_all(b"GET /callback?code=abc&state=xyz HTTP/1.1\r\nHost: localhost\r\n\r\n")
            .await
            .unwrap();
        let mut response = Vec::new();
        client.read_to_end(&mut response).await.unwrap();
        let response = String::from_utf8_lossy(&response);
        assert!(response.starts_with("HTTP/1.1 200"), "{response}");
        assert!(response.contains("You're signed in"), "{response}");

        server.await.unwrap();
        assert!(
            TcpStream::connect(addr).await.is_err(),
            "the port stayed open after the fixed window"
        );
    }

    // A browser sends back what we sent it, so on THIS leg an absent state is
    // as wrong as a wrong one — and the tab is told so on the spot.
    #[tokio::test]
    async fn loopback_refuses_a_state_that_is_not_ours() {
        use tokio::net::TcpStream;

        for target in [
            "/callback?code=abc&state=SOMEONE_ELSE",
            "/callback?code=abc",
        ] {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();
            let server = tokio::spawn(async move {
                capture_callback(&listener, "xyz", "https://hanzo.id").await
            });

            let mut client = TcpStream::connect(addr).await.unwrap();
            client
                .write_all(format!("GET {target} HTTP/1.1\r\nHost: localhost\r\n\r\n").as_bytes())
                .await
                .unwrap();
            let mut response = Vec::new();
            let _ = client.read_to_end(&mut response).await;
            let response = String::from_utf8_lossy(&response);

            assert!(server.await.unwrap().is_err(), "{target} was accepted");
            assert!(
                response.starts_with("HTTP/1.1 400 Bad Request"),
                "{target}: {response}"
            );
            assert!(response.contains("background:#000"), "{target}: {response}");
        }
    }

    // The provider's `error` comes off the query string: it is shown, never run.
    #[test]
    fn a_failure_page_shows_the_reason_as_text() {
        let page = conclusion("https://lux.id/", Some("<script>alert(1)</script> & \"x\""));
        assert!(page.starts_with("HTTP/1.1 400 Bad Request"));
        assert!(page.contains("&lt;script&gt;alert(1)&lt;/script&gt; &amp; &quot;x&quot;"));
        assert!(!page.contains("<script>"));
        assert!(page.contains("lux.id"));

        let ok = conclusion("https://lux.id/", None);
        assert!(ok.starts_with("HTTP/1.1 200 OK"));
        assert!(ok.contains("You're signed in"));
        assert!(ok.contains("lux.id"));
        assert!(!ok.contains("Location:"));
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
