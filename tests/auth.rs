//! `hanzo auth`, against the shipped `hanzo` binary: which identity a sign-in
//! files and activates, what `list` shows, and how `logout` ends the browser's
//! session. Config and credentials sit under a throwaway `HOME`, so nothing
//! touches the machine this suite runs on. Nothing here dials hanzo.id except
//! the seeding logins, which give up on userinfo after its own two seconds.

#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::time::Duration;

const HANZO: &str = env!("CARGO_BIN_EXE_hanzo");

struct Home {
    root: PathBuf,
}

impl Home {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "hanzo-auth-{}-{}",
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

/// A token as IAM mints it through `<owner>-cli`: `azp` names that client.
fn jwt(owner: &str, name: &str, email: Option<&str>) -> String {
    let email = email
        .map(|e| format!(r#","email":"{e}""#))
        .unwrap_or_default();
    let claims =
        format!(r#"{{"owner":"{owner}","name":"{name}","sub":"u-1","azp":"{owner}-cli"{email}}}"#);
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


/// `auth list` names each identity as `login` reports it and `use` takes it,
/// with the address its token claims, and marks the active one.
#[test]
fn the_listing_names_each_identity_by_owner_name_and_address() {
    let home = Home::new();
    sign_in(&home, &jwt("hanzo", "z", Some("z@hanzo.ai")));
    sign_in(&home, &jwt("hanzo", "grundy", Some("grundy@example.com")));
    let list = listed(&home);
    assert!(list.contains("* hanzo/grundy  grundy@example.com"), "{list}");
    assert!(list.contains("  hanzo/z  z@hanzo.ai"), "{list}");

    hanzo(&home).args(["auth", "use", "hanzo/z"]).assert().success();
    let list = listed(&home);
    assert!(list.contains("* hanzo/z  z@hanzo.ai"), "{list}");
}

/// `--account` and `--choose` pick a Hanzo account in the browser; a token
/// handed over already is one, so the two never combine with `--token`.
#[test]
fn an_account_or_the_chooser_never_rides_a_handed_token() {
    let home = Home::new();
    for args in [
        vec!["auth", "login", "--token", "-", "--account", "hanzo/z"],
        vec!["auth", "login", "--token", "-", "--choose"],
        vec!["login", "--token", "-", "--account", "z@hanzo.ai"],
    ] {
        let out = hanzo(&home).args(&args).write_stdin("x\n").output().unwrap();
        assert!(!out.status.success(), "{args:?} was accepted");
        assert!(String::from_utf8_lossy(&out.stderr).contains("cannot be used with"), "{args:?}");
    }
    let out = hanzo(&home)
        .args(["auth", "login", "--provider", "openai", "--account", "hanzo/z"])
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("pick a Hanzo account"));
}

/// With no browser to open, `logout` still ends the session at the server and
/// prints the end-session link for the browser that holds hanzo.id's cookie:
/// the CLI's own client, and no id token written into a URL.
#[test]
fn logout_without_a_browser_prints_the_end_session_link() {
    let home = Home::new();
    sign_in(&home, &jwt("hanzo", "z", Some("z@hanzo.ai")));
    let out = hanzo(&home)
        .env_remove("DISPLAY")
        .env_remove("WAYLAND_DISPLAY")
        .args(["auth", "logout"])
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("Signed out of hanzo as hanzo/z"), "{stdout}");
    assert!(stdout.contains("To sign out of hanzo.id in a browser too, open:"), "{stdout}");
    assert!(
        stdout.contains("https://hanzo.id/v1/iam/oauth/logout?client_id=hanzo-cli"),
        "{stdout}"
    );
    assert!(!stdout.contains("id_token_hint"), "{stdout}");
}

/// `--as admin` files the admin identity beside the default, which stays the
/// default, and `--as admin` then speaks as it. A token from another org is
/// refused and filed nowhere, and `auth show` under an org with no identity
/// held says so instead of naming the default.
#[test]
fn as_an_org_files_its_identity_beside_the_default_and_speaks_as_it() {
    let home = Home::new();
    sign_in(&home, &jwt("hanzo", "z", Some("z@hanzo.ai")));
    let as_admin = |token: String| {
        hanzo(&home)
            .args(["--as", "admin", "auth", "login", "--provider", "hanzo", "--token", "-"])
            .write_stdin(format!("{token}\n"))
            .assert()
    };

    let refused = hanzo(&home).args(["--as", "admin", "auth", "show"]).assert().failure();
    let err = String::from_utf8_lossy(&refused.get_output().stderr).into_owned();
    assert!(err.contains("no hanzo identity in admin"), "{err}");
    assert!(err.contains("without --as you are hanzo/z"), "{err}");

    let wrong = as_admin(jwt("hanzo", "x", None)).failure();
    let err = String::from_utf8_lossy(&wrong.get_output().stderr).into_owned();
    assert!(err.contains("IAM returned hanzo/x, which is not in admin"), "{err}");
    assert!(!listed(&home).contains("hanzo/x"), "a token from another org was filed");

    let added = as_admin(jwt("admin", "z", Some("z@hanzo.ai"))).success();
    let out = String::from_utf8_lossy(&added.get_output().stdout).into_owned();
    assert!(out.contains("hanzo/z stays the default; `hanzo --as admin …` speaks as admin/z"), "{out}");
    let list = listed(&home);
    assert!(list.contains("* hanzo/z") && list.contains("  admin/z"), "{list}");

    // `auth show` reads userinfo after naming the identity; the stand-in token is
    // refused there, so only the identity line is asserted.
    let shown = hanzo(&home).args(["--as", "admin", "auth", "show"]).output().unwrap();
    let out = String::from_utf8_lossy(&shown.stdout);
    assert!(out.contains("identity: admin/z"), "{out}");
    let shown = hanzo(&home).args(["auth", "show"]).output().unwrap();
    let out = String::from_utf8_lossy(&shown.stdout);
    assert!(out.contains("identity: hanzo/z"), "{out}");
}

/// Under `--as <org>`, `auth logout` and `auth token` act on the identity held
/// in that org, and refuse when there is none rather than act on the default.
#[test]
fn as_an_org_signs_out_and_hands_out_only_its_own_identity() {
    let home = Home::new();
    sign_in(&home, &jwt("hanzo", "z", Some("z@hanzo.ai")));
    let run = |args: &[&str]| hanzo(&home).args(args).output().unwrap();

    for refused in [run(&["--as", "admin", "auth", "logout"]), run(&["--as", "admin", "auth", "token"])] {
        assert!(!refused.status.success());
        assert!(refused.stdout.is_empty(), "{}", String::from_utf8_lossy(&refused.stdout));
        let err = String::from_utf8_lossy(&refused.stderr);
        assert!(err.contains("no hanzo identity in admin"), "{err}");
    }
    assert!(listed(&home).contains("* hanzo/z"), "the default was signed out");

    let admin = jwt("admin", "z", None);
    hanzo(&home)
        .args(["--as", "admin", "auth", "login", "--provider", "hanzo", "--token", "-"])
        .write_stdin(format!("{admin}\n"))
        .assert()
        .success();
    let token = run(&["--as", "admin", "auth", "token"]);
    assert_eq!(String::from_utf8_lossy(&token.stdout).trim(), admin);
    let out = run(&["--as", "admin", "auth", "logout"]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert!(String::from_utf8_lossy(&out.stdout).contains("Signed out of hanzo as admin/z"));
    let list = listed(&home);
    assert!(list.contains("* hanzo/z") && !list.contains("admin/z"), "{list}");
}
