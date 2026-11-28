//! Canonical CLI Lifecycle Specification conformance tests:
//! - `hanzo start / stop / restart` with human-readable `--disk 32GiB --memory 8GiB --cpus 8` & `--native`
//! - `hanzo up` (`--ai-only`, `--cloud-only`, `--no-ui`) & `hanzo down`
//! - `hanzo service start|stop|restart|list`
//! - `hanzo app start|stop|list|logs`
//! - `hanzo model pull|run|list` (while preserving `hanzo models` cloud catalog!)
//! - `hanzo sandbox start|stop`
//! - `hanzo run <target> [args...]`
//! - `hanzo dev --debug`
//! - `hanzo ps`, `hanzo status --local`, `hanzo reset --all`

use crate::*;
use clap::{CommandFactory, Parser};

#[test]
fn canonical_runtime_and_environment_commands_parse_with_human_units() {
    // Plain defaults
    let cli = Cli::try_parse_from(["hanzo", "up"]).expect("bare up parses");
    let Some(Commands::Up {
        command: None,
        cpus,
        memory,
        disk_size,
        cloud,
        ai_only,
        cloud_only,
        no_ui,
        native,
        ..
    }) = cli.command
    else {
        panic!("expected bare up")
    };
    assert_eq!((cpus, memory, disk_size), (4, 4096, 32768));
    assert_eq!(cloud, commands::up::CLOUD);
    assert!(!ai_only && !cloud_only && !no_ui && !native);

    // Human-readable size flags on `hanzo start`
    let cli = Cli::try_parse_from([
        "hanzo", "start", "--disk", "32GiB", "--memory", "8GiB", "--cpus", "8", "--native",
    ])
    .expect("start with 32GiB/8GiB parses");
    assert!(matches!(
        cli.command,
        Some(Commands::Start {
            cpus: 8,
            memory: 8192,
            disk_size: 32768,
            native: true,
            ..
        })
    ));

    // Plain MB integer backward compatibility (`4096` = 4096 MiB)
    let cli = Cli::try_parse_from([
        "hanzo",
        "start",
        "--cpus",
        "8",
        "--memory",
        "4096",
        "--disk-size",
        "65536",
    ])
    .expect("start with plain integers parses");
    assert!(matches!(
        cli.command,
        Some(Commands::Start {
            cpus: 8,
            memory: 4096,
            disk_size: 65536,
            ..
        })
    ));

    // `hanzo stop`, `hanzo restart`, `hanzo reset`
    assert!(matches!(
        Cli::try_parse_from(["hanzo", "stop"]).unwrap().command,
        Some(Commands::Stop)
    ));
    let cli = Cli::try_parse_from([
        "hanzo", "restart", "--disk", "64GiB", "--memory", "16GiB", "--cpus", "12",
    ])
    .expect("restart with human sizes parses");
    assert!(matches!(
        cli.command,
        Some(Commands::Restart {
            cpus: 12,
            memory: 16384,
            disk_size: 65536,
            ..
        })
    ));
    assert!(matches!(
        Cli::try_parse_from(["hanzo", "reset", "--all"])
            .unwrap()
            .command,
        Some(Commands::Reset { all: true })
    ));

    // `hanzo up --ai-only`, `--cloud-only`, `--no-ui`
    let ai = Cli::try_parse_from(["hanzo", "up", "--ai-only", "--no-ui"])
        .expect("up --ai-only --no-ui parses");
    assert!(matches!(
        ai.command,
        Some(Commands::Up {
            ai_only: true,
            cloud_only: false,
            no_ui: true,
            ..
        })
    ));

    let cloud_only =
        Cli::try_parse_from(["hanzo", "up", "--cloud-only"]).expect("up --cloud-only parses");
    assert!(matches!(
        cloud_only.command,
        Some(Commands::Up {
            ai_only: false,
            cloud_only: true,
            ..
        })
    ));

    // `--ai-only` and `--cloud-only` conflict
    assert!(Cli::try_parse_from(["hanzo", "up", "--ai-only", "--cloud-only"]).is_err());

    // `hanzo down` variants
    assert!(matches!(
        Cli::try_parse_from(["hanzo", "down"]).unwrap().command,
        Some(Commands::Down {
            vm: false,
            cloud: false,
            all_workloads: false
        })
    ));
    assert!(matches!(
        Cli::try_parse_from(["hanzo", "down", "--vm"])
            .unwrap()
            .command,
        Some(Commands::Down { vm: true, .. })
    ));
    assert!(matches!(
        Cli::try_parse_from(["hanzo", "down", "--cloud"])
            .unwrap()
            .command,
        Some(Commands::Down { cloud: true, .. })
    ));

    // Deprecated `hanzo up <service>` still forwards
    let cli = Cli::try_parse_from(["hanzo", "up", "iam"]).expect("old spelling parses");
    assert!(matches!(
        cli.command,
        Some(Commands::Up {
            command: Some(UpCommands::Service(_)),
            ..
        })
    ));
}

#[test]
fn canonical_resource_vocabulary_parses_cleanly() {
    // Services: `hanzo service start|stop|restart|list`
    let svc_start =
        Cli::try_parse_from(["hanzo", "service", "start", "cloud"]).expect("service start");
    assert!(matches!(
        svc_start.command,
        Some(Commands::Service {
            command: Some(ServiceCommands::Start { name })
        }) if name == "cloud"
    ));
    let svc_stop =
        Cli::try_parse_from(["hanzo", "service", "stop", "cloud"]).expect("service stop");
    assert!(matches!(
        svc_stop.command,
        Some(Commands::Service {
            command: Some(ServiceCommands::Stop { name })
        }) if name == "cloud"
    ));
    assert!(Cli::try_parse_from(["hanzo", "service", "restart", "engine"]).is_ok());
    assert!(Cli::try_parse_from(["hanzo", "service", "list"]).is_ok());
    assert!(Cli::try_parse_from(["hanzo", "service", "ls"]).is_ok());
    assert!(Cli::try_parse_from(["hanzo", "services", "status"]).is_ok());

    // Applications: `hanzo app start|stop|list|logs`
    let app_start = Cli::try_parse_from(["hanzo", "app", "start", "solar"]).expect("app start");
    assert!(matches!(
        app_start.command,
        Some(Commands::App {
            command: Some(AppCommands::Start { name: Some(n) })
        }) if n == "solar"
    ));
    let app_start_cwd = Cli::try_parse_from(["hanzo", "app", "start"]).expect("app start cwd");
    assert!(matches!(
        app_start_cwd.command,
        Some(Commands::App {
            command: Some(AppCommands::Start { name: None })
        })
    ));
    let app_stop = Cli::try_parse_from(["hanzo", "app", "stop", "solar"]).expect("app stop");
    assert!(matches!(
        app_stop.command,
        Some(Commands::App {
            command: Some(AppCommands::Stop { name: Some(n) })
        }) if n == "solar"
    ));
    assert!(Cli::try_parse_from(["hanzo", "app", "list"]).is_ok());
    assert!(Cli::try_parse_from(["hanzo", "app", "ls"]).is_ok());
    let app_logs =
        Cli::try_parse_from(["hanzo", "app", "logs", "solar", "-f"]).expect("app logs -f");
    assert!(matches!(
        app_logs.command,
        Some(Commands::App {
            command: Some(AppCommands::Logs {
                name: Some(n),
                follow: true
            })
        }) if n == "solar"
    ));

    // Models: `hanzo model pull|run|list` AND `hanzo models list` cloud catalog stays untouched!
    let m_pull =
        Cli::try_parse_from(["hanzo", "model", "pull", "zenlm/zen-nano-0.6b"]).expect("model pull");
    assert!(matches!(
        m_pull.command,
        Some(Commands::Model {
            command: ModelCommands::Pull { model, .. }
        }) if model == "zenlm/zen-nano-0.6b"
    ));
    let m_run = Cli::try_parse_from(["hanzo", "model", "run", "zen-nano", "hello", "world"])
        .expect("model run");
    assert!(matches!(
        m_run.command,
        Some(Commands::Model {
            command: ModelCommands::Run { model, prompt }
        }) if model == "zen-nano" && prompt == ["hello", "world"]
    ));
    assert!(Cli::try_parse_from(["hanzo", "model", "list"]).is_ok());
    assert!(Cli::try_parse_from(["hanzo", "model", "ls"]).is_ok());

    // Verify `hanzo models list` still resolves to generated `/v1/models` cloud product
    let hand = Cli::command();
    let merged = commands::product::augment(hand.clone());
    let m = merged
        .try_get_matches_from(["hanzo", "models", "list"])
        .expect("plural `hanzo models list` remains cloud product");
    assert!(commands::product::resolve(&hand, &m).is_some());

    // Sandboxes: `hanzo sandbox start [name]` & `hanzo sandbox stop [name]`
    let sbx_start = Cli::try_parse_from([
        "hanzo",
        "sandbox",
        "start",
        "claude-env",
        "--node",
        "spark.local",
    ])
    .expect("sandbox start");
    assert!(matches!(
        sbx_start.command,
        Some(Commands::Sandbox {
            command: Some(SandboxCommands::Start {
                name: Some(n),
                node,
                ..
            })
        }) if n == "claude-env" && node == "spark.local"
    ));
    assert!(Cli::try_parse_from(["hanzo", "sandbox", "start"]).is_ok());
    assert!(Cli::try_parse_from(["hanzo", "sandbox", "stop"]).is_ok());
    assert!(Cli::try_parse_from(["hanzo", "sandbox", "stop", "claude-env"]).is_ok());
    assert!(Cli::try_parse_from(["hanzo", "sandbox", "ls"]).is_ok());

    // Execution & Development: `hanzo run <target> [args...]` & `hanzo dev --debug`
    let run_cmd =
        Cli::try_parse_from(["hanzo", "run", "zen-nano", "explain", "k3s"]).expect("hanzo run");
    assert!(matches!(
        run_cmd.command,
        Some(Commands::Run { target, args }) if target == "zen-nano" && args == ["explain", "k3s"]
    ));

    let dev_dbg = Cli::try_parse_from(["hanzo", "dev", "--debug"]).expect("hanzo dev --debug");
    let Some(Commands::Dev(dev_args)) = dev_dbg.command else {
        panic!("expected dev")
    };
    assert!(dev_args.remote.debug);

    // `hanzo ps`, `hanzo ls`, `hanzo status --local`
    assert!(matches!(
        Cli::try_parse_from(["hanzo", "ps"]).unwrap().command,
        Some(Commands::Ps { json: false })
    ));
    assert!(matches!(
        Cli::try_parse_from(["hanzo", "ps", "--json"])
            .unwrap()
            .command,
        Some(Commands::Ps { json: true })
    ));
    assert!(matches!(
        Cli::try_parse_from(["hanzo", "ls"]).unwrap().command,
        Some(Commands::Ls)
    ));
    assert!(matches!(
        Cli::try_parse_from(["hanzo", "status", "--local"])
            .unwrap()
            .command,
        Some(Commands::Status {
            infer: false,
            local: true
        })
    ));
}

#[test]
fn lifecycle_resource_flags_and_down_conflicts_validate_cleanly() {
    // `--cpus` accepts 1..=256 and rejects out-of-range or non-numeric values.
    assert!(matches!(
        Cli::try_parse_from(["hanzo", "start", "--cpus", "1"])
            .unwrap()
            .command,
        Some(Commands::Start { cpus: 1, .. })
    ));
    assert!(matches!(
        Cli::try_parse_from(["hanzo", "start", "--cpus", "256"])
            .unwrap()
            .command,
        Some(Commands::Start { cpus: 256, .. })
    ));
    for bad in ["0", "257", "abc"] {
        let err = Cli::try_parse_from(["hanzo", "start", "--cpus", bad])
            .err()
            .expect("invalid --cpus must fail")
            .to_string();
        assert!(err.contains("--cpus"), "{err}");
    }

    // `--memory` enforces >= 256 MiB and `--disk` / `--disk-size` enforces >= 1 GiB.
    assert!(matches!(
        Cli::try_parse_from([
            "hanzo",
            "up",
            "--memory",
            "256MiB",
            "--disk",
            "1GiB",
            "--cloud-only"
        ])
        .unwrap()
        .command,
        Some(Commands::Up {
            memory: 256,
            disk_size: 1024,
            cloud_only: true,
            ..
        })
    ));
    let low_mem = Cli::try_parse_from(["hanzo", "up", "--memory", "128MiB"])
        .err()
        .expect("under-minimum memory must fail")
        .to_string();
    assert!(low_mem.contains("256 MiB"), "{low_mem}");

    let low_disk = Cli::try_parse_from(["hanzo", "start", "--disk", "512MiB"])
        .err()
        .expect("under-minimum disk must fail")
        .to_string();
    assert!(low_disk.contains("1024 MiB"), "{low_disk}");

    let restart_low_cpu = Cli::try_parse_from(["hanzo", "restart", "--cpus", "0"])
        .err()
        .expect("restart --cpus 0 must fail")
        .to_string();
    assert!(restart_low_cpu.contains("--cpus"), "{restart_low_cpu}");

    // `hanzo down` and `hanzo up down` reject conflicting scope flags and support `--vm` / `--cloud` / `--all-workloads`.
    assert!(Cli::try_parse_from(["hanzo", "down", "--vm", "--cloud"]).is_err());
    assert!(Cli::try_parse_from(["hanzo", "down", "--vm", "--all-workloads"]).is_err());
    assert!(Cli::try_parse_from(["hanzo", "down", "--cloud", "--all-workloads"]).is_err());
    assert!(Cli::try_parse_from(["hanzo", "up", "down", "--vm", "--cloud"]).is_err());
    assert!(matches!(
        Cli::try_parse_from(["hanzo", "up", "down", "--cloud"])
            .unwrap()
            .command,
        Some(Commands::Up {
            command: Some(UpCommands::Down {
                vm: false,
                cloud: true,
                all_workloads: false,
            }),
            ..
        })
    ));
}
