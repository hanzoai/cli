//! Hermetic canonical CLI lifecycle QA covering `start`, `up`, `ps`, `down`,
//! `restart`, `stop`, `status`, `doctor`, and `reset` (`#112`).

use assert_cmd::Command;
use serde_json::Value;
use std::path::Path;
use std::time::Instant;

fn isolated_hanzo(home: &Path, lifecycle_dir: &Path, cfg_dir: &Path) -> Command {
    let mut cmd = Command::cargo_bin("hanzo").expect("hanzo binary builds");
    cmd.env("NO_COLOR", "1");
    cmd.env("HANZO_NO_ANIMATION", "1");
    cmd.env("HOME", home);
    cmd.env("HANZO_LIFECYCLE_DIR", lifecycle_dir);
    for v in [
        "XDG_DATA_HOME",
        "XDG_CONFIG_HOME",
        "XDG_CACHE_HOME",
        "XDG_STATE_HOME",
        "HANZO_API_KEY",
    ] {
        cmd.env_remove(v);
    }
    cmd.arg("--config").arg(cfg_dir.join("config.toml"));
    cmd
}

#[test]
fn canonical_lifecycle_commands_and_state_transitions_qa() {
    let root = tempfile::tempdir().expect("tempdir");
    let home = root.path().join("home");
    let lc = root.path().join("lifecycle");
    let cfg = root.path().join("cfg");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&lc).unwrap();
    std::fs::create_dir_all(&cfg).unwrap();

    // Seed DesiredState with enabled platform workloads and a dead host app PID
    // plus an active k8s-backed app so `ps --json` & `down --all-workloads` are exercised.
    let seeded_state = serde_json::json!({
        "runtime": {
            "cpus": 4,
            "memory_mb": 4096,
            "disk_mb": 32768,
            "backend": "auto",
            "desired_running": true,
            "strict_registry": false
        },
        "platform": {
            "cloud_enabled": true,
            "cloud_image": "ghcr.io/hanzoai/cloud-oss:main",
            "engine_enabled": true
        },
        "apps": {
            "dead-host-app": {
                "slug": "dead-host-app",
                "name": "Dead Host App",
                "project_dir": root.path().to_string_lossy(),
                "namespace": "hanzo-app-dead-host-app",
                "kind": "package.json",
                "image": null,
                "command": "node server.js",
                "container_port": 3111,
                "node_port": 31111,
                "desired_running": true,
                "local_pid": 999999999u32
            },
            "stopped-k8s-app": {
                "slug": "stopped-k8s-app",
                "name": "Stopped K8s App",
                "project_dir": root.path().to_string_lossy(),
                "namespace": "hanzo-app-stopped-k8s-app",
                "kind": "Dockerfile",
                "image": "hanzo.local/stopped-k8s-app:dev",
                "command": null,
                "container_port": 8080,
                "node_port": 31222,
                "desired_running": false,
                "local_pid": null
            }
        }
    });
    let state_file = lc.join("state.json");
    std::fs::write(
        &state_file,
        serde_json::to_string_pretty(&seeded_state).unwrap(),
    )
    .unwrap();

    // Verify canonical CLI subcommand help surfaces + non-destructive live commands.
    for sub in [
        vec!["start", "--help"],
        vec!["up", "--help"],
        vec!["restart", "--help"],
        vec!["down", "--help"],
        vec!["stop", "--help"],
        vec!["reset", "--help"],
    ] {
        let t0 = Instant::now();
        isolated_hanzo(&home, &lc, &cfg)
            .args(&sub)
            .assert()
            .success();
        eprintln!(
            "[TIMING] hanzo {:?} -> {:.2} ms",
            sub,
            t0.elapsed().as_secs_f64() * 1000.0
        );
    }

    // `hanzo ps --json`: host app with dead PID and offline k3s must report "Stopped".
    let t_ps = Instant::now();
    let ps_out = isolated_hanzo(&home, &lc, &cfg)
        .args(["ps", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    eprintln!(
        "[TIMING] hanzo ps --json -> {:.2} ms",
        t_ps.elapsed().as_secs_f64() * 1000.0
    );
    let ps_json: Value = serde_json::from_slice(&ps_out).expect("valid ps json");
    let rows = ps_json["rows"].as_array().expect("rows array");
    let dead_row = rows
        .iter()
        .find(|r| r["component"] == "Dead Host App")
        .expect("Dead Host App row present");
    assert_eq!(dead_row["status"], "Stopped");
    assert_eq!(dead_row["endpoint"], "http://127.0.0.1:3111");

    let k8s_row = rows
        .iter()
        .find(|r| r["component"] == "Stopped K8s App")
        .expect("Stopped K8s App row present");
    assert_eq!(k8s_row["endpoint"], "http://127.0.0.1:31222");

    // `hanzo status --local` and `hanzo doctor --offline`
    let t_status = Instant::now();
    isolated_hanzo(&home, &lc, &cfg)
        .args(["status", "--local"])
        .assert()
        .success();
    eprintln!(
        "[TIMING] hanzo status --local -> {:.2} ms",
        t_status.elapsed().as_secs_f64() * 1000.0
    );

    let t_doc = Instant::now();
    isolated_hanzo(&home, &lc, &cfg)
        .args(["doctor", "--offline"])
        .assert()
        .success();
    eprintln!(
        "[TIMING] hanzo doctor --offline -> {:.2} ms",
        t_doc.elapsed().as_secs_f64() * 1000.0
    );

    // `hanzo down --all-workloads` must clear `cloud_enabled`, `engine_enabled`, and app desired_running,
    // while keeping runtime allocation and `~/.hanzo/up` warm state files intact.
    #[cfg(unix)]
    let warm_marker = {
        let up_dir = home.join(".hanzo").join("up");
        std::fs::create_dir_all(&up_dir).unwrap();
        let marker = up_dir.join("warm-cache.sentinel");
        std::fs::write(&marker, "cached\n").unwrap();
        marker
    };

    let t_down = Instant::now();
    isolated_hanzo(&home, &lc, &cfg)
        .args(["down", "--all-workloads"])
        .current_dir(root.path())
        .assert()
        .success();
    eprintln!(
        "[TIMING] hanzo down --all-workloads -> {:.2} ms",
        t_down.elapsed().as_secs_f64() * 1000.0
    );

    let after_down: Value =
        serde_json::from_str(&std::fs::read_to_string(&state_file).unwrap()).unwrap();
    assert_eq!(after_down["platform"]["cloud_enabled"], false);
    assert_eq!(after_down["platform"]["engine_enabled"], false);
    assert_eq!(
        after_down["apps"]["dead-host-app"]["desired_running"],
        false
    );
    assert!(
        state_file.is_file(),
        "down must preserve lifecycle state.json"
    );
    #[cfg(unix)]
    assert!(
        warm_marker.is_file(),
        "down must preserve warm cache under ~/.hanzo/up"
    );

    // `hanzo stop`: drains runtime cleanly without removing lifecycle config.
    let t_stop = Instant::now();
    isolated_hanzo(&home, &lc, &cfg)
        .args(["stop"])
        .assert()
        .success();
    eprintln!(
        "[TIMING] hanzo stop -> {:.2} ms",
        t_stop.elapsed().as_secs_f64() * 1000.0
    );

    // `hanzo reset` wipes isolated HOME/lifecycle state cleanly on Unix (where dirs::home_dir honors HOME).
    #[cfg(unix)]
    {
        let t_reset = Instant::now();
        isolated_hanzo(&home, &lc, &cfg)
            .args(["reset"])
            .assert()
            .success();
        eprintln!(
            "[TIMING] hanzo reset -> {:.2} ms",
            t_reset.elapsed().as_secs_f64() * 1000.0
        );
        assert!(!lc.exists(), "reset should remove isolated lifecycle dir");
        assert!(
            !home.join(".hanzo").join("up").exists(),
            "reset should remove isolated up cache dir"
        );
    }
}
