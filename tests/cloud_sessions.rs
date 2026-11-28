use assert_cmd::Command;
use predicates::prelude::*;

#[test]
fn cloud_entry_points_are_exposed() {
    for prefix in [vec![], vec!["dev"], vec!["code", "--cloud"]] {
        Command::cargo_bin("hanzo")
            .unwrap()
            .args(prefix)
            .arg("-h")
            .assert()
            .success()
            .stdout(
                predicate::str::contains("--cloud")
                    .and(predicate::str::contains("--teleport"))
                    .and(predicate::str::contains("--await-followup"))
                    .and(predicate::str::contains("--target"))
                    .and(predicate::str::contains("--timeout-seconds"))
                    .and(predicate::str::contains("--effort"))
                    .and(predicate::str::contains("--watch")),
            );
    }
}

#[test]
fn cloud_controls_require_a_cloud_session_and_valid_bounds() {
    for args in [
        vec!["--await-followup"],
        vec!["--cloud", "--await-followup"],
        vec!["--target", "machine"],
        vec!["--effort", "extreme"],
        vec!["--cloud", "--effort", "extreme", "task"],
        vec!["--cloud", "--timeout-seconds", "0", "task"],
        vec!["--cloud", "--timeout-seconds", "1801", "task"],
    ] {
        Command::cargo_bin("hanzo")
            .unwrap()
            .args(args)
            .assert()
            .failure()
            .stderr(predicate::str::contains("error:"));
    }
}

#[test]
fn cloud_does_not_silently_ignore_local_execution_options() {
    for option in ["--no-link", "--metered", "--no-sandbox", "--ask"] {
        Command::cargo_bin("hanzo")
            .unwrap()
            .args(["--cloud", option, "task"])
            .assert()
            .failure()
            .stderr(predicate::str::contains("cannot be used"));
    }
    Command::cargo_bin("hanzo")
        .unwrap()
        .args(["--mode", "plan", "task"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("--cloud"));
}

#[test]
fn help_advertises_expanded_reasoning_effort_ladder() {
    for args in [
        vec!["-h"],
        vec!["dev", "--help"],
        vec!["daemon", "dev", "--help"],
    ] {
        Command::cargo_bin("hanzo")
            .unwrap()
            .args(args)
            .assert()
            .success()
            .stdout(
                predicate::str::contains("--effort")
                    .and(predicate::str::contains("--reasoning-effort"))
                    .and(predicate::str::contains("minimal"))
                    .and(predicate::str::contains("xhigh"))
                    .and(predicate::str::contains("max")),
            );
    }

    let home = tempfile::tempdir().unwrap();
    Command::cargo_bin("hanzo")
        .unwrap()
        .env("HOME", home.path())
        .args(["sandbox", "explore"])
        .assert()
        .success()
        .stdout(
            predicate::str::contains("○ enso-auto")
                .and(predicate::str::contains("◕ enso-ultra"))
                .and(predicate::str::contains("◑ enso-max"))
                .and(predicate::str::contains("◔ enso-pro"))
                .and(predicate::str::contains("◦ enso-flash"))
                .and(predicate::str::contains("enso-free").not())
                .and(predicate::str::contains("Enso Free").not()),
        );
}
