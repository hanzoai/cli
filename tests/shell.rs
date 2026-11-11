//! Real subprocesses, isolated HOME: no caller profile or credentials touched.
#![cfg(unix)]
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

fn profile(home: &std::path::Path) -> std::path::PathBuf {
    home.join(if cfg!(target_os = "macos") {
        ".bash_profile"
    } else {
        ".bashrc"
    })
}

fn command(home: &std::path::Path) -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_hanzo"));
    c.env("HOME", home)
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env("XDG_DATA_HOME", home.join(".local/share"))
        .env_remove("ZDOTDIR")
        .env("SHELL", "/bin/bash")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    c
}

#[test]
fn startup_is_idempotent_singleton_and_uninstall_stops_it() {
    let home = tempfile::tempdir().unwrap();
    let profile = profile(home.path());
    std::fs::write(&profile, "# existing config\n").unwrap();
    for _ in 0..2 {
        assert!(command(home.path())
            .args(["shell", "install"])
            .status()
            .unwrap()
            .success());
    }
    let body = std::fs::read_to_string(&profile).unwrap();
    assert_eq!(body.matches("# >>> hanzo shell >>>").count(), 1);
    assert!(body.starts_with("# existing config\n"));
    let mut first = command(home.path())
        .args(["shell", "daemon"])
        .spawn()
        .unwrap();
    // Wait for the actual OS lock, rather than guessing when startup finished.
    let until = Instant::now() + Duration::from_secs(10);
    loop {
        if let Ok(file) = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(home.path().join(".hanzo/shell.lock"))
        {
            if matches!(file.try_lock(), Err(std::fs::TryLockError::WouldBlock)) {
                break;
            }
        }
        if Instant::now() > until {
            let _ = first.kill();
            panic!("daemon never took its lock");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(first.try_wait().unwrap().is_none());
    let mut second = command(home.path())
        .args(["shell", "daemon"])
        .spawn()
        .unwrap();
    let until = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(status) = second.try_wait().unwrap() {
            assert!(status.success());
            break;
        }
        if Instant::now() > until {
            let _ = first.kill();
            let _ = second.kill();
            panic!("duplicate daemon stayed alive");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(command(home.path())
        .args(["shell", "uninstall"])
        .status()
        .unwrap()
        .success());
    assert_eq!(
        std::fs::read_to_string(profile).unwrap(),
        "# existing config\n"
    );
    let until = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(status) = first.try_wait().unwrap() {
            assert!(status.success());
            break;
        }
        if Instant::now() > until {
            let _ = first.kill();
            panic!("uninstall did not stop daemon");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn malformed_profile_is_not_overwritten() {
    let home = tempfile::tempdir().unwrap();
    let profile = profile(home.path());
    let body = "# mine\n# >>> hanzo shell >>>\nunfinished\n";
    std::fs::write(&profile, body).unwrap();
    assert!(!command(home.path())
        .args(["shell", "install"])
        .status()
        .unwrap()
        .success());
    assert_eq!(std::fs::read_to_string(profile).unwrap(), body);
}

#[test]
fn installer_path_setup_does_not_enable_presence() {
    let home = tempfile::tempdir().unwrap();
    assert!(command(home.path())
        .args(["shell", "install", "--path-only"])
        .status()
        .unwrap()
        .success());
    let body = std::fs::read_to_string(profile(home.path())).unwrap();
    assert!(body.contains("export PATH="));
    assert!(!body.contains("shell daemon"));
    assert!(!home.path().join(".hanzo/shell-enabled").exists());
    assert!(command(home.path())
        .args(["shell", "daemon"])
        .status()
        .unwrap()
        .success());
    assert!(command(home.path())
        .args(["shell", "install"])
        .status()
        .unwrap()
        .success());
    assert!(command(home.path())
        .args(["shell", "install", "--path-only"])
        .status()
        .unwrap()
        .success());
    let body = std::fs::read_to_string(profile(home.path())).unwrap();
    assert!(
        body.contains("shell daemon"),
        "an upgrade preserves an existing opt-in"
    );
}
