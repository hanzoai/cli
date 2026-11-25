//! Integration tests covering the four user-reported flows in hanzoai/cli#65
//! (from hanzo-inc/platform#14):
//!   1. `hanzo models list` output formats (`table`, `json`, `yaml`) & anonymous access
//!   2. `hanzo chat completions` typed flags and `--data` refusal text
//!   3. `hanzo-mcp` / `hanzo __mcp` cold-cache stdio `initialize` within client timeout
//!   4. Isolated `hanzo auth` login (stdin token) → show → token → logout flow
//!
//! Runs hermetically with zero writes to the developer's HOME and no external
//! network dependency (uses a local loopback mock HTTP server).

#![cfg(unix)]

use assert_cmd::Command;
use predicates::prelude::*;
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

const HANZO: &str = env!("CARGO_BIN_EXE_hanzo");
static HOME_SEQ: AtomicUsize = AtomicUsize::new(0);

struct IsolatedHome {
    root: PathBuf,
}

impl IsolatedHome {
    fn new(prefix: &str) -> Self {
        // Keep path short so Unix domain sockets (`hanzod.sock`) stay below macOS SUN_LEN (104B).
        let seq = HOME_SEQ.fetch_add(1, Ordering::Relaxed);
        let root = PathBuf::from(format!("/tmp/hz-{prefix}-{}-{seq}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        Self { root }
    }

    fn path(&self) -> &Path {
        &self.root
    }

    fn config_arg(&self) -> String {
        self.root.join("config.toml").display().to_string()
    }
}

impl Drop for IsolatedHome {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn hanzo_in(home: &IsolatedHome) -> Command {
    let mut cmd = Command::new(HANZO);
    cmd.env("HOME", home.path())
        .env("NO_COLOR", "1")
        .env("HANZO_NO_ANIMATION", "1")
        .env_remove("HANZO_API_KEY")
        .env_remove("HANZO_TOKEN")
        .env_remove("XDG_CONFIG_HOME")
        .env_remove("XDG_DATA_HOME")
        .env_remove("XDG_CACHE_HOME")
        .env_remove("XDG_STATE_HOME")
        .timeout(Duration::from_secs(20));
    cmd
}

#[derive(Debug, Clone)]
struct CapturedRequest {
    method: String,
    path: String,
    auth: Option<String>,
    body: String,
}

/// Minimal HTTP/1.1 loopback server so tests prove real CLI wire serialization
/// without touching production or adding dev-deps.
/// Returns `(api_url, iam_url, captured)`:
/// - `api_url` uses `0.0.0.0:<port>` so `commands::host::origin` sends direct HTTP
///   to our loopback listener rather than auto-spawning `hanzod` over ZAP.
/// - `iam_url` uses `127.0.0.1:<port>` so IAM origin validation accepts plain HTTP.
fn spawn_mock_api(
    responder: impl Fn(&CapturedRequest) -> (u16, String) + Send + 'static,
) -> (String, String, Arc<Mutex<Vec<CapturedRequest>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
    let addr = listener.local_addr().unwrap();
    let captured = Arc::new(Mutex::new(Vec::new()));
    let cap_clone = Arc::clone(&captured);

    thread::spawn(move || {
        while let Ok((mut stream, _)) = listener.accept() {
            let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut request_line = String::new();
            if reader.read_line(&mut request_line).unwrap_or(0) == 0 {
                continue;
            }
            let parts: Vec<&str> = request_line.split_whitespace().collect();
            let method = parts.first().copied().unwrap_or("").to_string();
            let path = parts.get(1).copied().unwrap_or("").to_string();

            let mut content_len: usize = 0;
            let mut auth: Option<String> = None;
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap_or(0) == 0 {
                    break;
                }
                let trimmed = line.trim_end_matches(['\r', '\n']);
                if trimmed.is_empty() {
                    break;
                }
                if let Some((k, v)) = trimmed.split_once(':') {
                    if k.eq_ignore_ascii_case("content-length") {
                        content_len = v.trim().parse().unwrap_or(0);
                    } else if k.eq_ignore_ascii_case("authorization") {
                        auth = Some(v.trim().to_string());
                    }
                }
            }
            let mut body_bytes = vec![0u8; content_len];
            if content_len > 0 {
                let _ = reader.read_exact(&mut body_bytes);
            }
            let req = CapturedRequest {
                method,
                path,
                auth,
                body: String::from_utf8_lossy(&body_bytes).to_string(),
            };
            let (status, resp_body) = responder(&req);
            cap_clone.lock().unwrap().push(req);
            let reason = match status {
                200 => "OK",
                400 => "Bad Request",
                401 => "Unauthorized",
                _ => "Error",
            };
            let resp = format!(
                "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{resp_body}",
                resp_body.len()
            );
            let _ = stream.write_all(resp.as_bytes());
            let _ = stream.flush();
        }
    });

    (
        format!("http://0.0.0.0:{}", addr.port()),
        format!("http://127.0.0.1:{}", addr.port()),
        captured,
    )
}

fn b64url(data: &[u8]) -> String {
    const T: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::new();
    let mut i = 0;
    while i + 3 <= data.len() {
        let n = ((data[i] as u32) << 16) | ((data[i + 1] as u32) << 8) | data[i + 2] as u32;
        out.push(T[((n >> 18) & 63) as usize] as char);
        out.push(T[((n >> 12) & 63) as usize] as char);
        out.push(T[((n >> 6) & 63) as usize] as char);
        out.push(T[(n & 63) as usize] as char);
        i += 3;
    }
    let rest = &data[i..];
    if rest.len() == 1 {
        let n = (rest[0] as u32) << 16;
        out.push(T[((n >> 18) & 63) as usize] as char);
        out.push(T[((n >> 12) & 63) as usize] as char);
    } else if rest.len() == 2 {
        let n = ((rest[0] as u32) << 16) | ((rest[1] as u32) << 8);
        out.push(T[((n >> 18) & 63) as usize] as char);
        out.push(T[((n >> 12) & 63) as usize] as char);
        out.push(T[((n >> 6) & 63) as usize] as char);
    }
    out
}

fn fake_jwt(iss: &str, owner: &str, name: &str) -> String {
    let claims = format!(
        r#"{{"iss":"{iss}","owner":"{owner}","name":"{name}","sub":"u-feedback","azp":"{owner}-cli","email":"{name}@example.com"}}"#
    );
    format!(
        "{}.{}.c2ln",
        b64url(br#"{"alg":"none","typ":"JWT"}"#),
        b64url(claims.as_bytes())
    )
}

fn use_mock_network(home: &IsolatedHome, api_url: &str, iam_url: &str) {
    hanzo_in(home)
        .args([
            "network",
            "add",
            "mock",
            "--network-id",
            "1337",
            "--chain-id",
            "1337",
            "--rpc",
            "http://127.0.0.1:1",
            "--api",
            api_url,
            "--iam",
            iam_url,
            "--config",
            &home.config_arg(),
        ])
        .assert()
        .success();
    hanzo_in(home)
        .args(["network", "use", "mock", "--config", &home.config_arg()])
        .assert()
        .success();
}

#[test]
fn models_list_formats_table_json_and_yaml_anonymously() {
    let home = IsolatedHome::new("ml");
    let catalog = json!({
        "object": "list",
        "data": [
            { "id": "zen6-flash", "object": "model", "owned_by": "hanzo" },
            { "id": "enso-auto", "object": "model", "owned_by": "hanzo" }
        ]
    })
    .to_string();

    let (api_url, iam_url, captured) = spawn_mock_api(move |req| {
        if req.method == "GET" && req.path == "/v1/models" {
            (200, catalog.clone())
        } else {
            (404, r#"{"error":"not found"}"#.to_string())
        }
    });
    use_mock_network(&home, &api_url, &iam_url);

    // 1. JSON output (works anonymously when signed out)
    let json_out = hanzo_in(&home)
        .args([
            "models",
            "list",
            "-o",
            "json",
            "--config",
            &home.config_arg(),
        ])
        .assert()
        .success();
    let parsed: Value = serde_json::from_slice(&json_out.get_output().stdout)
        .expect("hanzo models list -o json must output valid JSON");
    let items = parsed["data"]
        .as_array()
        .or_else(|| parsed.as_array())
        .unwrap_or_else(|| panic!("expected array or .data array in models list JSON: {parsed}"));
    assert_eq!(items.len(), 2, "unexpected models list JSON: {parsed}");
    assert_eq!(items[0]["id"], "zen6-flash");
    assert_eq!(items[1]["id"], "enso-auto");

    // 2. Table output (human-readable rows containing model IDs)
    hanzo_in(&home)
        .args([
            "models",
            "list",
            "-o",
            "table",
            "--config",
            &home.config_arg(),
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("zen6-flash").and(predicate::str::contains("enso-auto")));

    // 3. YAML output
    hanzo_in(&home)
        .args([
            "models",
            "list",
            "-o",
            "yaml",
            "--config",
            &home.config_arg(),
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("zen6-flash").and(predicate::str::contains("enso-auto")));

    let reqs = captured.lock().unwrap();
    let model_reqs: Vec<_> = reqs.iter().filter(|r| r.path == "/v1/models").collect();
    assert_eq!(model_reqs.len(), 3);
    for r in model_reqs {
        assert!(
            r.auth.is_none(),
            "signed-out public models list must not send Authorization header"
        );
    }
}

#[test]
fn chat_completions_typed_flags_and_data_refusal() {
    let home = IsolatedHome::new("cf");

    // 1. Passing `--data` to a typed operation must fail with a clear refusal naming its flags.
    hanzo_in(&home)
        .args([
            "chat",
            "completions",
            "--data",
            r#"{"model":"zen6-flash"}"#,
            "--config",
            &home.config_arg(),
        ])
        .assert()
        .failure()
        .stderr(
            predicate::str::contains("takes its body as flags, not --data")
                .and(predicate::str::contains("--model"))
                .and(predicate::str::contains("--messages")),
        );

    // 2. Typed flags assemble a properly typed JSON body (`max_tokens` as integer, `messages` as array).
    let reply = json!({
        "id": "chatcmpl-test",
        "object": "chat.completion",
        "model": "zen6-flash",
        "choices": [{
            "index": 0,
            "message": { "role": "assistant", "content": "OK" },
            "finish_reason": "stop"
        }]
    })
    .to_string();

    let (api_url, iam_url, captured) = spawn_mock_api(move |req| {
        if req.method == "POST" && req.path == "/v1/chat/completions" {
            (200, reply.clone())
        } else {
            (404, r#"{"error":"not found"}"#.to_string())
        }
    });
    use_mock_network(&home, &api_url, &iam_url);

    let out = hanzo_in(&home)
        .env("HANZO_API_KEY", "hk-test-key")
        .args([
            "chat",
            "completions",
            "--model",
            "zen6-flash",
            "--messages",
            r#"{"role":"user","content":"Reply OK"}"#,
            "--max-tokens",
            "16",
            "-o",
            "json",
            "--config",
            &home.config_arg(),
        ])
        .assert()
        .success();

    let resp_json: Value =
        serde_json::from_slice(&out.get_output().stdout).expect("valid chat completion JSON");
    assert_eq!(resp_json["choices"][0]["message"]["content"], "OK");

    let reqs = captured.lock().unwrap();
    let comp_reqs: Vec<_> = reqs
        .iter()
        .filter(|r| r.path == "/v1/chat/completions")
        .collect();
    assert_eq!(comp_reqs.len(), 1);
    assert_eq!(comp_reqs[0].auth.as_deref(), Some("Bearer hk-test-key"));
    let sent: Value = serde_json::from_str(&comp_reqs[0].body).expect("sent JSON body");
    assert_eq!(sent["model"], "zen6-flash");
    assert_eq!(sent["max_tokens"], json!(16));
    assert_eq!(sent["messages"][0]["role"], "user");
    assert_eq!(sent["messages"][0]["content"], "Reply OK");
}

#[test]
fn auth_flow_stdin_login_show_token_and_logout_isolated() {
    let home = IsolatedHome::new("af");
    let userinfo = json!({
        "sub": "u-feedback",
        "owner": "acme",
        "name": "alice",
        "email": "alice@example.com"
    })
    .to_string();
    let (api_url, iam_url, captured) = spawn_mock_api(move |req| {
        if req.method == "GET" && req.path.contains("userinfo") {
            (200, userinfo.clone())
        } else {
            (200, r#"{"status":"ok"}"#.to_string())
        }
    });
    use_mock_network(&home, &api_url, &iam_url);
    let token = fake_jwt(&iam_url, "acme", "alice");

    // 1. Login via stdin `--token -`
    hanzo_in(&home)
        .args([
            "auth",
            "login",
            "--token",
            "-",
            "--config",
            &home.config_arg(),
        ])
        .write_stdin(format!("{token}\n"))
        .assert()
        .success();

    // 2. `auth show` reflects the signed-in identity
    hanzo_in(&home)
        .args(["auth", "show", "--config", &home.config_arg()])
        .assert()
        .success()
        .stdout(predicate::str::contains("acme/alice"));

    // 3. `auth token` prints the stored bearer verbatim
    let out = hanzo_in(&home)
        .args(["auth", "token", "--config", &home.config_arg()])
        .assert()
        .success();
    assert_eq!(
        String::from_utf8_lossy(&out.get_output().stdout).trim(),
        token
    );

    // 4. `auth logout` clears credentials cleanly
    hanzo_in(&home)
        .args(["auth", "logout", "--config", &home.config_arg()])
        .assert()
        .success();

    hanzo_in(&home)
        .args(["auth", "token", "--config", &home.config_arg()])
        .assert()
        .failure();

    let reqs = captured.lock().unwrap();
    assert!(
        reqs.iter().any(|r| r.path.contains("userinfo")),
        "expected userinfo request, got: {reqs:?}"
    );
}

/// Exercises the MCP JSON-RPC `initialize` handshake over stdio with a completely
/// empty HOME and cold XDG / npm / uv caches, asserting a response within timeout.
#[test]
fn mcp_cold_cache_initialize_within_timeout() {
    let home = IsolatedHome::new("mcp-cold");
    let mcp_bin = match which::which("hanzo-mcp") {
        Ok(p) => p,
        Err(_) => {
            eprintln!("skipping hanzo-mcp cold-cache initialize test: hanzo-mcp not on PATH");
            return;
        }
    };

    let start = Instant::now();
    let mut child = std::process::Command::new(&mcp_bin)
        .env("HOME", home.path())
        .env("XDG_CACHE_HOME", home.path().join("cache"))
        .env("XDG_CONFIG_HOME", home.path().join("config"))
        .env("XDG_DATA_HOME", home.path().join("data"))
        .env("NPM_CONFIG_CACHE", home.path().join("npm-cache"))
        .env("UV_CACHE_DIR", home.path().join("uv-cache"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn hanzo-mcp");

    let req = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "protocolVersion": "2024-11-05",
            "capabilities": {},
            "clientInfo": { "name": "hanzo-cli-e2e", "version": "1.0.0" }
        }
    });
    {
        let stdin = child.stdin.as_mut().expect("stdin");
        writeln!(stdin, "{req}").expect("write initialize");
        stdin.flush().expect("flush initialize");
    }

    let stdout = child.stdout.take().expect("stdout");
    let (tx, rx) = std::sync::mpsc::channel();
    thread::spawn(move || {
        let mut reader = BufReader::new(stdout);
        let mut line = String::new();
        let _ = reader.read_line(&mut line);
        let _ = tx.send(line);
    });

    let line = rx
        .recv_timeout(Duration::from_secs(10))
        .expect("hanzo-mcp did not answer initialize within 10s on a cold cache");
    let elapsed = start.elapsed();
    let _ = child.kill();
    let _ = child.wait();

    let resp: Value = serde_json::from_str(line.trim()).expect("valid JSON-RPC initialize reply");
    assert_eq!(resp["jsonrpc"], "2.0");
    assert_eq!(resp["id"], 1);
    assert!(
        resp["result"]["protocolVersion"]
            .as_str()
            .is_some_and(|s| !s.is_empty()),
        "missing protocolVersion in {resp}"
    );
    assert!(
        resp["result"]["serverInfo"]["name"]
            .as_str()
            .is_some_and(|s| !s.is_empty()),
        "missing serverInfo.name in {resp}"
    );
    assert!(
        elapsed < Duration::from_secs(10),
        "cold-cache initialize took {elapsed:?}, exceeding 10s timeout"
    );
}
