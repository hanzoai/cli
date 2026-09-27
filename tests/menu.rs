//! The signed-in account menu, against the shipped `hanzo` binary.
//!
//! `hanzo login` hands its loopback to a second copy of this binary and returns.
//! These tests are that second copy: a listening socket, the three handoff
//! variables, and HTTP. Config and credentials sit under a throwaway `HOME`,
//! so a click never touches the machine this suite is running on. The IAM
//! origin is a local stand-in; nothing here dials hanzo.id except the seeding
//! logins, which give up on userinfo after its own two seconds.

#![cfg(unix)]

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::Duration;

const HANZO: &str = env!("CARGO_BIN_EXE_hanzo");

struct Home {
    root: PathBuf,
}

impl Home {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "hanzo-menu-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&root).unwrap();
        Self { root }
    }

    fn path(&self) -> &Path {
        &self.root
    }
}

impl Drop for Home {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn hanzo(home: &Home) -> assert_cmd::Command {
    let mut cmd = assert_cmd::Command::new(HANZO);
    cmd.env("HOME", home.path());
    cmd.env_remove("XDG_CONFIG_HOME");
    cmd.env_remove("XDG_DATA_HOME");
    cmd.env("NO_COLOR", "1");
    cmd.env("HANZO_NO_ANIMATION", "1");
    cmd.timeout(Duration::from_secs(20));
    cmd
}

fn jwt(owner: &str, name: &str, email: Option<&str>) -> String {
    let email = email
        .map(|e| format!(r#","email":"{e}""#))
        .unwrap_or_default();
    let claims = format!(r#"{{"owner":"{owner}","name":"{name}","sub":"u-1"{email}}}"#);
    format!(
        "{}.{}.c2ln",
        b64url(br#"{"alg":"none","typ":"JWT"}"#),
        b64url(claims.as_bytes())
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

fn sign_in(home: &Home, token: &str) {
    hanzo(home)
        .args(["auth", "login", "--provider", "hanzo", "--token", "-"])
        .write_stdin(format!("{token}\n"))
        .assert()
        .success()
        .stdout(predicates::str::contains("Signed in"));
}

fn listed(home: &Home) -> String {
    let out = hanzo(home).args(["auth", "list"]).output().unwrap();
    assert!(
        out.status.success(),
        "auth list failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap()
}

/// A local IAM that only has to mint one token and accept one revocation.
struct Iam {
    origin: String,
    seen: Arc<Mutex<Vec<String>>>,
}

impl Iam {
    fn start(extra_token: String) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let seen = Arc::new(Mutex::new(Vec::new()));
        let record = Arc::clone(&seen);
        std::thread::spawn(move || {
            for conn in listener.incoming() {
                let Ok(mut stream) = conn else { continue };
                let req = read_one(&mut stream);
                record.lock().unwrap().push(req.clone());
                let (status, body) = if req.contains("code=deny") {
                    ("400 Bad Request", r#"{"error":"invalid_grant"}"#.to_string())
                } else if req.contains("/revoke") {
                    ("200 OK", String::new())
                } else {
                    (
                        "200 OK",
                        format!(
                            r#"{{"access_token":"{extra_token}","token_type":"Bearer","refresh_token":"rt-added"}}"#
                        ),
                    )
                };
                let response = format!(
                    "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = stream.write_all(response.as_bytes());
            }
        });
        Self { origin, seen }
    }

    fn saw(&self, needle: &str) -> bool {
        self.seen.lock().unwrap().iter().any(|req| req.contains(needle))
    }
}

fn read_one(stream: &mut TcpStream) -> String {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
    let mut buf = Vec::new();
    let mut tmp = [0u8; 2048];
    loop {
        match stream.read(&mut tmp) {
            Ok(0) => break,
            Ok(n) => {
                buf.extend_from_slice(&tmp[..n]);
                if let Some(end) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                    let headers = String::from_utf8_lossy(&buf[..end]);
                    let len = headers
                        .lines()
                        .find_map(|line| {
                            let (name, value) = line.split_once(':')?;
                            (name.eq_ignore_ascii_case("content-length"))
                                .then(|| value.trim().parse::<usize>().ok())?
                        })
                        .unwrap_or(0);
                    if buf.len() >= end + 4 + len {
                        break;
                    }
                }
            }
            Err(_) => break,
        }
    }
    String::from_utf8_lossy(&buf).into_owned()
}

struct Menu {
    child: Child,
    addr: SocketAddr,
    log: PathBuf,
}

impl Menu {
    fn start(home: &Home, origin: &str) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let fd = {
            use std::os::fd::AsRawFd;
            let fd = listener.as_raw_fd();
            unsafe {
                let flags = libc::fcntl(fd, libc::F_GETFD);
                assert!(flags >= 0, "F_GETFD");
                assert_eq!(
                    libc::fcntl(fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC),
                    0,
                    "clearing cloexec"
                );
            }
            fd
        };
        let log = home.path().join("menu.log");
        let log_file = std::fs::File::create(&log).unwrap();
        let child = Command::new(HANZO)
            .env("HOME", home.path())
            .env_remove("XDG_CONFIG_HOME")
            .env_remove("XDG_DATA_HOME")
            .env("NO_COLOR", "1")
            .env("HANZO_LOGIN_FD", fd.to_string())
            .env("HANZO_LOGIN_ORIGIN", origin)
            .env("HANZO_LOGIN_BRAND", "hanzo")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::from(log_file))
            .spawn()
            .unwrap();
        // The child inherited the socket at exec. Closing this side leaves
        // the child's descriptor as the only one still listening.
        drop(listener);
        Self { child, addr, log }
    }

    fn ask(&mut self, target: &str) -> String {
        let mut stream = TcpStream::connect(self.addr).unwrap_or_else(|err| {
            panic!("menu port: {err}\n{}", self.log_text());
        });
        let _ = stream.set_read_timeout(Some(Duration::from_secs(8)));
        let req = format!("GET {target} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n");
        stream.write_all(req.as_bytes()).unwrap();
        let mut buf = Vec::new();
        let _ = stream.read_to_end(&mut buf);
        let body = String::from_utf8_lossy(&buf).into_owned();
        assert!(!body.is_empty(), "no reply for {target}\n{}", self.log_text());
        if let Some(status) = self.child.try_wait().unwrap() {
            panic!("menu exited {status} during {target}\n{}", self.log_text());
        }
        body
    }

    fn log_text(&self) -> String {
        std::fs::read_to_string(&self.log).unwrap_or_default()
    }
}

impl Drop for Menu {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn attr_path<'a>(page: &'a str, path: &str) -> &'a str {
    let at = page.find(path).unwrap_or_else(|| panic!("no {path} in the page"));
    page[at..].split('"').next().unwrap()
}

/// Add, confirm-remove, and switch, through the loopback the login child holds.
#[test]
fn the_signed_in_menu_adds_removes_and_switches_over_http() {
    let home = Home::new();
    sign_in(&home, &jwt("hanzo", "z", Some("z@hanzo.ai")));
    sign_in(&home, &jwt("lux", "a", Some("a@lux.id")));
    sign_in(&home, &jwt("hanzo", "Zach Kelling", None));

    let extra = jwt("hanzo", "extra", Some("extra@hanzo.ai"));
    let iam = Iam::start(extra);
    let mut menu = Menu::start(&home, &iam.origin);

    // A request that never finishes its headers must not hold the port.
    {
        let mut hung = TcpStream::connect(menu.addr).unwrap();
        hung.write_all(b"GET /callback HTTP/1.1\r\nHost: 127.0.0.1\r\n").unwrap();
        std::thread::sleep(Duration::from_millis(500));
    }

    let icon = menu.ask("/favicon.ico");
    assert!(icon.starts_with("HTTP/1.1 204"), "{icon}");

    let page = menu.ask("/callback");
    assert!(page.starts_with("HTTP/1.1 200"), "{page}");
    assert!(page.contains("You're signed in"), "{page}");
    assert!(page.contains("z@hanzo.ai"), "{page}");
    assert!(page.contains("a@lux.id"), "{page}");
    assert!(page.contains("Zach Kelling"), "{page}");
    assert!(page.contains(">hanzo</div>"), "{page}");
    assert!(page.contains(">lux</div>"), "{page}");
    assert!(page.contains(">Add account<"), "{page}");
    assert!(!page.contains("Personal"), "{page}");
    assert!(!page.contains("Organizations"), "{page}");
    // The origin here is the local stand-in, so the corner names that host.
    // hanzo.id's "Hanzo AI" and the block-H are pinned by the page test.
    assert!(page.contains("127 ID"), "{page}");
    assert!(!page.contains("href=\"https://hanzo.ai\""), "{page}");

    let stray = menu.ask("/switch?x=1");
    assert!(stray.contains("You're signed in"), "{stray}");
    let stray = menu.ask("/remove?yes=1");
    assert!(stray.contains("You're signed in"), "{stray}");
    assert!(listed(&home).contains("lux/a"), "a query with no id removed an account");

    let switch_lux = attr_path(&page, "/switch?id=lux%2Fa");
    assert_eq!(switch_lux, "/switch?id=lux%2Fa");
    let switched = menu.ask(switch_lux);
    assert!(switched.contains("You're signed in"), "{switched}");
    let list = listed(&home);
    assert!(list.contains("* lux/a"), "active did not move:\n{list}");

    let confirm = menu.ask("/remove?id=lux%2Fa");
    assert!(confirm.contains("Yes, remove"), "{confirm}");
    assert!(confirm.contains("/remove?id=lux%2Fa&amp;yes=1"), "{confirm}");
    assert!(listed(&home).contains("lux/a"), "confirm removed the account");

    let removed = menu.ask("/remove?id=lux%2Fa&yes=1");
    assert!(removed.contains("You're signed in"), "{removed}");
    assert!(!removed.contains("a@lux.id"), "{removed}");
    let list = listed(&home);
    assert!(!list.contains("lux/a"), "{list}");
    assert!(list.contains("* hanzo/Zach Kelling"), "no slide onto the next account:\n{list}");

    let add = menu.ask("/add");
    assert!(add.starts_with("HTTP/1.1 303"), "{add}");
    let location = add
        .lines()
        .find_map(|line| line.strip_prefix("Location: "))
        .unwrap_or_else(|| panic!("no Location in {add}"))
        .trim();
    assert!(location.contains("prompt=select_account"), "{location}");
    assert!(location.contains("client_id=hanzo-cli"), "{location}");
    assert!(location.contains("/v1/iam/oauth/authorize"), "{location}");
    let state = location
        .split("state=")
        .nth(1)
        .unwrap()
        .split('&')
        .next()
        .unwrap()
        .to_string();

    let mismatch = menu.ask("/callback?code=fresh&state=not-the-one");
    assert!(mismatch.contains("You're signed in"), "{mismatch}");
    assert!(!iam.saw("code=fresh"), "a mismatched state was exchanged");
    assert!(!listed(&home).contains("extra"), "a mismatched state was stored");

    let refused = menu.ask(&format!("/callback?code=deny&state={state}"));
    assert!(refused.starts_with("HTTP/1.1 200"), "{refused}");
    assert!(iam.saw("code=deny"), "the refused code never reached IAM");
    assert!(!listed(&home).contains("hanzo/extra"), "a refused code was stored");

    let again = menu.ask("/add");
    let location = again
        .lines()
        .find_map(|line| line.strip_prefix("Location: "))
        .unwrap()
        .trim();
    let state = state_from(location);
    let added = menu.ask(&format!("/callback?code=fresh&state={state}"));
    assert!(added.contains("extra@hanzo.ai"), "{added}");
    assert!(iam.saw("grant_type=authorization_code"), "the code was not exchanged");
    assert!(iam.saw("client_id=hanzo-cli"));
    assert!(iam.saw("code=fresh"));
    let list = listed(&home);
    assert!(list.contains("* hanzo/extra"), "the added account is not active:\n{list}");

    let confirm = menu.ask("/remove?id=hanzo%2Fextra");
    assert!(confirm.contains("Yes, remove"), "{confirm}");
    let after = menu.ask("/remove?id=hanzo%2Fextra&yes=1");
    assert!(!after.contains("extra@hanzo.ai"), "{after}");
    assert!(iam.saw("token=rt-added"), "removing the account did not revoke its refresh token");
    assert!(!listed(&home).contains("hanzo/extra"));

    menu.ask("/remove?id=hanzo%2Fz&yes=1");
    menu.ask("/remove?id=hanzo%2FZach%20Kelling&yes=1");
    let empty = menu.ask("/callback");
    assert!(empty.contains("Signed out"), "{empty}");
    assert!(empty.contains("Add an account to keep going."), "{empty}");
    assert!(empty.contains("href=\"/add\""), "{empty}");
    let signed_out = hanzo(&home).args(["auth", "list"]).output().unwrap();
    assert!(!signed_out.status.success());
    let err = String::from_utf8_lossy(&signed_out.stderr);
    assert!(err.contains("not signed in"), "{err}");
}

/// `/add` only redirects when the authorize URL can be built. A bad origin
/// stays on the loopback as an empty success, and the page is still served.
#[test]
fn an_add_with_no_authorize_url_is_not_a_redirect() {
    let home = Home::new();
    let mut menu = Menu::start(&home, "not a url");
    let add = menu.ask("/add");
    assert!(add.starts_with("HTTP/1.1 204"), "{add}");
    let page = menu.ask("/callback");
    assert!(page.contains("Signed out"), "{page}");
    assert!(page.contains("href=\"/add\""), "{page}");
}

/// The handoff variables are required. A value that is not a descriptor never
/// reaches the socket call.
#[test]
fn the_menu_refuses_a_handoff_it_cannot_use() {
    let home = Home::new();
    let missing = Command::new(HANZO)
        .env("HOME", home.path())
        .env("HANZO_LOGIN_FD", "not-a-descriptor")
        .env("HANZO_LOGIN_ORIGIN", "http://127.0.0.1:9")
        .env("HANZO_LOGIN_BRAND", "hanzo")
        .output()
        .unwrap();
    assert!(!missing.status.success(), "a non-numeric descriptor was accepted");
    let err = String::from_utf8_lossy(&missing.stderr);
    assert!(err.contains("HANZO_LOGIN_FD"), "{err}");

    let no_origin = Command::new(HANZO)
        .env("HOME", home.path())
        .env("HANZO_LOGIN_FD", "3")
        .env_remove("HANZO_LOGIN_ORIGIN")
        .env("HANZO_LOGIN_BRAND", "hanzo")
        .output()
        .unwrap();
    assert!(!no_origin.status.success(), "a missing origin was accepted");
    let err = String::from_utf8_lossy(&no_origin.stderr);
    assert!(err.contains("HANZO_LOGIN_ORIGIN"), "{err}");
}

fn state_from(location: &str) -> String {
    location
        .split("state=")
        .nth(1)
        .unwrap_or_else(|| panic!("no state in {location}"))
        .split('&')
        .next()
        .unwrap()
        .trim()
        .to_string()
}
