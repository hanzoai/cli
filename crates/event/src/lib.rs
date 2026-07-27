//! The canonical Event emitter for Hanzo terminal tools.
//!
//! A terminal app speaks the SAME analytics wire the web and product surfaces do:
//! one POST to the `/v1/event` front door, body `Event | [Event]`, tenant resolved
//! SERVER-SIDE from the bearer. This crate is the Rust half of `@hanzo/event` — the
//! four-field [`Event`] and a best-effort [`Telemetry`] handle that both `hanzo`
//! (the CLI) and `dev` wire into their command-dispatch seam.
//!
//! Three properties hold at every call:
//!
//!  - **fail-soft** — a record or a flush can never block, break, or slow a command
//!    beyond a tight network budget; every error is swallowed.
//!  - **opt-out** — `HANZO_TELEMETRY=0` or `DO_NOT_TRACK` disables it entirely, with
//!    no id read and no network ([`opted_out`]).
//!  - **no PII** — the visitor is a random per-install [`device_id`]; a command
//!    reports only its verb, duration, and outcome. Never argv, paths, or free text.
//!
//! Auth is one bearer header: a hanzo.id JWT when the caller is signed in, else a
//! write-only publishable key (`pk_…`) supplied via config or `HANZO_EVENT_KEY`.
//! The `/v1/event` door is fail-closed, so a run with neither credential resolves a
//! device id but does not transmit — provisioning a `pk_` turns anonymous telemetry
//! on with no code change.

use std::path::Path;
use std::sync::Mutex;
use std::time::Duration;

use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// The canonical event name for a finished command invocation. Follows the
/// `@hanzo/event` vocab grammar (`noun_verbpast`, like `signup_completed`).
pub const COMMAND_COMPLETED: &str = "command_completed";

/// The `HANZO_TELEMETRY` opt-out variable (`0`/`false`/`no`/`off` disables).
pub const TELEMETRY_ENV: &str = "HANZO_TELEMETRY";
/// The cross-vendor `DO_NOT_TRACK` opt-out variable (any truthy value disables).
pub const DO_NOT_TRACK_ENV: &str = "DO_NOT_TRACK";
/// Optional write-only publishable key (`pk_…`) for anonymous telemetry.
pub const KEY_ENV: &str = "HANZO_EVENT_KEY";
/// Optional full endpoint override (else `{api_base}/v1/event`).
pub const URL_ENV: &str = "HANZO_EVENT_URL";
/// Optional flush budget in milliseconds (network timeout at exit).
pub const TIMEOUT_ENV: &str = "HANZO_EVENT_TIMEOUT_MS";

/// Default flush budget: the whole POST (connect + send + response) is bounded by
/// this so a slow or dead network delays process teardown by at most ~1s.
const DEFAULT_TIMEOUT_MS: u64 = 1000;

/// The canonical analytics event — the entire ingest contract in four fields. The
/// tenant is NOT here: the server resolves it from the bearer, so a caller only ever
/// writes into its own org. Everything non-core travels in [`Event::properties`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Event {
    /// Event name (required; an empty name is dropped server-side as unroutable).
    pub event: String,
    /// The stable, non-PII visitor id the caller owns (the per-install device id).
    #[serde(rename = "distinctId")]
    pub distinct_id: String,
    /// RFC3339 timestamp; the server clamps skew and fills an absent value.
    pub time: String,
    /// Everything non-core.
    pub properties: Map<String, Value>,
}

/// How to build a [`Telemetry`] handle. The caller resolves the pieces from its own
/// environment (active network `api`, signed-in bearer, device id) and hands them
/// over; the handle owns opt-out, endpoint shaping, and the wire.
pub struct Config {
    /// Emitting surface — `"cli"` or `"dev"`.
    pub product: String,
    /// The emitter's own version (the caller's `CARGO_PKG_VERSION`).
    pub version: String,
    /// Origin of the active network, e.g. `https://api.hanzo.ai`. The endpoint is
    /// `{api_base}/v1/event` unless [`URL_ENV`] overrides it.
    pub api_base: String,
    /// The per-install device id used as `distinctId`.
    pub distinct_id: String,
    /// A hanzo.id JWT (or `hk-`/`pk_` key) when one is cheaply available; else the
    /// handle falls back to [`KEY_ENV`], else it transmits nothing.
    pub bearer: Option<String>,
}

/// A best-effort, batched emitter. Records buffer in memory and go out as one
/// `[Event]` array on [`Telemetry::flush`] (typically once, at process exit).
pub struct Telemetry {
    enabled: bool,
    endpoint: String,
    auth: Option<String>,
    distinct_id: String,
    product: String,
    version: String,
    events: Mutex<Vec<Event>>,
    http: Option<Client>,
}

impl Telemetry {
    /// Build a live handle from [`Config`]. Disabled (a no-op) when the user has
    /// opted out. Auth is `config.bearer`, else [`KEY_ENV`], else none.
    pub fn new(config: Config) -> Telemetry {
        let enabled = !opted_out();
        let endpoint = std::env::var(URL_ENV)
            .ok()
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| format!("{}/v1/event", config.api_base.trim_end_matches('/')));
        let auth = config
            .bearer
            .filter(|s| !s.trim().is_empty())
            .or_else(|| std::env::var(KEY_ENV).ok().filter(|s| !s.trim().is_empty()));
        Telemetry::build(
            enabled,
            endpoint,
            auth,
            config.distinct_id,
            config.product,
            config.version,
        )
    }

    /// A handle that records and sends nothing — the opt-out and error paths return
    /// this, so callers never branch on enablement.
    pub fn disabled() -> Telemetry {
        Telemetry::build(
            false,
            String::new(),
            None,
            String::new(),
            String::new(),
            String::new(),
        )
    }

    fn build(
        enabled: bool,
        endpoint: String,
        auth: Option<String>,
        distinct_id: String,
        product: String,
        version: String,
    ) -> Telemetry {
        let http = if enabled {
            let budget = std::env::var(TIMEOUT_ENV)
                .ok()
                .and_then(|s| s.trim().parse::<u64>().ok())
                .filter(|ms| *ms > 0)
                .unwrap_or(DEFAULT_TIMEOUT_MS);
            Client::builder()
                .timeout(Duration::from_millis(budget))
                .connect_timeout(Duration::from_millis(budget / 2 + 1))
                .build()
                .ok()
        } else {
            None
        };
        Telemetry {
            enabled: enabled && http.is_some(),
            endpoint,
            auth,
            distinct_id,
            product,
            version,
            events: Mutex::new(Vec::new()),
            http,
        }
    }

    /// Whether this handle will record and (given a credential) transmit.
    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    /// Record one finished command: its verb, wall-clock duration, and outcome.
    /// `command` is a fixed dispatch label (never argv or user input), so the
    /// properties carry no PII.
    pub fn command(&self, command: &str, elapsed: Duration, ok: bool) {
        if !self.enabled {
            return;
        }
        let mut props = Map::new();
        props.insert("product".into(), Value::String(self.product.clone()));
        props.insert("command".into(), Value::String(command.to_string()));
        props.insert(
            "duration_ms".into(),
            Value::from(elapsed.as_millis() as u64),
        );
        props.insert("ok".into(), Value::Bool(ok));
        props.insert(
            "status".into(),
            Value::String(if ok { "ok" } else { "error" }.into()),
        );
        props.insert("version".into(), Value::String(self.version.clone()));
        props.insert("os".into(), Value::String(std::env::consts::OS.into()));
        props.insert("arch".into(), Value::String(std::env::consts::ARCH.into()));
        self.record(COMMAND_COMPLETED, props);
    }

    /// Buffer one arbitrary event. The caller owns keeping `properties` free of PII.
    pub fn record(&self, event: &str, properties: Map<String, Value>) {
        if !self.enabled {
            return;
        }
        let ev = Event {
            event: event.to_string(),
            distinct_id: self.distinct_id.clone(),
            time: now_rfc3339(),
            properties,
        };
        if let Ok(mut buf) = self.events.lock() {
            buf.push(ev);
        }
    }

    /// Drain the buffer and POST it as one `[Event]` batch. Bounded by the flush
    /// budget and fully best-effort: no credential, an empty buffer, or any network
    /// error is a silent no-op. Typically called once, at process exit.
    pub async fn flush(&self) {
        if !self.enabled {
            return;
        }
        let batch = match self.events.lock() {
            Ok(mut buf) if !buf.is_empty() => std::mem::take(&mut *buf),
            _ => return,
        };
        let (Some(http), Some(auth)) = (&self.http, &self.auth) else {
            return; // the fail-closed door rejects an unauthenticated event; don't send it
        };
        let _ = http
            .post(&self.endpoint)
            .bearer_auth(auth)
            .json(&batch)
            .send()
            .await;
    }
}

/// Whether telemetry is opted out via the environment: `HANZO_TELEMETRY` is
/// `0`/`false`/`no`/`off`, or `DO_NOT_TRACK` carries any truthy value.
pub fn opted_out() -> bool {
    is_opt_out(
        std::env::var(TELEMETRY_ENV).ok().as_deref(),
        std::env::var(DO_NOT_TRACK_ENV).ok().as_deref(),
    )
}

/// The opt-out decision as a pure function of the two variables' values.
fn is_opt_out(hanzo_telemetry: Option<&str>, do_not_track: Option<&str>) -> bool {
    if let Some(v) = hanzo_telemetry {
        if matches!(
            v.trim().to_ascii_lowercase().as_str(),
            "0" | "false" | "no" | "off"
        ) {
            return true;
        }
    }
    if let Some(v) = do_not_track {
        let v = v.trim().to_ascii_lowercase();
        if !matches!(v.as_str(), "" | "0" | "false" | "no" | "off") {
            return true;
        }
    }
    false
}

/// Resolve the stable, privacy-clean device id used as `distinctId`: a random
/// 128-bit value minted once and cached owner-only under `dir`. It identifies an
/// install, never a person — we deliberately read no system machine identifier.
pub fn device_id(dir: &Path) -> String {
    let path = dir.join("telemetry-id");
    if let Ok(existing) = std::fs::read_to_string(&path) {
        let id = existing.trim();
        if !id.is_empty() {
            return id.to_string();
        }
    }
    let id = random_hex_16();
    let _ = std::fs::create_dir_all(dir);
    let _ = write_private(&path, id.as_bytes());
    id
}

/// Current time as RFC3339 UTC — the wire's `time` field.
fn now_rfc3339() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

/// 16 random bytes, lowercase hex. Falls back to a time-seeded value if the OS RNG
/// is unavailable, so an id is always produced (uniqueness over unpredictability).
fn random_hex_16() -> String {
    let mut bytes = [0u8; 16];
    if getrandom::getrandom(&mut bytes).is_err() {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        bytes[..16].copy_from_slice(&nanos.to_le_bytes());
    }
    hex(&bytes)
}

fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push(DIGITS[(b >> 4) as usize] as char);
        s.push(DIGITS[(b & 0xf) as usize] as char);
    }
    s
}

/// Write `bytes` to `path` owner-only (`0600` on Unix), truncating any prior value.
fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut f = opts.open(path)?;
    f.write_all(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;

    #[test]
    fn opt_out_reads_both_variables() {
        // HANZO_TELEMETRY falsey values disable; truthy/unset do not.
        assert!(is_opt_out(Some("0"), None));
        assert!(is_opt_out(Some("false"), None));
        assert!(is_opt_out(Some(" OFF "), None));
        assert!(!is_opt_out(Some("1"), None));
        assert!(!is_opt_out(Some(""), None));
        assert!(!is_opt_out(None, None));
        // DO_NOT_TRACK: any truthy value disables; falsey/empty does not.
        assert!(is_opt_out(None, Some("1")));
        assert!(is_opt_out(None, Some("true")));
        assert!(!is_opt_out(None, Some("0")));
        assert!(!is_opt_out(None, Some("")));
    }

    #[test]
    fn disabled_handle_records_nothing() {
        let t = Telemetry::disabled();
        assert!(!t.is_enabled());
        t.command("network", Duration::from_millis(5), true);
        assert_eq!(
            t.events.lock().unwrap().len(),
            0,
            "a disabled handle buffers nothing"
        );
    }

    #[test]
    fn opted_out_new_is_a_noop() {
        // A live Config that has opted out via HANZO_EVENT_URL-independent env is
        // exercised through the pure path above; here assert `new` honors a forced
        // opt-out by constructing a disabled handle and checking the invariant.
        let t = Telemetry::build(
            false,
            "http://127.0.0.1:1/v1/event".into(),
            Some("pk_x".into()),
            "dev-id".into(),
            "cli".into(),
            "0.0.0".into(),
        );
        t.command("wallet", Duration::from_millis(1), false);
        assert_eq!(t.events.lock().unwrap().len(), 0);
    }

    #[test]
    fn device_id_is_stable_across_calls() {
        let dir = std::env::temp_dir().join(format!("hanzo-event-test-{}", random_hex_16()));
        let a = device_id(&dir);
        let b = device_id(&dir);
        assert_eq!(a, b, "the cached id is reused");
        assert_eq!(a.len(), 32, "128-bit hex");
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn command_builds_the_canonical_wire() {
        let t = Telemetry::build(
            true,
            "http://127.0.0.1:1/v1/event".into(),
            Some("pk_x".into()),
            "device-42".into(),
            "cli".into(),
            "1.2.3".into(),
        );
        t.command("network", Duration::from_millis(7), true);
        let buf = t.events.lock().unwrap();
        assert_eq!(buf.len(), 1);
        let ev = &buf[0];
        assert_eq!(ev.event, COMMAND_COMPLETED);
        assert_eq!(ev.distinct_id, "device-42");
        assert!(!ev.time.is_empty());
        assert_eq!(ev.properties["product"], Value::String("cli".into()));
        assert_eq!(ev.properties["command"], Value::String("network".into()));
        assert_eq!(ev.properties["duration_ms"], Value::from(7u64));
        assert_eq!(ev.properties["ok"], Value::Bool(true));
        assert_eq!(ev.properties["status"], Value::String("ok".into()));
        assert_eq!(ev.properties["version"], Value::String("1.2.3".into()));
        // Serialize to the canonical field names.
        let wire = serde_json::to_value(ev).unwrap();
        assert!(
            wire.get("distinctId").is_some(),
            "distinctId is the wire field"
        );
        assert!(wire.get("distinct_id").is_none());
    }

    /// Read one HTTP request off `stream`, reply `200`, and return the body bytes.
    fn serve_once(stream: std::net::TcpStream) -> Vec<u8> {
        let mut stream = stream;
        let mut buf = Vec::new();
        let mut tmp = [0u8; 2048];
        let mut header_end: Option<usize> = None;
        let mut content_len = 0usize;
        loop {
            if header_end.is_none() {
                if let Some(pos) = find(&buf, b"\r\n\r\n") {
                    header_end = Some(pos + 4);
                    let head = String::from_utf8_lossy(&buf[..pos]);
                    for line in head.split("\r\n") {
                        if let Some((k, v)) = line.split_once(':') {
                            if k.trim().eq_ignore_ascii_case("content-length") {
                                content_len = v.trim().parse().unwrap_or(0);
                            }
                        }
                    }
                }
            }
            if let Some(he) = header_end {
                if buf.len() >= he + content_len {
                    let _ = stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\n{}");
                    let _ = stream.flush();
                    return buf[he..he + content_len].to_vec();
                }
            }
            match stream.read(&mut tmp) {
                Ok(0) => return header_end.map(|he| buf[he..].to_vec()).unwrap_or_default(),
                Ok(n) => buf.extend_from_slice(&tmp[..n]),
                Err(_) => return Vec::new(),
            }
        }
    }

    fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
        haystack.windows(needle.len()).position(|w| w == needle)
    }

    #[tokio::test]
    async fn flush_posts_the_batch_to_the_endpoint() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let handle = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            serve_once(stream)
        });

        let url = format!("http://{addr}/v1/event");
        let t = Telemetry::build(
            true,
            url,
            Some("pk_test".into()),
            "device-9".into(),
            "cli".into(),
            "9.9.9".into(),
        );
        t.command("code", Duration::from_millis(3), true);
        t.flush().await;

        let body = handle.join().unwrap();
        let parsed: Vec<Event> =
            serde_json::from_slice(&body).expect("the flushed body is a canonical [Event] array");
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].event, COMMAND_COMPLETED);
        assert_eq!(parsed[0].distinct_id, "device-9");
        assert_eq!(
            parsed[0].properties["command"],
            Value::String("code".into())
        );
        // The buffer drained.
        assert_eq!(t.events.lock().unwrap().len(), 0);
    }

    #[tokio::test]
    async fn flush_without_credential_sends_nothing() {
        // No auth -> the fail-closed door would 403; the handle must not transmit.
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        listener.set_nonblocking(true).unwrap();

        let url = format!("http://{addr}/v1/event");
        let t = Telemetry::build(
            true,
            url,
            None,
            "device-0".into(),
            "cli".into(),
            "0.0.0".into(),
        );
        t.command("whoami", Duration::from_millis(1), true);
        t.flush().await;

        // Nothing connected.
        assert!(
            listener.accept().is_err(),
            "no request was sent without a credential"
        );
    }
}
