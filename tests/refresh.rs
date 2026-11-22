//! Several `hanzo` processes that need a refresh at the same moment, against the
//! shipped binary and a stand-in IAM that rotates single-use refresh tokens and
//! revokes the family on a replay, as hanzo.id does. Config and credentials sit
//! under a throwaway `HOME`.

#![cfg(unix)]

use base64::Engine;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const HANZO: &str = env!("CARGO_BIN_EXE_hanzo");
const PROCESSES: usize = 8;

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs()
}

/// A token as the stand-in IAM mints it: its own issuer, the principal, the
/// client it was issued to, and when it runs out.
fn jwt(iss: &str, exp: u64, serial: usize) -> String {
    let b64 = |b: &[u8]| base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(b);
    let claims = format!(
        r#"{{"iss":"{iss}","owner":"hanzo","name":"z","sub":"u-1","azp":"hanzo-cli","exp":{exp},"jti":"{serial}"}}"#
    );
    format!("{}.{}.c2ln", b64(br#"{"alg":"none","typ":"JWT"}"#), b64(claims.as_bytes()))
}

#[derive(Default)]
struct Iam {
    /// The refresh token IAM will accept next.
    current: String,
    served: usize,
    replays: usize,
}

/// The stand-in IAM: answers every refresh slowly, so a second process that has
/// not waited for the first is in flight with it.
fn serve(listener: TcpListener, origin: String, iam: Arc<Mutex<Iam>>) {
    for stream in listener.incoming() {
        let Ok(mut stream) = stream else { break };
        let (origin, iam) = (origin.clone(), Arc::clone(&iam));
        std::thread::spawn(move || {
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut length = 0;
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
                    break;
                }
                if let Some(v) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                    length = v.trim().parse().unwrap_or(0);
                }
            }
            let mut body = vec![0; length];
            let _ = reader.read_exact(&mut body);
            let form = String::from_utf8_lossy(&body).into_owned();
            let Some(presented) = form.split('&').find_map(|p| p.strip_prefix("refresh_token=")) else {
                // Anything else a command asks of IAM is not a refresh.
                let _ = write!(stream, "HTTP/1.1 404 Not Found\r\ncontent-length: 0\r\nconnection: close\r\n\r\n");
                return;
            };
            let presented = presented.to_string();
            std::thread::sleep(Duration::from_millis(500));
            let answer = {
                let mut iam = iam.lock().unwrap();
                if presented == iam.current {
                    iam.served += 1;
                    iam.current = format!("rt-{}", iam.served);
                    let next = (jwt(&origin, now() + 3600, iam.served), iam.current.clone());
                    Some(next)
                } else {
                    iam.replays += 1;
                    None
                }
            };
            let (status, payload) = match answer {
                Some((access, refresh)) => (
                    "200 OK",
                    format!(r#"{{"access_token":"{access}","token_type":"Bearer","refresh_token":"{refresh}"}}"#),
                ),
                None => ("400 Bad Request", r#"{"error":"invalid_grant"}"#.to_string()),
            };
            let _ = write!(
                stream,
                "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{payload}",
                payload.len()
            );
        });
    }
}

struct Home(PathBuf);

impl Home {
    /// A throwaway HOME, taken by this process as well as by every `hanzo` it
    /// starts. Call it before any thread exists: it sets this process's
    /// environment.
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!("hanzo-refresh-{}-{}", std::process::id(), now()));
        std::fs::create_dir_all(&root).unwrap();
        std::env::set_var("HOME", &root);
        std::env::remove_var("XDG_CONFIG_HOME");
        std::env::remove_var("XDG_DATA_HOME");
        Self(root)
    }

    fn path(&self) -> &Path {
        &self.0
    }

    /// The credential store the binary under test writes, resolved by the
    /// product's own resolver against the same environment the binary sees. The
    /// path is the platform's data dir, so it is not spelled out here:
    /// `~/.local/share` on Linux and `~/Library/Application Support` on macOS
    /// (hanzoai/cli#70).
    fn credentials(&self) -> PathBuf {
        let store = hanzo_identity::token::FileVault::resolve().unwrap();
        assert!(store.path().starts_with(self.path()), "the store resolved outside the throwaway HOME: {}", store.path().display());
        store.path().to_path_buf()
    }

    fn hanzo(&self) -> std::process::Command {
        let mut cmd = std::process::Command::new(HANZO);
        cmd.env("HOME", self.path())
            .env_remove("XDG_CONFIG_HOME")
            .env_remove("XDG_DATA_HOME")
            .env("NO_COLOR", "1")
            .env("HANZO_NO_ANIMATION", "1");
        cmd
    }
}

impl Drop for Home {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// N processes whose access token has run out all ask for one: IAM is asked once,
/// and every one of them gets the fresh credential.
#[test]
fn processes_that_need_a_refresh_at_once_spend_the_refresh_token_once() {
    let home = Home::new();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let iam = Arc::new(Mutex::new(Iam { current: "rt-0".into(), ..Iam::default() }));
    {
        let (origin, iam) = (origin.clone(), Arc::clone(&iam));
        std::thread::spawn(move || serve(listener, origin, iam));
    }

    let run = |args: &[&str], stdin: Option<&str>| {
        let mut cmd = home.hanzo();
        cmd.args(args).stdin(std::process::Stdio::piped()).stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped());
        let mut child = cmd.spawn().unwrap();
        if let Some(s) = stdin {
            child.stdin.take().unwrap().write_all(s.as_bytes()).unwrap();
        }
        child.wait_with_output().unwrap()
    };
    let name = "refresh-race";
    let added = run(&["network", "add", name, "--network-id", "1337", "--chain-id", "1337", "--rpc", "http://127.0.0.1:1", "--api", "http://127.0.0.1:1", "--iam", &origin], None);
    assert!(added.status.success(), "{}", String::from_utf8_lossy(&added.stderr));
    assert!(run(&["network", "use", name], None).status.success());

    // Signed in with a token that has already run out, then given the refresh token
    // a sign-in through the browser leaves beside it.
    let stale = jwt(&origin, now() - 3600, 0);
    let signed = run(&["auth", "login", "--provider", "hanzo", "--token", "-"], Some(&format!("{stale}\n")));
    assert!(signed.status.success(), "{}", String::from_utf8_lossy(&signed.stderr));
    let credentials = home.credentials();
    let mut map: serde_json::Map<String, serde_json::Value> = serde_json::from_slice(&std::fs::read(&credentials).unwrap()).unwrap();
    for value in map.values_mut() {
        let mut held: serde_json::Value = serde_json::from_str(value.as_str().unwrap()).unwrap();
        held["refresh_token"] = "rt-0".into();
        *value = serde_json::Value::String(held.to_string());
    }
    std::fs::write(&credentials, serde_json::to_vec(&map).unwrap()).unwrap();

    let outputs: Vec<_> = std::thread::scope(|s| {
        let spawned: Vec<_> = (0..PROCESSES).map(|_| s.spawn(|| run(&["auth", "token"], None))).collect();
        spawned.into_iter().map(|h| h.join().unwrap()).collect()
    });

    let iam = iam.lock().unwrap();
    assert_eq!((iam.served, iam.replays), (1, 0), "IAM was asked {} times and saw {} replays", iam.served, iam.replays);
    let mut tokens = std::collections::BTreeSet::new();
    for out in &outputs {
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        tokens.insert(String::from_utf8_lossy(&out.stdout).trim().to_string());
    }
    assert_eq!(tokens.len(), 1, "the processes were handed {} different credentials", tokens.len());
    let payload = tokens.iter().next().unwrap().split('.').nth(1).unwrap().to_string();
    let claims = base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(payload).unwrap();
    assert!(String::from_utf8_lossy(&claims).contains(r#""jti":"1""#), "not the refreshed credential");
}
