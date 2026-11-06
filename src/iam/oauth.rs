//! The OIDC Authorization-Code-with-PKCE flow against Hanzo IAM (HIP-0111).
//!
//! The client is `<org>-cli` ([`client`]), a PUBLIC client (no secret): PKCE
//! S256 is the proof. We bind
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

use super::device::DEVICE_GRANT_TYPE;
use super::identity::{self, Identity};
use super::paths::{self, APPLICATION, AUTHORIZE, LOGOUT, REVOKE, TOKEN, USERINFO};
use super::pkce;
use super::token::TokenSet;

/// OIDC scopes — identity only.
pub const SCOPE: &str = "openid profile email";

/// The IAM client a sign-in runs through: `<org>-cli` (HIP-0111) for the org
/// `--as` names, else `hanzo-cli`. IAM signs a person in to the client's own
/// org, so this decides whose identity comes back.
pub fn client(org: Option<&str>) -> String {
    format!("{}-cli", org.unwrap_or("hanzo"))
}

/// Read the sign-in client's public descriptor before any browser opens, and
/// refuse when IAM has no such client or, under `--as <org>`, when it signs in
/// to another org. Returns whether the client permits the device grant.
pub async fn served(origin: &str, org: Option<&str>) -> Result<bool> {
    #[derive(Deserialize)]
    struct Descriptor {
        #[serde(default)]
        status: String,
        #[serde(default)]
        msg: String,
        data: Option<App>,
    }
    #[derive(Deserialize)]
    struct App {
        #[serde(default)]
        organization: String,
        #[serde(default, rename = "grantTypes")]
        grants: Vec<String>,
    }
    let id = client(org);
    let resp = reqwest::Client::new()
        .get(paths::iam_url(origin, APPLICATION))
        .query(&[("clientId", id.as_str()), ("responseType", "code")])
        .send()
        .await
        .context("calling IAM application descriptor")?;
    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    let found: Descriptor = serde_json::from_str(&body)
        .map_err(|_| anyhow!("IAM's descriptor for `{id}` is unreadable ({status})"))?;
    let app = match found {
        Descriptor { status, data: Some(app), .. } if status == "ok" => app,
        Descriptor { msg, .. } => match org {
            Some(org) => bail!("IAM has no `{id}` application, so `--as {org}` has no {org} sign-in to open ({msg})"),
            None => bail!("IAM has no `{id}` application ({msg})"),
        },
    };
    if let Some(org) = org.filter(|org| *org != app.organization) {
        bail!("IAM's `{id}` application signs in to org `{}`, not `{org}`", app.organization);
    }
    Ok(app.grants.iter().any(|grant| grant == DEVICE_GRANT_TYPE))
}

/// A refresh IAM refused, or one no client can be named for: the session is
/// over, and only a new sign-in replaces it.
#[derive(Debug)]
pub struct Spent(String);

impl std::fmt::Display for Spent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Spent {}

/// Refuse a token outside the org `--as` named. IAM stamps `owner` with the
/// signing application's org, so another owner means another org's sign-in.
/// A refusal only: nothing is granted on this unverified read.
pub fn owned(tokens: &TokenSet, org: Option<&str>) -> Result<()> {
    let Some(org) = org else { return Ok(()) };
    let id = Identity::from_access_token(&tokens.access_token)?;
    if id.owner != org {
        bail!(
            "`--as {org}` signed in through `{}` and IAM returned {id}, which is not in {org}; nothing was stored",
            client(Some(org))
        );
    }
    Ok(())
}

/// The client `held` was issued to, which is the only one IAM accepts it from:
/// a refresh token presented by another client is refused, and a revocation
/// ends only the calling client's grants.
fn issued(held: &TokenSet) -> Result<String> {
    identity::party(&held.access_token)
        .ok_or_else(|| anyhow!("the stored credential names no client (no `azp`, no single `aud`)"))
}

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

/// Resolve a realm to its IAM origin, or error with the known set.
pub fn server_url(brand: &str) -> Result<String> {
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

/// The loopback after the exchange, still listening, with the tab's
/// connections waiting for the page. [`finish`] writes it and closes the port.
pub(crate) struct Held {
    listener: TcpListener,
    origin: String,
    waiting: Vec<TcpStream>,
}

pub(crate) enum Done {
    Pasted(TokenSet),
    Browser(TokenSet, Held),
}

/// What a sign-in asks hanzo.id for: the account it is for (`login_hint`), and
/// whether the person chooses among the accounts signed in on that browser
/// (`prompt=select_account`) rather than continuing as the latest one.
#[derive(Debug, Default)]
pub struct Ask {
    pub hint: Option<String>,
    pub choose: bool,
}

/// Run the full interactive login flow for `brand` and return the tokens.
/// `org` (`--as`) signs in through that org's own client and refuses a token
/// from any other org. `ask` names the account and whether to show the chooser.
/// `open` opens a browser here; without one the code arrives by paste alone.
pub async fn login(brand: &str, org: Option<&str>, open: bool, ask: &Ask) -> Result<Done> {
    if !open && !std::io::stdin().is_terminal() {
        bail!(
            "no browser opens here and `{}` permits no device grant, so the sign-in needs its \
             callback URL pasted: run this at a terminal",
            client(org)
        );
    }
    let origin = &server_url(brand)?;
    let pkce = pkce::generate_pkce();
    let state = pkce::generate_state();

    // Bind the loopback callback FIRST so the port is known for redirect_uri.
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .context("binding loopback callback server")?;
    let port = listener.local_addr()?.port();
    let redirect_uri = format!("http://127.0.0.1:{port}/callback");

    let authorize_url = build_authorize_url(origin, org, &redirect_uri, &pkce.challenge, &state, ask)?;

    // Print the link BEFORE asking the OS to open it. `open` on this machine
    // can sit there until a browser answers, and that wait used to be the
    // whole of `hanzo login`: no line, no URL, nothing to paste.
    if open {
        println!("Opening your browser to sign in to {brand}...");
        println!("If it does not open, visit:\n  {authorize_url}\n");
    } else {
        println!("Sign in to {brand} in a browser on any machine:\n  {authorize_url}\n");
    }
    if std::io::stdin().is_terminal() {
        println!("Signed in on another machine? Paste the URL it lands on here.");
    }
    let _ = std::io::Write::flush(&mut std::io::stdout());
    if open {
        let url_for_open = authorize_url.to_string();
        std::thread::spawn(move || {
            let _ = webbrowser::open(&url_for_open);
        });
    }

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
                exchange_code(origin, org, &code, &redirect_uri, &pkce.verifier).await?,
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
    let mut exchange = std::pin::pin!(exchange_code(origin, org, &code, &redirect_uri, &verifier));
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
    Ok(Done::Browser(
        tokens,
        Held {
            listener,
            origin: origin.to_string(),
            waiting,
        },
    ))
}

/// Finish a browser sign-in: write the page on every connection the tab is
/// waiting on, keep answering its reloads while `work` (filing the identity)
/// runs, then close the port. `active` is the identity the terminal goes on
/// as, when that is not the one just signed in. Nothing listens once this
/// returns: the loopback lives exactly as long as the command that opened it.
pub(crate) async fn finish<T>(
    held: Held,
    tokens: &TokenSet,
    active: Option<&Identity>,
    work: impl std::future::Future<Output = Result<T>>,
) -> Result<T> {
    // Claims only. Userinfo is another round trip, and holding the document
    // for it is what made Safari abandon the tab before any byte arrived.
    let who = shown_account(tokens);
    let kept = active.map(Identity::to_string);
    let page = html_ok(&signed_in_page(&held.origin, who.as_ref(), kept.as_deref()));
    let Held { listener, mut waiting, .. } = held;
    for stream in &mut waiting {
        reply(stream, &page).await;
    }
    let mut work = std::pin::pin!(work);
    loop {
        tokio::select! {
            biased;
            result = &mut work => return result,
            incoming = listener.accept() => {
                let Ok((mut stream, _)) = incoming else { continue };
                answer_held(&mut stream, &page).await;
            }
        }
    }
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
    if request_target(&request).unwrap_or("").starts_with("/callback") {
        reply(stream, page).await;
    } else {
        reply(stream, EMPTY_OK).await;
    }
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

/// Fetch the userinfo profile for an access token.
pub async fn userinfo(brand: &str, access_token: &str) -> Result<UserInfo> {
    let origin = &server_url(brand)?;
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
    org: Option<&str>,
    redirect_uri: &str,
    challenge: &str,
    state: &str,
    ask: &Ask,
) -> Result<Url> {
    let client = client(org);
    let mut q = vec![
        ("response_type", "code"),
        ("client_id", client.as_str()),
        ("redirect_uri", redirect_uri),
        ("scope", SCOPE),
        ("state", state),
        ("code_challenge", challenge),
        ("code_challenge_method", "S256"),
    ];
    if ask.choose {
        q.push(("prompt", "select_account"));
    }
    if let Some(hint) = ask.hint.as_deref() {
        q.push(("login_hint", hint));
    }
    Url::parse_with_params(&paths::iam_url(origin, AUTHORIZE), &q).context("building authorize URL")
}

/// Exchange an authorization code for tokens (RFC 6749 §4.1.3 + PKCE §4.5),
/// through `org`'s client, refusing tokens from any other org.
async fn exchange_code(
    origin: &str,
    org: Option<&str>,
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
            ("client_id", client(org).as_str()),
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
    let tokens = serde_json::from_str::<TokenSet>(&body).context("parsing token response")?;
    owned(&tokens, org)?;
    Ok(tokens)
}

/// How long one refresh may take. A connection that stalls fails here, with its
/// cause, rather than holding every command that needs a credential.
const REFRESH_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// Exchange a refresh token for a fresh access token (RFC 6749 §6), as the
/// client the credential was issued to.
///
/// The access token IAM mints lives one hour. Without this the CLI holds a
/// refresh token it never spends, so every command an hour after login fails —
/// and fails CONFUSINGLY, because a stale token reads downstream as "X-Org-Id
/// required" or "a validated principal is required" rather than "log in again".
pub async fn refresh(origin: &str, held: &TokenSet) -> Result<TokenSet> {
    let refresh_token = held
        .refresh_token
        .as_deref()
        .context("the stored credential holds no refresh token")?;
    let client = issued(held).map_err(|e| Spent(e.to_string()))?;
    let resp = reqwest::Client::builder()
        .timeout(REFRESH_TIMEOUT)
        .build()
        .context("building the IAM client")?
        .post(paths::iam_url(origin, TOKEN))
        .form(&[
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token),
            ("client_id", client.as_str()),
        ])
        .send()
        .await
        .context("calling IAM token endpoint")?;
    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    if matches!(status.as_u16(), 400 | 401 | 403) {
        return Err(Spent(format!("token refresh refused ({status}): {body}")).into());
    }
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
/// exactly what a client with no secret has to offer (RFC 6749 §3.2.1). The
/// client is the one the credential was issued to; a credential with no refresh
/// token holds no session at IAM, and sends nothing.
pub async fn revoke(origin: &str, held: &TokenSet) -> Result<()> {
    let Some(refresh_token) = held.refresh_token.as_deref() else {
        return Ok(());
    };
    let client = issued(held)?;
    let resp = reqwest::Client::new()
        .post(paths::iam_url(origin, REVOKE))
        .form(&[
            ("token", refresh_token),
            ("token_type_hint", "refresh_token"),
            ("client_id", client.as_str()),
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
            let body = signed_in_page(origin, None, None);
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

/// The corner name and footer mark of a loopback page, as hanzo.id places
/// them: on hanzo.id the name is "Hanzo AI" and both open https://hanzo.ai;
/// another issuer gets its own "<Name> ID" and no mark.
fn chrome(host: &str) -> (String, String) {
    if host == "hanzo.id" {
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
                id_label(host)
            ),
            String::new(),
        )
    }
}

/// One loopback page: the corner, an optional account face at the top right,
/// and a message in the middle. Self-contained; the loopback has no stylesheet.
fn page(origin: &str, title: &str, account: &str, heading: &str, detail: &str) -> String {
    let origin = origin.trim_end_matches('/');
    let host = escape(origin.split("://").nth(1).unwrap_or(origin));
    let (corner, mark) = chrome(&host);
    format!(
        "<!doctype html><html lang=en><meta charset=utf-8><meta name=color-scheme content=dark>\
         <meta name=viewport content=\"width=device-width,initial-scale=1\"><title>{title} — {host}</title>\
         <body style=\"margin:0;min-height:100vh;min-height:100dvh;background:#070709;color:#f4f4f5;position:relative;\
         display:flex;flex-direction:column;font-family:Zen,Inter,ui-sans-serif,system-ui,-apple-system,sans-serif\">\
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

/// The page a finished `hanzo login` leaves in the browser. It names the
/// identity that signed in, and the one the terminal goes on as: the same one,
/// or `kept` when hanzo.id answered with an account nobody chose and the active
/// identity stays as it was. Nothing on it changes anything; the port closes
/// when the command returns, and `hanzo auth use` is the one way to switch.
fn signed_in_page(origin: &str, who: Option<&Shown>, kept: Option<&str>) -> String {
    let account = who.map(account_face).unwrap_or_default();
    let detail = match (who, kept) {
        (Some(who), Some(kept)) => format!(
            "Added {}. The terminal still uses {}.<br><code>hanzo auth use {}</code> switches.",
            escape(&who.id),
            escape(kept),
            escape(&who.id)
        ),
        (Some(who), None) => format!(
            "The terminal continues as {}.<br>You can close this tab.",
            escape(&who.id)
        ),
        (None, _) => "Return to the terminal.<br>You can close this tab.".to_string(),
    };
    page(origin, "Signed in", &account, "You're signed in", &detail)
}

/// The page `hanzo auth logout` leaves in the browser once hanzo.id has ended
/// that browser's sessions.
fn signed_out_page(origin: &str) -> String {
    page(
        origin,
        "Signed out",
        "",
        "You're signed out",
        "This browser is signed out too, so the next sign-in asks which account.<br>You can close this tab.",
    )
}

/// The account a page names, top right: initial, name, and owner.
fn account_face(who: &Shown) -> String {
    let letter = who
        .label
        .chars()
        .find(|c| c.is_alphanumeric())
        .map(|c| c.to_uppercase().to_string())
        .unwrap_or_else(|| "?".to_string());
    let name = who.id.split_once('/').map(|(_, name)| name).unwrap_or(who.id.as_str());
    let title = if who.label == who.id { name } else { who.label.as_str() };
    let owner = who.id.split_once('/').map(|(owner, _)| owner).unwrap_or(who.id.as_str());
    format!(
        "<span style=\"position:absolute;top:16px;right:16px;display:inline-flex;align-items:center;gap:8px;\
         padding:4px 10px 4px 4px;border:1px solid #2a2a2e;border-radius:999px;background:#101012\">\
         <span style=\"width:28px;height:28px;border-radius:999px;background:#27272a;display:inline-flex;\
         align-items:center;justify-content:center;font-size:12px;font-weight:600;flex:none\">{}</span>\
         <span style=\"display:flex;flex-direction:column;line-height:1.2;text-align:left\">\
         <span style=\"font-size:13px;font-weight:600\">{}</span>\
         <span style=\"margin-top:2px;font-size:11px;color:#a1a1aa\">{}</span></span></span>",
        escape(&letter),
        escape(title),
        escape(owner)
    )
}

/// The end-session URL (OIDC RP-Initiated Logout 1.0 §2) for `client`. With
/// `back`, the loopback IAM returns the browser to and the state it carries.
///
/// It names the client by `client_id` and sends no `id_token_hint`: IAM accepts
/// either to name the application, ends every session that browser holds
/// whichever it gets, and the CLI's own grant is already revoked (RFC 7009). A
/// hint would add nothing here but a signed identity assertion written into the
/// browser's history and whatever syncs it.
fn end_session_url(origin: &str, client: &str, back: Option<(&str, &str)>) -> Result<Url> {
    let mut q = vec![("client_id", client)];
    if let Some((redirect, state)) = back {
        q.push(("post_logout_redirect_uri", redirect));
        q.push(("state", state));
    }
    Url::parse_with_params(&paths::iam_url(origin, LOGOUT), &q).context("building end-session URL")
}

/// How long `hanzo auth logout` waits for the browser to come back.
const SIGN_OUT_WAIT: std::time::Duration = std::time::Duration::from_secs(90);

/// End the browser's session at the issuer, so the next `hanzo auth login`
/// asks which account rather than continuing as the one that signed out.
///
/// The session lives in a cookie only the browser holds, so only a browser
/// navigation can end it. With a browser here the CLI opens the end-session
/// URL and waits on a loopback for IAM to return the tab (the client's
/// registered `http://127.0.0.1/callback`, any port, RFC 8252 §7.3), then
/// closes the port. Without one it prints the URL. Returns whether the
/// browser confirmed.
pub async fn end_session(origin: &str, client: &str, open: bool) -> Result<bool> {
    let host = origin.trim_end_matches('/').split("://").nth(1).unwrap_or(origin).to_string();
    if !open {
        let url = end_session_url(origin, client, None)?;
        println!("To sign out of {host} in a browser too, open:\n  {url}");
        return Ok(false);
    }
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .context("binding loopback sign-out server")?;
    let redirect = format!("http://127.0.0.1:{}/callback", listener.local_addr()?.port());
    let state = pkce::generate_state();
    let url = end_session_url(origin, client, Some((redirect.as_str(), state.as_str())))?;
    println!("Signing out of {host} in your browser...");
    println!("If it does not open, visit:\n  {url}");
    let _ = std::io::Write::flush(&mut std::io::stdout());
    let to_open = url.to_string();
    std::thread::spawn(move || {
        let _ = webbrowser::open(&to_open);
    });
    Ok(matches!(
        tokio::time::timeout(SIGN_OUT_WAIT, signed_out(&listener, &state, origin)).await,
        Ok(true)
    ))
}

/// Accept loopback requests until IAM returns the tab with our state, answer it
/// with the signed-out page, and return. Anything else gets an empty answer:
/// a redirect with another state is some other tab, and this one may still come.
/// False only when the listener itself fails.
async fn signed_out(listener: &TcpListener, state: &str, origin: &str) -> bool {
    loop {
        let Ok((mut stream, _)) = listener.accept().await else { return false };
        let request = match tokio::time::timeout(std::time::Duration::from_secs(3), read_request(&mut stream)).await {
            Ok(Ok(body)) => body,
            _ => {
                let _ = stream.shutdown().await;
                continue;
            }
        };
        let target = request_target(&request).unwrap_or("");
        let ours = target.starts_with("/callback")
            && parse_callback(target).is_ok_and(|cb| cb.state.as_deref() == Some(state));
        if ours {
            reply(&mut stream, &html_ok(&signed_out_page(origin))).await;
            return true;
        }
        reply(&mut stream, EMPTY_OK).await;
    }
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
    use crate::iam::identity::testjwt::{claims_jwt, jwt};
    use std::collections::HashMap;
    use std::sync::Mutex;

    /// A credential `client` was issued, holding `refresh_token`.
    fn issued_to(client: &str, refresh_token: &str) -> TokenSet {
        TokenSet {
            access_token: claims_jwt(&format!(r#"{{"owner":"admin","name":"z","azp":"{client}"}}"#)),
            token_type: "Bearer".into(),
            refresh_token: Some(refresh_token.into()),
            id_token: None,
            expires_in: None,
            scope: None,
        }
    }

    /// A stand-in IAM that records every request and answers from `answer`.
    async fn stand(
        answer: fn(&str) -> (&'static str, String),
    ) -> (String, Arc<Mutex<Vec<String>>>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let seen = Arc::new(Mutex::new(Vec::new()));
        let record = Arc::clone(&seen);
        tokio::spawn(async move {
            loop {
                let Ok((mut stream, _)) = listener.accept().await else { break };
                let req = read_http(&mut stream).await;
                record.lock().unwrap().push(req.clone());
                let (status, body) = answer(&req);
                let response = format!(
                    "HTTP/1.1 {status}\r\nContent-Type: application/json\r\n\
                     Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = stream.write_all(response.as_bytes()).await;
                let _ = stream.shutdown().await;
            }
        });
        (origin, seen)
    }

    /// A descriptor as IAM serves it: the client's org and its grants.
    fn described(org: &str, device: bool) -> String {
        let device = if device { r#","urn:ietf:params:oauth:grant-type:device_code""# } else { "" };
        format!(
            r#"{{"status":"ok","msg":"","data":{{"organization":"{org}","grantTypes":["authorization_code","refresh_token"{device}]}}}}"#
        )
    }

    /// `--as <org>` is refused before any browser opens when IAM has no
    /// `<org>-cli`, or when that client signs in to another org; otherwise the
    /// descriptor says whether the client permits the device grant.
    #[tokio::test]
    async fn wire_an_org_without_its_own_cli_is_refused_before_sign_in() {
        let (origin, seen) = stand(|req| {
            if req.contains("clientId=admin-cli") {
                ("400 Bad Request", r#"{"status":"error","msg":"the application does not exist","data":null}"#.into())
            } else if req.contains("clientId=lux-cli") {
                ("200 OK", described("lux", false))
            } else if req.contains("clientId=hanzo-cli") || req.contains("clientId=odd-cli") {
                ("200 OK", described("hanzo", true))
            } else {
                ("200 OK", "<!doctype html>".into())
            }
        })
        .await;

        let err = served(&origin, Some("admin")).await.unwrap_err().to_string();
        assert!(err.contains("IAM has no `admin-cli` application"), "{err}");
        assert!(err.contains("`--as admin` has no admin sign-in"), "{err}");
        assert!(!served(&origin, Some("lux")).await.unwrap(), "lux-cli grants no device code");
        assert!(served(&origin, None).await.unwrap(), "hanzo-cli grants the device code");
        let err = served(&origin, Some("odd")).await.unwrap_err().to_string();
        assert!(err.contains("`odd-cli` application signs in to org `hanzo`, not `odd`"), "{err}");
        let err = served(&origin, Some("html")).await.unwrap_err().to_string();
        assert!(err.contains("unreadable"), "{err}");

        let seen = seen.lock().unwrap();
        assert!(seen[0].starts_with("GET /v1/iam/auth/application?"), "{}", seen[0]);
        assert!(seen[0].contains("clientId=admin-cli") && seen[0].contains("responseType=code"));
        assert!(seen[1].contains("clientId=lux-cli"), "admin fell back to another client: {}", seen[1]);
    }

    /// The code is exchanged through `--as`'s client, and a token for another
    /// org is refused rather than returned for filing.
    #[tokio::test]
    async fn wire_the_exchange_runs_as_the_orgs_client_and_checks_the_owner() {
        let (origin, seen) = stand(|req| {
            let owner = if req.contains("code=wrong") { "hanzo" } else { "admin" };
            let token = jwt(owner, "z");
            ("200 OK", format!(r#"{{"access_token":"{token}","token_type":"Bearer","refresh_token":"rt-1"}}"#))
        })
        .await;
        let redirect = "http://127.0.0.1:1/callback";

        let tokens = exchange_code(&origin, Some("admin"), "right", redirect, "v").await.unwrap();
        assert_eq!(Identity::from_access_token(&tokens.access_token).unwrap().to_string(), "admin/z");
        let err = exchange_code(&origin, Some("admin"), "wrong", redirect, "v")
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("IAM returned hanzo/z, which is not in admin"), "{err}");

        let seen = seen.lock().unwrap();
        assert_eq!(seen.len(), 2);
        assert!(seen.iter().all(|req| req.contains("client_id=admin-cli")), "{seen:?}");
    }

    /// Refresh and revocation present the client the credential was issued to,
    /// read off its own `azp` (else its one `aud`), never a constant; a
    /// credential naming no client sends nothing.
    #[tokio::test]
    async fn wire_refresh_and_revoke_present_the_credentials_own_client() {
        let (origin, seen) = stand(|req| {
            if req.contains("/v1/iam/oauth/revoke") {
                return ("200 OK", String::new());
            }
            let token = jwt("admin", "z");
            ("200 OK", format!(r#"{{"access_token":"{token}","token_type":"Bearer","refresh_token":"rt-2"}}"#))
        })
        .await;

        refresh(&origin, &issued_to("admin-cli", "rt-1")).await.unwrap();
        revoke(&origin, &issued_to("admin-cli", "rt-1")).await.unwrap();
        let aud = TokenSet {
            access_token: claims_jwt(r#"{"owner":"lux","name":"z","aud":["lux-cli"]}"#),
            ..issued_to("", "rt-lux")
        };
        refresh(&origin, &aud).await.unwrap();
        {
            let seen = seen.lock().unwrap();
            assert_eq!(seen.len(), 3);
            assert!(seen[0].contains("grant_type=refresh_token") && seen[0].contains("client_id=admin-cli"));
            assert!(seen[1].contains("/v1/iam/oauth/revoke") && seen[1].contains("client_id=admin-cli"));
            assert!(seen[2].contains("refresh_token=rt-lux") && seen[2].contains("client_id=lux-cli"));
            assert!(seen.iter().all(|req| !req.contains("hanzo-cli")), "{seen:?}");
        }

        let nameless = TokenSet { access_token: jwt("admin", "z"), ..issued_to("", "rt-x") };
        let err = refresh(&origin, &nameless).await.unwrap_err().to_string();
        assert!(err.contains("names no client"), "{err}");
        let err = revoke(&origin, &nameless).await.unwrap_err().to_string();
        assert!(err.contains("names no client"), "{err}");
        revoke(&origin, &TokenSet { refresh_token: None, ..nameless }).await.unwrap();
        assert_eq!(seen.lock().unwrap().len(), 3, "a credential naming no client was sent anyway");
    }

    /// A refresh IAM refuses is `Spent` (sign in again); one it never answered,
    /// or answered with its own failure, is not.
    #[tokio::test]
    async fn wire_a_refused_refresh_is_spent_and_an_unanswered_one_is_not() {
        let (origin, _) = stand(|req| {
            if req.contains("refresh_token=rt-down") {
                ("503 Service Unavailable", String::new())
            } else if req.contains("refresh_token=rt-busy") {
                ("429 Too Many Requests", String::new())
            } else if req.contains("refresh_token=rt-slow") {
                ("408 Request Timeout", String::new())
            } else if req.contains("refresh_token=rt-denied") {
                ("403 Forbidden", String::new())
            } else {
                ("400 Bad Request", r#"{"error":"invalid_grant","error_description":"refresh token expired"}"#.into())
            }
        })
        .await;
        let refused = refresh(&origin, &issued_to("admin-cli", "rt-old")).await.unwrap_err();
        assert!(refused.is::<Spent>(), "{refused}");
        assert!(refused.to_string().contains("refresh token expired"), "{refused}");
        let nameless = TokenSet { access_token: jwt("admin", "z"), ..issued_to("", "rt-x") };
        assert!(refresh(&origin, &nameless).await.unwrap_err().is::<Spent>());
        let denied = refresh(&origin, &issued_to("admin-cli", "rt-denied")).await.unwrap_err();
        assert!(denied.is::<Spent>(), "{denied}");
        for transient in ["rt-down", "rt-busy", "rt-slow"] {
            let err = refresh(&origin, &issued_to("admin-cli", transient)).await.unwrap_err();
            assert!(!err.is::<Spent>(), "{transient}: {err}");
        }
        let closed = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let gone = format!("http://{}", closed.local_addr().unwrap());
        drop(closed);
        assert!(!refresh(&gone, &issued_to("admin-cli", "rt")).await.unwrap_err().is::<Spent>());
    }

    fn admin(req: &str) -> (&'static str, String) {
        let token = claims_jwt(r#"{"owner":"admin","name":"z","sub":"u-1","azp":"admin-cli","aud":["admin-cli"]}"#);
        if req.starts_with("GET /v1/iam/auth/application") {
            return ("200 OK", described("admin", false));
        }
        if req.contains("/v1/iam/oauth/revoke") {
            return ("200 OK", String::new());
        }
        ("200 OK", format!(r#"{{"access_token":"{token}","token_type":"Bearer","refresh_token":"rt-admin"}}"#))
    }

    /// `hanzo --as admin auth login` end to end over the wire, as it runs where
    /// no browser opens: the descriptor grants `admin-cli` no device code, so
    /// the sign-in is the browser flow's (authorize URL, pasted code, exchange),
    /// then the owner check, a refresh and a revocation. Every request names
    /// `admin-cli`, none `hanzo-cli`, and nothing reaches the device endpoint.
    /// (Filing it beside the default is the shipped binary's test in
    /// `tests/menu.rs`; picking the leg is `login::by_device`.)
    #[tokio::test]
    async fn wire_as_admin_signs_in_by_paste_and_renews_through_admin_cli() {
        let (origin, seen) = stand(admin).await;
        let org = Some("admin");

        assert!(!served(&origin, org).await.unwrap(), "admin-cli must not grant the device code");
        let ask = Ask { choose: true, hint: None };
        let url = build_authorize_url(&origin, org, "http://127.0.0.1:1/callback", "C", "S", &ask).unwrap();
        let q: HashMap<_, _> = url.query_pairs().into_owned().collect();
        assert_eq!(q["client_id"], "admin-cli");
        let tokens = exchange_code(&origin, org, "pasted", "http://127.0.0.1:1/callback", "V").await.unwrap();
        let fresh = refresh(&origin, &tokens).await.unwrap();
        revoke(&origin, &fresh).await.unwrap();

        let seen = seen.lock().unwrap();
        assert_eq!(seen.len(), 4, "{seen:?}");
        assert!(seen[0].contains("clientId=admin-cli"), "{}", seen[0]);
        assert!(seen[1].contains("grant_type=authorization_code") && seen[1].contains("code=pasted"));
        assert!(seen[2].contains("grant_type=refresh_token") && seen[2].contains("refresh_token=rt-admin"));
        assert!(seen[3].contains("/v1/iam/oauth/revoke"));
        assert!(seen[1..].iter().all(|req| req.contains("client_id=admin-cli")), "{seen:?}");
        assert!(seen.iter().all(|req| !req.contains("hanzo-cli") && !req.contains("oauth/device")), "{seen:?}");
    }

    fn lux(req: &str) -> (&'static str, String) {
        if req.starts_with("GET /v1/iam/auth/application") {
            return ("200 OK", described("lux", true));
        }
        if req.contains("/v1/iam/oauth/device") {
            return (
                "200 OK",
                r#"{"device_code":"dc-1","user_code":"ABCD-EFGH","verification_uri":"http://127.0.0.1/device","expires_in":60,"interval":1}"#.into(),
            );
        }
        let token = claims_jwt(r#"{"owner":"lux","name":"z","sub":"u-1","azp":"lux-cli"}"#);
        ("200 OK", format!(r#"{{"access_token":"{token}","token_type":"Bearer","refresh_token":"rt-lux"}}"#))
    }

    /// A client whose descriptor grants the device code signs in by it where no
    /// browser opens, and the grant and its poll both name that org's client.
    #[tokio::test]
    async fn wire_a_client_granted_the_device_code_signs_in_by_it_as_itself() {
        let (origin, seen) = stand(lux).await;
        assert!(served(&origin, Some("lux")).await.unwrap());
        let tokens = crate::iam::device::login(&origin, "hanzo", Some("lux")).await.unwrap();
        owned(&tokens, Some("lux")).unwrap();

        let seen = seen.lock().unwrap();
        assert_eq!(seen.len(), 3, "{seen:?}");
        assert!(seen[1].contains("/v1/iam/oauth/device") && seen[1].contains("client_id=lux-cli"));
        assert!(seen[2].contains("device_code=dc-1") && seen[2].contains("client_id=lux-cli"));
        assert!(seen.iter().all(|req| !req.contains("hanzo-cli")), "{seen:?}");
    }

    /// Where no browser opens and stdin is not a terminal, nothing can paste
    /// the callback, so the paste leg refuses at once instead of waiting forever.
    #[tokio::test]
    async fn a_paste_leg_with_no_terminal_refuses_at_once() {
        if std::io::stdin().is_terminal() {
            return;
        }
        let err = match login("hanzo", Some("admin"), false, &Ask::default()).await {
            Err(err) => err.to_string(),
            Ok(_) => panic!("a sign-in with nothing to paste into started"),
        };
        assert!(err.contains("`admin-cli` permits no device grant"), "{err}");
        assert!(err.contains("run this at a terminal"), "{err}");
    }

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
            None,
            "http://127.0.0.1:54321/callback",
            "CHALLENGE",
            "STATE",
            &Ask::default(),
        )
        .unwrap();
        // Exact HIP-0111 path — never /api/, never legacy /oauth/authorize.
        assert_eq!(url.path(), "/v1/iam/oauth/authorize");
        let q: HashMap<_, _> = url.query_pairs().into_owned().collect();
        assert_eq!(q["response_type"], "code");
        assert_eq!(q["client_id"], "hanzo-cli");
        assert_eq!(q["code_challenge_method"], "S256");
        assert_eq!(q["code_challenge"], "CHALLENGE");
        assert_eq!(q["state"], "STATE");
        assert_eq!(q["scope"], SCOPE);
        assert_eq!(q["redirect_uri"], "http://127.0.0.1:54321/callback");
        assert!(
            !q.contains_key("prompt"),
            "a plain login reuses the browser's session"
        );
        assert!(!q.contains_key("login_hint"));

        let chosen = build_authorize_url(
            "https://hanzo.id",
            Some("admin"),
            "http://127.0.0.1:1/callback",
            "C",
            "S",
            &Ask { choose: true, hint: None },
        )
        .unwrap();
        let q: HashMap<_, _> = chosen.query_pairs().into_owned().collect();
        assert_eq!(q["prompt"], "select_account");
        assert_eq!(q["client_id"], "admin-cli", "--as admin signs in through admin's own client");

        // `--account`: the hint, and no chooser, since the account is named.
        let named = build_authorize_url(
            "https://hanzo.id",
            None,
            "http://127.0.0.1:1/callback",
            "C",
            "S",
            &Ask { choose: false, hint: Some("z@hanzo.ai".into()) },
        )
        .unwrap();
        let q: HashMap<_, _> = named.query_pairs().into_owned().collect();
        assert_eq!(q["login_hint"], "z@hanzo.ai");
        assert!(!q.contains_key("prompt"));
    }

    #[test]
    fn the_client_is_the_orgs_own_cli() {
        assert_eq!(client(None), "hanzo-cli");
        assert_eq!(client(Some("admin")), "admin-cli");
        assert_eq!(client(Some("lux")), "lux-cli");
    }

    #[test]
    fn a_token_outside_the_named_org_is_refused() {
        let held = |owner: &str| TokenSet {
            access_token: jwt(owner, "z"),
            token_type: "Bearer".into(),
            refresh_token: None,
            id_token: None,
            expires_in: None,
            scope: None,
        };
        owned(&held("hanzo"), None).unwrap();
        owned(&held("admin"), Some("admin")).unwrap();
        let err = owned(&held("hanzo"), Some("admin")).unwrap_err().to_string();
        assert!(err.contains("hanzo/z") && err.contains("admin-cli"), "{err}");
        assert!(err.contains("not in admin") && err.contains("nothing was stored"), "{err}");
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

    /// The page names who signed in and who the terminal goes on as, and
    /// offers nothing to click: switching is `hanzo auth use`, in the terminal.
    #[test]
    fn signed_in_page_names_the_account_and_the_one_the_terminal_uses() {
        let who = Shown {
            label: "z@hanzo.ai".into(),
            id: "hanzo/z".into(),
        };
        let page = signed_in_page("https://hanzo.id", Some(&who), None);
        assert!(page.contains("The terminal continues as hanzo/z."), "{page}");
        assert!(page.contains("z@hanzo.ai"), "{page}");
        assert!(page.contains("top:16px;right:16px"), "{page}");
        assert_eq!(page.matches("href=\"https://hanzo.ai\"").count(), 2, "{page}");
        for gone in ["/switch", "/remove", "/add", "<details", "Add account"] {
            assert!(!page.contains(gone), "{gone}: {page}");
        }

        let kept = signed_in_page("https://hanzo.id", Some(&who), Some("hanzo/grundy"));
        assert!(kept.contains("Added hanzo/z. The terminal still uses hanzo/grundy."), "{kept}");
        assert!(kept.contains("<code>hanzo auth use hanzo/z</code>"), "{kept}");

        let hostile = Shown {
            label: "<b>x</b>".into(),
            id: "hanzo/<i>".into(),
        };
        let page = signed_in_page("https://hanzo.id", Some(&hostile), Some("hanzo/<s>"));
        assert!(!page.contains("<b>x") && !page.contains("<i>") && !page.contains("<s>"), "{page}");
    }

    // The first write is not the tab's only try. Safari retries the redirect,
    // and the process has to be there for it while the identity is filed,
    // then gone: nothing listens once the command has returned.
    #[tokio::test]
    async fn finish_answers_the_tab_and_its_retry_then_closes_the_port() {
        use tokio::net::TcpStream;

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let mut tab = TcpStream::connect(addr).await.unwrap();
        let (held_stream, _) = listener.accept().await.unwrap();
        let held = Held {
            listener,
            origin: "https://hanzo.id".into(),
            waiting: vec![held_stream],
        };
        let tokens = TokenSet {
            access_token: jwt("hanzo", "z"),
            token_type: "Bearer".into(),
            refresh_token: None,
            id_token: None,
            expires_in: None,
            scope: None,
        };
        let (go, wait) = tokio::sync::oneshot::channel::<()>();
        let finished = tokio::spawn(async move {
            finish(held, &tokens, None, async {
                let _ = wait.await;
                Ok(7)
            })
            .await
        });

        let mut page = Vec::new();
        tab.read_to_end(&mut page).await.unwrap();
        let page = String::from_utf8_lossy(&page);
        assert!(page.starts_with("HTTP/1.1 200 OK"), "{page}");
        assert!(page.contains("The terminal continues as hanzo/z."), "{page}");

        let mut retry = TcpStream::connect(addr).await.unwrap();
        retry
            .write_all(b"GET /callback?code=abc&state=xyz HTTP/1.1\r\nHost: localhost\r\n\r\n")
            .await
            .unwrap();
        let mut again = Vec::new();
        retry.read_to_end(&mut again).await.unwrap();
        assert!(String::from_utf8_lossy(&again).contains("You're signed in"));

        go.send(()).unwrap();
        assert_eq!(finished.await.unwrap().unwrap(), 7);
        assert!(TcpStream::connect(addr).await.is_err(), "the port stayed open");
    }

    /// RP-Initiated Logout through the CLI's own client: the loopback and state
    /// when a browser comes back here, neither when it cannot, and never an id
    /// token in a URL.
    #[test]
    fn the_end_session_url_names_the_client_and_its_loopback() {
        let url = end_session_url(
            "https://hanzo.id",
            "hanzo-cli",
            Some(("http://127.0.0.1:5123/callback", "S-1")),
        )
        .unwrap();
        assert_eq!(url.path(), "/v1/iam/oauth/logout");
        let q: HashMap<_, _> = url.query_pairs().into_owned().collect();
        assert_eq!(q["client_id"], "hanzo-cli");
        assert_eq!(q["post_logout_redirect_uri"], "http://127.0.0.1:5123/callback");
        assert_eq!(q["state"], "S-1");
        assert!(!q.contains_key("id_token_hint"));

        let bare = end_session_url("https://hanzo.id", "hanzo-cli", None).unwrap();
        let q: HashMap<_, _> = bare.query_pairs().into_owned().collect();
        assert_eq!(q.len(), 1, "{bare}");
    }

    /// The sign-out loopback answers only IAM's return with our state. Another
    /// tab's return is not it and the wait goes on.
    #[tokio::test]
    async fn the_sign_out_loopback_waits_for_its_own_state() {
        use tokio::net::TcpStream;

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { signed_out(&listener, "mine", "https://hanzo.id").await });

        for target in ["/favicon.ico", "/callback?state=other", "/callback"] {
            let mut stray = TcpStream::connect(addr).await.unwrap();
            stray
                .write_all(format!("GET {target} HTTP/1.1\r\nHost: localhost\r\n\r\n").as_bytes())
                .await
                .unwrap();
            let mut answer = Vec::new();
            stray.read_to_end(&mut answer).await.unwrap();
            assert!(String::from_utf8_lossy(&answer).starts_with("HTTP/1.1 204"), "{target}");
        }
        assert!(!server.is_finished());

        let mut tab = TcpStream::connect(addr).await.unwrap();
        tab.write_all(b"GET /callback?state=mine HTTP/1.1\r\nHost: localhost\r\n\r\n")
            .await
            .unwrap();
        let mut page = Vec::new();
        tab.read_to_end(&mut page).await.unwrap();
        let page = String::from_utf8_lossy(&page);
        assert!(page.starts_with("HTTP/1.1 200 OK"), "{page}");
        assert!(page.contains("You're signed out"), "{page}");
        assert!(server.await.unwrap());
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

    /// The paste leg shares the runtime with the loopback. When the keyboard
    /// is not a terminal it must not take a blocking read, or `hanzo login`
    /// never returns after the browser already won.
    #[tokio::test]
    async fn wire_a_paste_does_not_hold_the_runtime_when_nothing_is_typed() {
        if std::io::stdin().is_terminal() {
            return;
        }
        let pending = paste_callback("state");
        tokio::pin!(pending);
        let waited = tokio::time::timeout(std::time::Duration::from_millis(80), pending).await;
        assert!(waited.is_err(), "paste completed with nothing to read");
    }

    /// Exchange, refresh, and revocation talk to whatever origin they are
    /// given. A local stand-in answers every status the caller branches on.
    #[tokio::test]
    async fn wire_exchange_refresh_and_revoke_use_the_token_endpoint() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move {
            loop {
                let Ok((mut stream, _)) = listener.accept().await else { break };
                let req = read_http(&mut stream).await;
                let (status, body) = iam_answer(&req);
                let response = format!(
                    "HTTP/1.1 {status}\r\nContent-Type: application/json\r\n\
                     Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = stream.write_all(response.as_bytes()).await;
                let _ = stream.shutdown().await;
            }
        });

        let denied = exchange_code(&origin, None, "nope", "http://127.0.0.1/callback", "ver")
            .await
            .unwrap_err()
            .to_string();
        assert!(denied.contains("token exchange failed"), "{denied}");
        assert!(denied.contains("invalid_grant"), "{denied}");

        let garbled = exchange_code(&origin, None, "garbled", "http://127.0.0.1/callback", "ver")
            .await
            .unwrap_err()
            .to_string();
        assert!(garbled.contains("parsing token response"), "{garbled}");

        let tokens = exchange_code(&origin, None, "the-code", "http://127.0.0.1/callback", "the-verifier")
            .await
            .unwrap();
        assert!(tokens.access_token.contains('.'), "{}", tokens.access_token);
        assert_eq!(tokens.refresh_token.as_deref(), Some("rt-next"));

        let spent = refresh(&origin, &issued_to("hanzo-cli", "nope")).await.unwrap_err().to_string();
        assert!(spent.contains("token refresh refused"), "{spent}");

        let fresh = refresh(&origin, &issued_to("hanzo-cli", "rt-1")).await.unwrap();
        assert_eq!(fresh.refresh_token.as_deref(), Some("rt-2"));
        assert!(!fresh.access_token.is_empty());

        let revoked = revoke(&origin, &issued_to("hanzo-cli", "nope")).await.unwrap_err().to_string();
        assert!(revoked.contains("revocation failed"), "{revoked}");
        revoke(&origin, &issued_to("hanzo-cli", "rt-1")).await.unwrap();
    }

    /// Nothing is listening: each call fails as a transport error, before it
    /// tries to read a token.
    #[tokio::test]
    async fn wire_a_closed_token_endpoint_is_a_failure() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        drop(listener);

        for err in [
            exchange_code(&origin, None, "c", "http://127.0.0.1/callback", "v")
                .await
                .unwrap_err()
                .to_string(),
            refresh(&origin, &issued_to("hanzo-cli", "rt")).await.unwrap_err().to_string(),
            revoke(&origin, &issued_to("hanzo-cli", "rt")).await.unwrap_err().to_string(),
            served(&origin, Some("admin")).await.unwrap_err().to_string(),
        ] {
            assert!(err.contains("calling IAM"), "{err}");
        }
    }

    async fn read_http(stream: &mut TcpStream) -> String {
        let mut buf = Vec::new();
        let mut tmp = [0u8; 2048];
        loop {
            let Ok(n) = tokio::time::timeout(
                std::time::Duration::from_millis(500),
                stream.read(&mut tmp),
            )
            .await
            else {
                break;
            };
            let Ok(n) = n else { break };
            if n == 0 {
                break;
            }
            buf.extend_from_slice(&tmp[..n]);
            if let Some(end) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                let headers = String::from_utf8_lossy(&buf[..end]);
                let len = headers
                    .lines()
                    .find_map(|line| {
                        let (name, value) = line.split_once(':')?;
                        name.eq_ignore_ascii_case("content-length")
                            .then(|| value.trim().parse::<usize>().ok())?
                    })
                    .unwrap_or(0);
                if buf.len() >= end + 4 + len {
                    break;
                }
            }
        }
        String::from_utf8_lossy(&buf).into_owned()
    }

    fn iam_answer(req: &str) -> (&'static str, &'static str) {
        let path_is_revoke = req.contains("/v1/iam/oauth/revoke");
        if path_is_revoke {
            return if req.contains("token=nope") {
                ("400 Bad Request", r#"{"error":"invalid_token"}"#)
            } else {
                ("200 OK", "")
            };
        }
        if req.contains("code=nope") {
            return ("400 Bad Request", r#"{"error":"invalid_grant"}"#);
        }
        if req.contains("code=garbled") {
            return ("200 OK", "nope");
        }
        if req.contains("grant_type=refresh_token") && req.contains("refresh_token=nope") {
            return ("401 Unauthorized", r#"{"error":"invalid_grant"}"#);
        }
        if req.contains("grant_type=refresh_token") {
            return (
                "200 OK",
                r#"{"access_token":"header.payload.sig","token_type":"Bearer","refresh_token":"rt-2"}"#,
            );
        }
        (
            "200 OK",
            r#"{"access_token":"header.payload.sig","token_type":"Bearer","refresh_token":"rt-next"}"#,
        )
    }
}
