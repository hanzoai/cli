//! Opt-in tests of real installed binaries and production services.
//! Run with HANZO_E2E_TOKEN and optionally HANZO_E2E_BIN; see E2E.md.
use assert_cmd::Command;
use std::{fs, path::PathBuf, time::Duration};

struct Session {
    root: tempfile::TempDir,
    binary: PathBuf,
}
impl Session {
    fn new() -> Self {
        let token = std::env::var("HANZO_E2E_TOKEN")
            .expect("HANZO_E2E_TOKEN is required; obtain it with hanzo auth token");
        assert!(!token.trim().is_empty(), "HANZO_E2E_TOKEN is empty");
        let s = Self {
            root: tempfile::tempdir().unwrap(),
            binary: std::env::var_os("HANZO_E2E_BIN")
                .map(PathBuf::from)
                .unwrap_or_else(|| assert_cmd::cargo::cargo_bin("hanzo")),
        };
        s.cmd()
            .args(["auth", "login", "--token", "-"])
            .write_stdin(format!("{}\n", token.trim()))
            .assert()
            .success();
        s
    }
    fn cmd(&self) -> Command {
        let mut c = Command::new(&self.binary);
        c.current_dir(self.root.path())
            .env("HOME", self.root.path())
            .env("NO_COLOR", "1")
            .env("HANZO_NO_ANIMATION", "1")
            .args([
                "--config",
                self.root.path().join("config.toml").to_str().unwrap(),
            ])
            .timeout(Duration::from_secs(300));
        for key in [
            "XDG_DATA_HOME",
            "XDG_CONFIG_HOME",
            "XDG_CACHE_HOME",
            "XDG_STATE_HOME",
        ] {
            c.env_remove(key);
        }
        c
    }
}

#[test]
#[ignore = "uses a real production identity and network"]
fn production_login_and_usage() {
    let s = Session::new();
    s.cmd().args(["auth", "show"]).assert().success();
    let out = s.cmd().args(["usage", "summary"]).assert().success();
    let v: serde_json::Value = serde_json::from_slice(&out.get_output().stdout).unwrap();
    assert!(v["scope"]["org"].as_str().is_some_and(|s| !s.is_empty()));
    assert!(v["scope"]["user"].as_str().is_some_and(|s| !s.is_empty()));
}

#[test]
#[ignore = "runs a metered coding task in a temporary directory"]
fn production_local_dev_writes_a_file() {
    let s = Session::new();
    Command::new("git")
        .args(["init", "--quiet"])
        .current_dir(s.root.path())
        .assert()
        .success();
    s.cmd().args(["dev", "--metered", "--no-link", "--no-mcp",
        "Create the file hanzo-e2e.txt in the current directory containing exactly HANZO_E2E_OK followed by a newline. Do not modify any other file. Then read it back to verify it."])
        .assert().success();
    assert_eq!(
        fs::read_to_string(s.root.path().join("hanzo-e2e.txt")).unwrap(),
        "HANZO_E2E_OK\n"
    );
}

#[test]
#[ignore = "leases a production sandbox and runs a metered read-only task"]
fn production_cloud_session_completes() {
    let s = Session::new();
    let out = s
        .cmd()
        .args([
            "--cloud",
            "--mode",
            "plan",
            "--repo",
            "https://github.com/hanzoai/cli",
            "-p",
            "Read Cargo.toml and report the package name. Do not edit files.",
            "--watch",
        ])
        .assert()
        .success();
    let text = String::from_utf8_lossy(&out.get_output().stdout);
    assert!(
        text.contains("hanzo-cli"),
        "cloud task did not report the repository's package name"
    );
}
