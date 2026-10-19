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
                    .and(predicate::str::contains("--watch")),
            );
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
