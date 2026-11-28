//! Request dispatch, stdin secret injection, authenticated Cloud seam, and output rendering.

use anyhow::{anyhow, bail, Context, Result};
use hanzo_client::{Http, Method, Reply, Request, Transport};
use serde_json::{json, Map, Value};
use std::io::{IsTerminal, Read};

use crate::commands::cloud_sessions::finished;
use crate::commands::host::{self, Origin};
use crate::config::Config;
use crate::iam::{paths, store};
#[cfg(unix)]
use crate::zap;

use super::{
    command_words, flags_not_data, insert_path, memberships_query_ok, Field, HttpStatusError,
    LeafBody, Op, Output, Resolved, Submit,
};

pub async fn dispatch(cfg: &mut Config, resolved: Resolved) -> Result<()> {
    let Resolved::Leaf {
        op,
        values,
        body,
        query,
        output,
        submit,
    } = resolved;
    if op.product == "iam" && op.nodes == ["memberships"] && op.verb == "get"
        && !memberships_query_ok(&query)
    {
        anyhow::bail!("iam memberships get needs exactly one of --user or --org");
    }
    let owner = store::acting(cfg, paths::DEFAULT_BRAND).map(|i| i.owner);
    let path = fill_path(op.path, op.rest, owner.as_deref(), &values)?;
    let method = parse_method(op.method)?;
    let body = match body {
        // A typed body may carry a stdin-secret field, read here (the IO layer)
        // so `resolve` stays pure and the value never passes through argv.
        LeafBody::Typed(v) => Some(named(op, inject_secret(op, v)?)),
        LeafBody::Data(d) => read_body(d, &method)?,
        LeafBody::Misused => bail!("{}", flags_not_data(op)),
        LeafBody::None => None,
    };
    match submit {
        Submit::Sync => call(cfg, method, path, body, query, output).await,
        Submit::Async | Submit::Wait(..) => {
            // `agent run` has exactly one path parameter: the ref the run is of.
            let r = values.first().ok_or_else(|| anyhow!("missing agent ref"))?;
            let async_body = async_body(r, body)?;
            let seam = Seam::open(cfg, paths::DEFAULT_BRAND).await?;
            let reply = seam
                .send(Method::POST, "/v1/agent/coding", &[], Some(async_body))
                .await?;
            let id = accepted_session(reply)?;
            println!("Agent run started: {id}");
            println!(
                "Follow: hanzo agent sessions get {id}  ·  hanzo agent sessions events get {id}"
            );
            match submit {
                Submit::Wait(seconds) => follow_session(&seam, &id, seconds).await,
                _ => Ok(()),
            }
        }
    }
}

/// The async route's body: the caller's `--data` object, addressed to the SAME
/// agent, with an idempotency key when the caller gave none. A non-object body
/// is refused — the coding route takes a typed object, not an array or scalar.
pub(super) fn async_body(r: &str, body: Option<Value>) -> Result<Value> {
    let mut obj = match body {
        Some(Value::Object(m)) => m,
        Some(_) => bail!("--async needs a JSON object in --data (the /v1/agent/coding body)"),
        None => Map::new(),
    };
    if r.trim().is_empty() {
        bail!("an agent run needs a ref");
    }
    obj.entry("agentRef".to_string())
        .or_insert_with(|| json!(r));
    obj.entry("requestId".to_string())
        .or_insert_with(|| json!(request_id()));
    Ok(Value::Object(obj))
}

/// Unwrap the async route's 202 the same way `call` unwraps any other answer,
/// and validate the session id BEFORE it is put into a follow-up URL.
pub(super) fn accepted_session(Reply { status, body }: Reply) -> Result<String> {
    if !(200..300).contains(&status) {
        let shown = match &body {
            Value::Null => String::new(),
            Value::String(s) => s.trim().to_string(),
            v => v.to_string(),
        };
        return Err(HttpStatusError {
            path: "/v1/agent/coding".to_string(),
            status,
            detail: shown,
            hint: None,
        }
        .into());
    }
    if let Some(msg) = envelope_error(&body) {
        bail!("/v1/agent/coding: {msg}");
    }
    let value = body.get("data").cloned().unwrap_or(body);
    let id = value.get("sessionId").and_then(Value::as_str).unwrap_or("");
    if id.is_empty()
        || !id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    {
        bail!("the async route answered no usable session id: {value}");
    }
    Ok(id.to_owned())
}

/// Poll the same session GET `hanzo agent sessions get` uses until the run ends
/// or the deadline. Each request is short and independent, so losing this
/// terminal costs the FOLLOW, never the run itself.
async fn follow_session(seam: &Seam, id: &str, seconds: u64) -> Result<()> {
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(seconds);
    let mut last = String::new();
    loop {
        let Reply { status: code, body } = seam
            .send(Method::GET, &format!("/v1/agent/sessions/{id}"), &[], None)
            .await?;
        if !(200..300).contains(&code) {
            return Err(HttpStatusError {
                path: format!("polling session {id}"),
                status: code,
                detail: body.to_string(),
                hint: None,
            }
            .into());
        }
        let value = body.get("data").cloned().unwrap_or(body);
        let current = value.get("status").and_then(Value::as_str).unwrap_or("");
        if current != last {
            println!("Session {id}: {current}");
            last = current.to_owned();
        }
        if finished(current) {
            if current == "error" {
                bail!("agent run {id} failed");
            }
            return Ok(());
        }
        if tokio::time::Instant::now() >= deadline {
            bail!("agent run {id} is still {current} after {seconds}s; follow it with `hanzo agent sessions get {id}`");
        }
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    }
}

/// A fresh id for one invocation: the platform answers a request that carries it once, so a request made
/// again after a failure cannot do its work twice.
pub(crate) fn request_id() -> String {
    format!("cli-{:032x}", rand::random::<u128>())
}

/// The body of an operation that declares a `requestId`, with one given when the caller gave none.
pub(super) fn named(op: &Op, mut body: Value) -> Value {
    if op.fields.iter().any(|f| f.key == "requestId" && !f.query) {
        if let Some(map) = body.as_object_mut() {
            map.entry("requestId")
                .or_insert_with(|| json!(request_id()));
        }
    }
    body
}

/// Fill a secret body field (`kms secrets create`'s `value`, `integrations
/// connect`'s `token`) through the ONE secret law (`iam::secret`). Because the
/// field has no flag and no positional, this is the ONLY way a secret enters the
/// body — it can never come from argv, and a value-bearing argv is a parse error
/// rather than a matter of discipline. A single op reads stdin exactly once; more
/// than one secret field is an authoring error we refuse rather than read twice.
///
/// A PIPE is read; a TERMINAL is PROMPTED, hidden. It used to read stdin either
/// way, so a person running this at a terminal typed their credential in the
/// clear — the reason a hand-written `hanzo connector add` survived beside the
/// generated command for the same four routes.
pub(super) fn inject_secret(op: &Op, mut body: Value) -> Result<Value> {
    use crate::iam::secret;
    let mut secrets = op.fields.iter().filter(|f| f.secret);
    let Some(f) = secrets.next() else {
        return Ok(body);
    };
    if secrets.next().is_some() {
        anyhow::bail!(
            "{} declares more than one secret field — an op reads stdin once",
            op.path
        );
    }
    // A secret field has no flag, so `None` is the only question that can be
    // asked: nothing could have carried a literal here for the law to refuse.
    let value = match secret::secret_source(None, secret::stdin_is_tty()) {
        secret::SecretSource::Prompt => secret::prompt(&prompt_label(op, f))?,
        _ => secret::read_secret(std::io::stdin().lock())?,
    };
    let obj = body
        .as_object_mut()
        .ok_or_else(|| anyhow!("a typed body must be a JSON object"))?;
    insert_path(obj, f.key, Value::String(value));
    Ok(body)
}

/// What the hidden prompt asks for, and for which command.
pub(super) fn prompt_label(op: &Op, f: &Field) -> String {
    format!("{} for `hanzo {}`", f.flag, command_words(op))
}

/// Fill a `/v1` template: a param preceded by `orgs` is the tenant scope, bound
/// to `owner` (refused when signed out); every other param takes the next
/// positional. Values are percent-encoded so a value cannot re-address a
/// different route — the same rule `kms` uses. This mirrors the generator's
/// scope predicate; their agreement is pinned by `every_op_fills_to_a_path`.
pub(super) fn fill_path(
    template: &str,
    rest: &[&str],
    owner: Option<&str>,
    values: &[String],
) -> Result<String> {
    let raw: Vec<&str> = template.split('/').collect();
    let mut out = String::new();
    let mut it = values.iter();
    for (i, seg) in raw.iter().enumerate() {
        if i > 0 {
            out.push('/');
        }
        match seg.strip_prefix('{').and_then(|s| s.strip_suffix('}')) {
            Some(name) => {
                let prev = if i > 0 { raw[i - 1] } else { "" };
                if prev == "orgs" {
                    let o =
                        owner.ok_or_else(|| anyhow!("not signed in — run `hanzo auth login`"))?;
                    out.push_str(&enc(o));
                } else {
                    let v = it
                        .next()
                        .ok_or_else(|| anyhow!("missing value for {{{name}}}"))?;
                    // A MULTI-SEGMENT (catch-all) param keeps its slashes raw so the
                    // server route resolves; a single-segment one `%2F`-escapes them.
                    if rest.contains(&name) {
                        out.push_str(&enc_path(v)?);
                    } else {
                        out.push_str(&enc(v));
                    }
                }
            }
            None => out.push_str(seg),
        }
    }
    Ok(out)
}

/// Encode a MULTI-SEGMENT path value whose `/` are STRUCTURAL — a server catch-all
/// (e.g. a KMS secret `sub/path/name`). Each segment is percent-encoded by [`enc`]
/// and the slashes stay raw, so the value addresses the secret the user named
/// rather than a `%2F`-mangled one the backend 404s. `.`/`..`/empty segments are
/// refused BEFORE a URL exists, so a value can never `../../` its way onto another
/// org's route — the same protection the flat case gets for free from `%2F`.
pub(super) fn enc_path(s: &str) -> Result<String> {
    let mut out = String::with_capacity(s.len());
    for (i, seg) in s.split('/').enumerate() {
        if seg.is_empty() || seg == "." || seg == ".." {
            bail!("invalid path segment {seg:?} in {s:?}");
        }
        if i > 0 {
            out.push('/');
        }
        out.push_str(&enc(seg));
    }
    Ok(out)
}

/// Percent-encode one URL path segment: everything outside the RFC 3986
/// unreserved set becomes `%XX`, so a value with `/`, `?` or `#` addresses the
/// segment the user meant, never a different route.
pub(super) fn enc(s: &str) -> String {
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

// ---- the ONE authenticated call into cloud (the seam the tree dispatches on) --

/// Resolve WHERE (the active network's origin) and WHO (the active identity's
/// bearer) through the one identity seam, send the request, print the `data`, and
/// explain a 403 in identity terms. The org is NEVER a header: where a route names
/// it in the PATH, the caller has already addressed it (its own `owner`), and the
/// server re-checks it against the JWT it verifies. `path` is the FINAL
/// template-filled `/v1/…` — never a fetched spec, never a user-supplied host.
pub(crate) async fn call(
    cfg: &mut Config,
    method: Method,
    path: String,
    body: Option<Value>,
    query: Vec<String>,
    output: Option<Output>,
) -> Result<()> {
    let seam = Seam::open(cfg, paths::DEFAULT_BRAND).await?;
    let Reply { status, body: resp } = seam.send(method, &path, &query, body).await?;

    if (200..300).contains(&status) {
        // A 2xx is NOT proof of success. Some planes (IAM) answer an error
        // with HTTP 200 and an `{"status":"error","msg":…}` envelope; rendering
        // only `data` then prints nothing and exits 0, silently swallowing the
        // refusal. Surface the server's own message to stderr with a non-zero exit.
        if let Some(msg) = envelope_error(&resp) {
            bail!("{path}: {msg}");
        }
        print_body_for_path(&path, &resp, output);
        return Ok(());
    }

    // Non-2xx: surface the SERVER's own body. The refusal is the server's,
    // never a client-side guess; a 403 already names who was refused and why.
    let shown = match &resp {
        Value::Null => String::new(),
        Value::String(s) => s.trim().to_string(),
        v => v.to_string(),
    };
    let hint =
        (status == 401 && seam.token.is_empty()).then_some("not signed in; run `hanzo auth login`");
    Err(HttpStatusError {
        path,
        status,
        detail: shown,
        hint,
    }
    .into())
}

/// WHERE and WHO, resolved ONCE — the one authenticated route into cloud, held
/// as a value so a command that reads SEVERAL surfaces (`hanzo status`) resolves
/// the origin and the credential once and then fans out concurrently, instead of
/// re-deriving them per call and drifting.
pub(crate) struct Seam {
    origin: Origin,
    /// The bearer. Cloud derives the tenant from the `owner` claim it verifies,
    /// so a request that names no org acts in the caller's own — which is every
    /// request unless [`Seam::org`] is set.
    pub(crate) token: String,
    /// The org to act in, when the caller named one (`--org`).
    ///
    /// It rides as `X-Org-Id` and is a SELECTION, not an assertion: the gateway
    /// checks it against the IAM-signed `orgs` membership claim and discards a
    /// value outside that set, so naming an org one does not belong to is a
    /// no-op rather than an escalation. Without it an operator who belongs to
    /// several orgs could only ever see one of them.
    pub(crate) org: Option<String>,
    wire: Http,
}

impl Seam {
    /// The seam as `brand`'s active identity: the CLI's own commands speak as
    /// the default brand, and a session's meter as the brand the session runs as.
    pub(crate) async fn open(cfg: &mut Config, brand: &str) -> Result<Self> {
        // WHERE, and over WHICH WIRE. A LOCAL origin also gets a host started (or
        // reused) behind it, so the same command tree runs against a checkout with
        // no server up — over ZAP, the native transport, because the CLI is first
        // party.
        let origin = host::origin(cfg).await?;

        // A MACHINE CALLER'S KEY, read at the point of use. An API key names no
        // principal, so it is never filed as an identity (Identity::from_access_token
        // refuses one); a caller that holds only a key — a bot in a sandbox, a CI
        // step — sets HANZO_API_KEY and every cloud command speaks as that key. It
        // wins over a stored login for the reason `hanzo dev`'s provider tries it
        // first: the variable is chosen for this process, the login is the machine's
        // default. Without it a box answered `not signed in`, and a box signed in as a
        // person read none of the sessions its key had opened (404).
        if let Some(token) = machine_key(std::env::var_os("HANZO_API_KEY")) {
            return Ok(Self {
                origin,
                token,
                org: cfg.org.clone(),
                wire: Http::default(),
            });
        }

        // WHO. A local host may have no IAM to sign in to, so a missing credential
        // is NOT refused here — the call goes out unauthenticated and the server
        // decides. That is the same rule this module already follows for a 403:
        // the refusal is always the server's, never a client-side guess.
        let identity = store::active_token(cfg, brand).await?;
        // Public routes (`models list`, health) answer without a credential, so a
        // signed-out call goes out anonymously; a protected route answers 401 and
        // the error says how to sign in.
        let token = identity.map(|(_, t)| t.access_token).unwrap_or_default();
        Ok(Self {
            origin,
            token,
            org: cfg.org.clone(),
            wire: Http::default(),
        })
    }

    pub(crate) fn is_http_origin(&self) -> bool {
        matches!(self.origin, Origin::Http(_))
    }

    /// Send one request to an explicit HTTP origin (e.g. `https://post.hanzo.ai`
    /// fallback while `/v1/channel` & `/v1/post` finish rolling onto `api.hanzo.ai`)
    /// carrying the exact same IAM bearer and `X-Org-Id` selection.
    pub(crate) async fn send_at(
        &self,
        base_url: &str,
        method: Method,
        path: &str,
        query: &[String],
        body: Option<Value>,
    ) -> Result<Reply> {
        let target = target(path, query)?;
        let base = base_url.trim_end_matches('/');
        self.over_http(&format!("{base}{target}"), method, body)
            .await
    }

    /// Send one request over whichever wire the origin named. The STATUS is
    /// handed back, never flattened: a refusal is a value the caller must read,
    /// and only a transport fault is an `Err`.
    pub(crate) async fn send(
        &self,
        method: Method,
        path: &str,
        query: &[String],
        body: Option<Value>,
    ) -> Result<Reply> {
        let target = target(path, query)?;
        match &self.origin {
            #[cfg(unix)]
            Origin::Local(sock) => {
                zap::send(sock, &method, &target, &self.token, body.as_ref()).await
            }
            #[cfg(not(unix))]
            Origin::Local(base) => {
                self.over_http(&format!("{base}{target}"), method, body)
                    .await
            }
            Origin::Http(base) => {
                self.over_http(&format!("{base}{target}"), method, body)
                    .await
            }
        }
    }

    async fn over_http(&self, url: &str, method: Method, body: Option<Value>) -> Result<Reply> {
        let mut request = Request::new(method, url).token(&self.token);
        if let Some(org) = &self.org {
            request = request.org(org);
        }
        if let Some(body) = body {
            request = request.body(body);
        }
        Ok(self.wire.send(request).await?)
    }
}

/// The bearer a machine caller set in `HANZO_API_KEY`, or `None` when it is unset,
/// blank or not text. Surrounding whitespace is a paste, not part of the key.
pub(super) fn machine_key(var: Option<std::ffi::OsString>) -> Option<String> {
    let key = var?.into_string().ok()?;
    let key = key.trim();
    (!key.is_empty()).then(|| key.to_string())
}

/// Map an op's method string to a `reqwest::Method`.
pub(super) fn parse_method(m: &str) -> Result<Method> {
    match m {
        "GET" => Ok(Method::GET),
        "POST" => Ok(Method::POST),
        "PUT" => Ok(Method::PUT),
        "PATCH" => Ok(Method::PATCH),
        "DELETE" => Ok(Method::DELETE),
        "HEAD" => Ok(Method::HEAD),
        other => anyhow::bail!("unsupported method {other:?}"),
    }
}

/// `--data` is JSON; `-` reads stdin so a secret in a body never lands in argv,
/// `ps` or shell history — the same rule as `kms set`. A body on a GET/HEAD is a
/// named error, not silently sent.
pub(super) fn read_body(data: Option<String>, method: &Method) -> Result<Option<Value>> {
    let Some(d) = data else { return Ok(None) };
    if matches!(*method, Method::GET | Method::HEAD) {
        anyhow::bail!("--data is not sent on a {method} — this verb takes no body");
    }
    let raw = if d == "-" {
        let mut s = String::new();
        std::io::stdin()
            .read_to_string(&mut s)
            .context("reading --data from stdin")?;
        s
    } else {
        d
    };
    let value: Value = serde_json::from_str(raw.trim())
        .context("--data must be valid JSON (use `-` to read a JSON body from stdin)")?;
    Ok(Some(value))
}

/// The origin-form request target: `/path` plus any `--query k=v`, percent-
/// encoded. ZAP puts this in the target slot and HTTP appends it to its origin,
/// so a query is encoded exactly ONCE, here, for both wires. Values go through
/// `reqwest::Url`, so a `k=a b&c` cannot forge extra parameters.
pub(super) fn target(path: &str, query: &[String]) -> Result<String> {
    // A placeholder authority, only so the URL crate will parse an origin-form
    // path and lend its query encoder. Nothing reads it back — the host on the
    // wire comes from the transport, never from here.
    let mut url = reqwest::Url::parse(&format!("http://local{path}"))
        .with_context(|| format!("building request target {path}"))?;
    if !query.is_empty() {
        let mut pairs = url.query_pairs_mut();
        for q in query {
            let (k, v) = q
                .split_once('=')
                .ok_or_else(|| anyhow!("--query must be k=v (got {q:?})"))?;
            pairs.append_pair(k, v);
        }
    }
    Ok(match url.query() {
        Some(q) => format!("{}?{}", url.path(), q),
        None => url.path().to_string(),
    })
}

/// The keys a `/v1` envelope carries: `{status,msg,data}`, IAM's `sub`, `name` and
/// `data2`, and the bare `error`/`code` form.
const ENVELOPE: &[&str] = &[
    "status", "msg", "data", "data2", "sub", "name", "error", "code",
];

/// Read the cloud `/v1` envelope (`{status,msg,data}`) for an error the HTTP status
/// did not carry. Some planes (IAM) answer a refusal with HTTP 200 and
/// `{"status":"error","msg":"…"}`, so a 2xx is not proof of success. Returns the
/// server's own message when the body is an ERROR — an explicit `status:"error"`
/// (any case), or a bare `{"error":"…"}` with no `data` — and `None` otherwise, so
/// a genuine success (a success envelope, or a raw non-enveloped body) renders
/// unchanged. It never invents a message and never fires on a success that merely
/// has a `msg` with null `data`: an explicit non-error `status` wins.
///
/// Only a body made of envelope keys is an envelope. A body with any other key is a
/// resource, and its `status`/`error` are its own state: a failed coding run is
/// `{"id":…,"status":"error","error":…}`, a success to render, not a refusal.
pub(crate) fn envelope_error(body: &Value) -> Option<String> {
    let obj = body.as_object()?;
    if !obj.keys().all(|k| ENVELOPE.contains(&k.as_str())) {
        return None;
    }
    let message = || {
        obj.get("msg")
            .or_else(|| obj.get("error"))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or("request failed")
            .to_string()
    };
    match obj.get("status").and_then(Value::as_str) {
        // An explicit error status is a failure regardless of the HTTP code.
        Some(s) if s.eq_ignore_ascii_case("error") || s.eq_ignore_ascii_case("failed") => {
            Some(message())
        }
        // Any other explicit status (`ok`/`success`/…) is the server saying success.
        Some(_) => None,
        // No envelope status: a bare `{"error":"…"}` carrying no payload is still an
        // error; a raw body (an array, or an object that IS the data) is not.
        None => {
            let has_data = obj.get("data").is_some_and(|d| !d.is_null());
            let bare_error = obj
                .get("error")
                .and_then(Value::as_str)
                .is_some_and(|s| !s.trim().is_empty());
            (bare_error && !has_data).then(message)
        }
    }
}

/// Whether `path` names a model catalog endpoint whose retired `enso-free`
/// entries must never be surfaced in CLI lists or pickers.
fn is_model_catalog_path(path: &str) -> bool {
    let clean = path.split('?').next().unwrap_or(path).trim_end_matches('/');
    clean == "/v1/models" || clean.ends_with("/models")
}

/// Strip retired `enso-free` entries from a model catalog payload (preserving
/// every other model and envelope shape).
pub(super) fn sanitize_model_catalog(v: &Value) -> Value {
    let hidden = |item: &Value| {
        item.get("id")
            .and_then(Value::as_str)
            .is_some_and(hanzo_gen::enso::is_hidden_enso_model)
            || item
                .get("name")
                .and_then(Value::as_str)
                .is_some_and(hanzo_gen::enso::is_hidden_enso_model)
    };
    match v {
        Value::Array(items) => {
            Value::Array(items.iter().filter(|it| !hidden(it)).cloned().collect())
        }
        Value::Object(map) => {
            let mut out = map.clone();
            for key in ["data", "models"] {
                if let Some(Value::Array(items)) = out.get(key) {
                    let filtered: Vec<Value> =
                        items.iter().filter(|it| !hidden(it)).cloned().collect();
                    out.insert(key.to_string(), Value::Array(filtered));
                }
            }
            Value::Object(out)
        }
        other => other.clone(),
    }
}

/// Print the response for `path`, filtering retired `enso-free` entries from
/// model catalog endpoints before rendering in table, JSON, or YAML form.
pub(super) fn print_body_for_path(path: &str, resp: &Value, output: Option<Output>) {
    if is_model_catalog_path(path) {
        let cleaned = sanitize_model_catalog(resp);
        print_body(&cleaned, output);
    } else {
        print_body(resp, output);
    }
}

/// Print the response. The cloud `/v1` envelope is `{status,msg,data}` and what is shown is `data`. A list
/// prints as a table on a terminal and as JSON anywhere else, so a pipe still gets what it can parse; `-o`
/// names the format outright.
pub(super) fn print_body(resp: &Value, output: Option<Output>) {
    let shown = resp.get("data").unwrap_or(resp);
    let output = output.unwrap_or_else(|| {
        if rows(shown).is_some() && std::io::stdout().is_terminal() {
            Output::Table
        } else {
            Output::Json
        }
    });
    if let Some(text) = render(shown, output, terminal_width()) {
        println!("{text}");
    }
}

/// An answer in a format: nothing for none, a string as written.
pub(super) fn render(v: &Value, output: Output, width: usize) -> Option<String> {
    match (v, output) {
        (Value::Null, _) => None,
        (Value::String(s), _) => Some(s.clone()),
        (v, Output::Table) => Some(table(v, width)),
        (v, Output::Yaml) => Some(
            serde_norway::to_string(v)
                .unwrap_or_else(|_| v.to_string())
                .trim_end()
                .to_string(),
        ),
        (v, Output::Json) => Some(pretty(v)),
    }
}

pub(super) fn terminal_width() -> usize {
    crossterm::terminal::size()
        .map(|(w, _)| w as usize)
        .unwrap_or(usize::MAX)
}

/// The list an answer is: the array itself, or the one array an object carries (`{"models":[…]}`).
pub(super) fn rows(v: &Value) -> Option<&Vec<Value>> {
    match v {
        Value::Array(a) => Some(a),
        Value::Object(o) => {
            let mut arrays = o.values().filter_map(Value::as_array);
            let only = arrays.next()?;
            arrays.next().is_none().then_some(only)
        }
        _ => None,
    }
}

/// A scalar as a table cell: text as written, null as `-`.
pub(super) fn cell(v: &Value) -> Option<String> {
    match v {
        Value::Null => Some("-".into()),
        Value::String(s) => Some(s.replace('\n', " ")),
        Value::Bool(_) | Value::Number(_) => Some(v.to_string()),
        Value::Array(_) | Value::Object(_) => None,
    }
}

/// An answer as a table: a list is one row per item, a single object one row per field. The columns are the
/// scalar keys (an object's keys are sorted, so they are), the identifying ones first, as many as fit `width`,
/// the first always; nested values are left to `-o json`.
pub(super) fn table(v: &Value, width: usize) -> String {
    let (header, grid) = match (rows(v), v) {
        (Some(list), _) => match columns(list) {
            Some(shape) => shape,
            None => return pretty(v),
        },
        (None, Value::Object(o)) => fields(o),
        _ => return pretty(v),
    };
    lay(&header, &grid, width)
}

pub(super) fn pretty(v: &Value) -> String {
    serde_json::to_string_pretty(v).unwrap_or_else(|_| v.to_string())
}

/// The keys that identify an item, which lead its row.
const LEAD: [&str; 2] = ["id", "name"];

/// A list's header and rows, or `None` when its items have nothing scalar to show.
pub(super) fn columns(list: &[Value]) -> Option<(Vec<String>, Vec<Vec<String>>)> {
    let visible: Vec<&Value> = list
        .iter()
        .filter(|item| {
            !item
                .get("id")
                .and_then(Value::as_str)
                .is_some_and(hanzo_gen::enso::is_hidden_enso_model)
                && !item
                    .get("name")
                    .and_then(Value::as_str)
                    .is_some_and(hanzo_gen::enso::is_hidden_enso_model)
        })
        .collect();
    let mut keys: Vec<&str> = Vec::new();
    for item in visible.iter().copied().filter_map(Value::as_object) {
        for (key, value) in item {
            if cell(value).is_some() && !keys.contains(&key.as_str()) {
                keys.push(key);
            }
        }
    }
    if keys.is_empty() {
        return None;
    }
    keys.sort_by_key(|key| {
        LEAD.iter()
            .position(|lead| lead == key)
            .unwrap_or(LEAD.len())
    });
    let grid = visible
        .iter()
        .map(|item| {
            let id_is_enso = item
                .get("id")
                .and_then(Value::as_str)
                .is_some_and(hanzo_gen::enso::is_enso_model);
            keys.iter()
                .map(|key| {
                    let raw = item
                        .get(*key)
                        .and_then(cell)
                        .unwrap_or_else(|| "-".into());
                    match *key {
                        "id" => hanzo_gen::enso::format_model_id(&raw),
                        "name" if !id_is_enso => hanzo_gen::enso::format_model_id(&raw),
                        _ => raw,
                    }
                })
                .collect()
        })
        .collect();
    Some((keys.iter().map(|key| key.to_uppercase()).collect(), grid))
}

/// One object's header and rows: a field and its value, for each field that is a scalar.
pub(super) fn fields(o: &Map<String, Value>) -> (Vec<String>, Vec<Vec<String>>) {
    let grid = o
        .iter()
        .filter_map(|(key, value)| {
            let val = cell(value)?;
            let shown = if key == "id" {
                hanzo_gen::enso::format_model_id(&val)
            } else {
                val
            };
            Some(vec![key.clone(), shown])
        })
        .collect();
    (vec!["FIELD".into(), "VALUE".into()], grid)
}

/// The text of a table: each column as wide as its widest cell, two spaces between, and the columns that fit
/// `width` (the first always; one too wide is left out and the narrower ones after it are kept).
pub(super) fn lay(header: &[String], grid: &[Vec<String>], width: usize) -> String {
    let widths: Vec<usize> = (0..header.len())
        .map(|i| {
            grid.iter()
                .map(|row| row[i].chars().count())
                .chain([header[i].chars().count()])
                .max()
                .unwrap_or(0)
        })
        .collect();
    let mut keep: Vec<usize> = Vec::new();
    let mut used = 0;
    for (i, w) in widths.iter().enumerate() {
        let next = used + if keep.is_empty() { 0 } else { 2 } + w;
        if keep.is_empty() || next <= width {
            used = next;
            keep.push(i);
        }
    }
    let line = |cells: &[String]| {
        let padded: Vec<String> = keep
            .iter()
            .map(|&i| format!("{:<w$}", cells[i], w = widths[i]))
            .collect();
        padded.join("  ").trim_end().to_string()
    };
    std::iter::once(line(header))
        .chain(grid.iter().map(|row| line(row.as_slice())))
        .collect::<Vec<_>>()
        .join("\n")
}

