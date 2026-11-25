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

#[test]
#[ignore = "exercises production models list and chat completions typed flags"]
fn production_models_list_and_chat_completions_flags() {
    let s = Session::new();

    // Table and JSON output from `hanzo models list`
    let table = s.cmd().args(["models", "list"]).assert().success();
    let table_out = String::from_utf8_lossy(&table.get_output().stdout);
    assert!(
        table_out.contains("zen"),
        "models list table missing zen models: {table_out}"
    );

    let json_out = s
        .cmd()
        .args(["models", "list", "-o", "json"])
        .assert()
        .success();
    let v: serde_json::Value = serde_json::from_slice(&json_out.get_output().stdout).unwrap();
    let count = v["data"]
        .as_array()
        .map(Vec::len)
        .or_else(|| v.as_array().map(Vec::len))
        .unwrap_or(0);
    assert!(count > 0, "models list JSON returned no models: {v}");

    // `--data` must fail cleanly with actionable typed-flag guidance
    let bad = s
        .cmd()
        .args(["chat", "completions", "--data", "{}"])
        .assert()
        .failure();
    let bad_err = String::from_utf8_lossy(&bad.get_output().stderr);
    assert!(
        bad_err.contains("--data") && bad_err.contains("--model"),
        "unexpected --data error text: {bad_err}"
    );

    // Typed flags succeed against production
    let comp = s
        .cmd()
        .args([
            "chat",
            "completions",
            "--model",
            "zen6-flash",
            "--messages",
            r#"{"role":"user","content":"Reply with OK"}"#,
            "--max-tokens",
            "16",
            "-o",
            "json",
        ])
        .assert()
        .success();
    let cv: serde_json::Value = serde_json::from_slice(&comp.get_output().stdout).unwrap();
    assert!(
        cv["choices"].as_array().is_some_and(|c| !c.is_empty()),
        "chat completions returned no choices: {cv}"
    );
}

#[test]
#[ignore = "runs installed hanzo-mcp on a cold package cache"]
fn production_mcp_cold_cache_initialize() {
    use std::io::{BufRead, BufReader, Write};
    use std::process::{Command as StdCommand, Stdio};
    use std::time::Instant;

    let mcp_bin = std::env::var_os("HANZO_E2E_MCP_BIN")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("hanzo-mcp"));
    let cold = tempfile::tempdir().unwrap();
    let start = Instant::now();
    let mut child = StdCommand::new(&mcp_bin)
        .env("HOME", cold.path())
        .env("XDG_CACHE_HOME", cold.path().join("cache"))
        .env("XDG_DATA_HOME", cold.path().join("data"))
        .env("XDG_CONFIG_HOME", cold.path().join("config"))
        .env("NPM_CONFIG_CACHE", cold.path().join("npm"))
        .env("UV_CACHE_DIR", cold.path().join("uv"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap_or_else(|e| panic!("failed to spawn {}: {e}", mcp_bin.display()));

    let mut stdin = child.stdin.take().unwrap();
    writeln!(
        stdin,
        r#"{{"jsonrpc":"2.0","id":1,"method":"initialize","params":{{"protocolVersion":"2024-11-05","capabilities":{{}},"clientInfo":{{"name":"hanzo-cli-e2e","version":"0.1"}}}}}}"#
    )
    .unwrap();
    stdin.flush().unwrap();

    let mut reader = BufReader::new(child.stdout.take().unwrap());
    let mut line = String::new();
    let n = reader.read_line(&mut line).unwrap();
    let elapsed = start.elapsed();
    let _ = child.kill();
    let _ = child.wait();

    assert!(
        n > 0,
        "hanzo-mcp closed stdout without answering initialize"
    );
    assert!(
        elapsed < Duration::from_secs(15),
        "hanzo-mcp cold-cache initialize exceeded timeout: {elapsed:?}"
    );
    let resp: serde_json::Value = serde_json::from_str(line.trim()).unwrap();
    assert_eq!(resp["id"], 1);
    assert!(resp["result"]["serverInfo"].is_object(), "{resp}");
}
