//! Core CLI parsing, coding-session backend selection, auth, chain, and link tests.

use crate::*;
use clap::{CommandFactory, Parser};

fn root() -> hanzo_gen::Root {
    static DIR: std::sync::OnceLock<tempfile::TempDir> = std::sync::OnceLock::new();
    hanzo_gen::Root::at(
        DIR.get_or_init(|| tempfile::tempdir().expect("a temp dir"))
            .path(),
    )
}

#[test]
fn bare_hanzo_has_no_subcommand() {
    let cli = Cli::try_parse_from(["hanzo"]).expect("bare hanzo parses");
    assert!(cli.command.is_none());
}

#[test]
fn bare_hanzo_carries_top_level_code_flags() {
    let cli = Cli::try_parse_from(["hanzo", "--model", "enso", "fix the bug"]).expect("parses");
    assert!(cli.command.is_none());
    assert_eq!(cli.code.model.as_deref(), Some("enso"));
    assert_eq!(cli.code.positional.as_deref(), Some("fix the bug"));

    let cli = Cli::try_parse_from(["hanzo", "--safe"]).expect("parses");
    assert!(cli.code.ask);
    assert!(Cli::try_parse_from(["hanzo", "--ask", "--no-sandbox"]).is_err());
}

#[test]
fn the_duplicate_agent_spelling_is_gone_and_desktop_survives() {
    assert!(Cli::try_parse_from(["hanzo", "agent", "run", "fix it"]).is_err());
    assert!(Cli::try_parse_from(["hanzo", "agent", "run", "--mode", "code"]).is_err());
    assert!(Cli::try_parse_from(["hanzo", "agent", "run", "--mode", "desktop"]).is_err());

    let cli = Cli::try_parse_from(["hanzo", "desktop", "--model", "enso", "browse docs"])
        .expect("`hanzo desktop` parses");
    let Some(Commands::Desktop(code)) = cli.command else {
        panic!("expected desktop")
    };
    assert_eq!(code.model.as_deref(), Some("enso"));
    assert_eq!(code.positional.as_deref(), Some("browse docs"));
}

#[test]
fn every_entry_spelling_resolves_to_one_session() {
    use hanzo_gen::backend::BackendKind;

    fn resolved(argv: &[&str]) -> BackendKind {
        let cli = Cli::try_parse_from(argv).expect("parses");
        let args = match cli.command {
            Some(Commands::Dev(a)) => a.dev("hanzo dev").expect("dev resolves"),
            None => cli.code,
            _ => panic!("expected a coding session for {argv:?}"),
        };
        args.into_options(root()).expect("resolves").backend
    }

    assert_eq!(resolved(&["hanzo", "dev"]), BackendKind::Dev);
    assert_eq!(resolved(&["hanzo", "dev", "fix the bug"]), BackendKind::Dev);
    assert_eq!(resolved(&["hanzo", "fix the bug"]), BackendKind::Dev);
    assert_eq!(resolved(&["hanzo", "dev", "--dev"]), BackendKind::Dev);
    assert_eq!(
        resolved(&["hanzo", "dev", "--backend", "dev"]),
        BackendKind::Dev
    );

    for (flag, kind) in [
        ("--claude", BackendKind::Claude),
        ("--codex", BackendKind::Codex),
        ("--agy", BackendKind::parse("agy").unwrap()),
        ("--cursor", BackendKind::parse("cursor").unwrap()),
        ("--agent", BackendKind::parse("cursor").unwrap()),
    ] {
        assert_eq!(resolved(&["hanzo", "dev", flag]), kind, "{flag}");
        assert_eq!(
            resolved(&["hanzo", "dev", flag, "fix it"]),
            kind,
            "{flag} + task"
        );
        assert_eq!(
            resolved(&["hanzo", "dev", flag, "--", "-c"]),
            kind,
            "{flag} + passthrough"
        );
    }
    for argv in [
        ["hanzo", "agy"].as_slice(),
        ["hanzo", "antigravity"].as_slice(),
    ] {
        assert_eq!(resolved(argv).as_str(), "agy", "{argv:?}");
    }
    assert_eq!(resolved(&["hanzo", "agent"]).as_str(), "cursor");

    let merged = commands::product::augment(Cli::command());
    for name in [
        "gemini",
        "qwen",
        "opencode",
        "kilo",
        "crush",
        "pi",
        "copilot",
        "amp",
        "grok",
        "kilocode",
        "muse",
        "muse-code",
    ] {
        let argv: Vec<String> = ["hanzo", "dev", &format!("--{name}"), "fix it"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let argv = backend_flags(&merged, argv);
        let cli = Cli::try_parse_from(&argv).unwrap_or_else(|e| panic!("{name}: {e}"));
        let Some(Commands::Dev(args)) = cli.command else {
            panic!("expected dev")
        };
        let opts = args
            .dev("hanzo dev")
            .unwrap()
            .into_options(root())
            .expect("resolves");
        assert_eq!(opts.backend, BackendKind::parse(name).unwrap(), "{name}");
        assert_eq!(opts.task.as_deref(), Some("fix it"));
        let bare = backend_flags(
            &merged,
            ["hanzo", &format!("--{name}")]
                .iter()
                .map(|s| s.to_string())
                .collect(),
        );
        assert_eq!(bare[1..], ["--backend".to_string(), name.to_string()]);
    }

    let keep = |line: &[&str]| -> Vec<String> { line.iter().map(|s| s.to_string()).collect() };
    assert_eq!(
        backend_flags(&merged, keep(&["hanzo", "--claude"])),
        keep(&["hanzo", "--claude"])
    );
    assert_eq!(
        backend_flags(&merged, keep(&["hanzo", "vm", "run", "--gemini"])),
        keep(&["hanzo", "vm", "run", "--gemini"])
    );

    let cli =
        Cli::try_parse_from(["hanzo", "dev", "--claude", "fix it", "--", "-c"]).expect("parses");
    let Some(Commands::Dev(args)) = cli.command else {
        panic!("expected dev")
    };
    let opts = args
        .dev("hanzo dev")
        .unwrap()
        .into_options(root())
        .expect("resolves");
    assert_eq!(opts.task.as_deref(), Some("fix it"));
    assert_eq!(opts.passthrough, ["-c"]);

    let cli = Cli::try_parse_from([
        "hanzo",
        "dev",
        "--claude",
        "--no-link",
        "--no-mcp",
        "--resume",
        "abc",
        "--runtime",
        "runc",
    ])
    .expect("parses");
    let Some(Commands::Dev(args)) = cli.command else {
        panic!("expected dev")
    };
    let opts = args
        .dev("hanzo dev")
        .unwrap()
        .into_options(root())
        .expect("resolves");
    assert!(!opts.cloud && opts.no_link && !opts.mcp && opts.no_sandbox);
    assert_eq!(opts.resume.as_deref(), Some("abc"));

    let cli = Cli::try_parse_from(["hanzo", "dev", "--claude", "--metered"]).expect("parses");
    let Some(Commands::Dev(args)) = cli.command else {
        panic!("expected dev")
    };
    assert!(
        args.dev("hanzo dev")
            .unwrap()
            .into_options(root())
            .expect("resolves")
            .cloud
    );
    assert!(Cli::try_parse_from(["hanzo", "dev", "--claude", "--no-route"]).is_err());
    assert!(Cli::try_parse_from(["hanzo", "--no-route"]).is_err());
}

#[test]
fn contradictory_backend_spellings_are_refused() {
    assert!(Cli::try_parse_from(["hanzo", "dev", "--claude", "--codex"]).is_err());
    assert!(Cli::try_parse_from(["hanzo", "dev", "--claude", "--agy"]).is_err());
    assert!(Cli::try_parse_from(["hanzo", "dev", "--dev", "--backend", "claude"]).is_err());

    let cli = Cli::try_parse_from(["hanzo", "claude", "--codex"]).expect("parses");
    let err = match cli.code.into_options(root()) {
        Err(e) => e.to_string(),
        Ok(_) => panic!("`claude --codex` must be refused, not resolved"),
    };
    assert!(err.contains("named twice"), "got: {err}");
}

#[test]
fn a_lone_near_miss_names_the_command_and_a_task_still_runs() {
    let merged = commands::product::augment(Cli::command());
    let line = |words: &[&str]| -> Vec<String> {
        std::iter::once("hanzo")
            .chain(words.iter().copied())
            .map(String::from)
            .collect()
    };
    let e = misspelled(&merged, &line(&["stauts"])).expect("a near miss");
    assert!(e.contains("`hanzo status`"), "{e}");
    assert!(misspelled(&merged, &line(&["whoami"])).is_none());
    assert!(misspelled(&merged, &line(&["claude"])).is_none());
    assert!(misspelled(&merged, &line(&["fix the failing test"])).is_none());
    assert!(misspelled(&merged, &line(&["stauts", "--cloud"])).is_none());
}

#[test]
fn a_lone_word_that_is_one_nested_command_names_its_path() {
    let merged = commands::product::augment(Cli::command());
    let line = |words: &[&str]| -> Vec<String> {
        std::iter::once("hanzo")
            .chain(words.iter().copied())
            .map(String::from)
            .collect()
    };
    let e = misspelled(&merged, &line(&["balance"])).expect("a nested command");
    assert!(e.contains("did you mean `hanzo billing balance`?"), "{e}");
    assert!(e.contains("`hanzo dev \"balance\"`"), "{e}");
    let e = misspelled(&merged, &line(&["quota"])).expect("a nested command");
    assert!(e.contains("did you mean `hanzo sandbox quota`?"), "{e}");
    let e = misspelled(&merged, &line(&["undeploy"])).expect("a nested command");
    assert!(
        e.contains("did you mean `hanzo ai deployments owner undeploy`"),
        "{e}"
    );
    // An exact nested command (`billing mode`) and a near top-level one (`model`):
    // the exact path comes first, the near command second.
    let e = misspelled(&merged, &line(&["mode"])).expect("exact nested + near top-level");
    assert!(
        e.contains("did you mean `hanzo billing mode` or `hanzo model`?"),
        "{e}"
    );

    // `create` is a verb in dozens of groups: ambiguous, so still a task.
    assert!(misspelled(&merged, &line(&["create"])).is_none());
    // Explicit `hanzo dev balance` is never intercepted by `misspelled`.
    assert!(misspelled(&merged, &line(&["dev", "balance"])).is_none());
    assert!(
        misspelled(&merged, &line(&["nowhere-in-the-tree"])).is_none(),
        "a word that is no command anywhere is a task"
    );
}

#[test]
fn only_a_unique_nested_match_is_named() {
    use clap::Command;
    let tree = Command::new("hanzo")
        .subcommand(Command::new("status"))
        .subcommand(
            Command::new("ops")
                .subcommand(Command::new("go"))
                .subcommand(Command::new("stauts"))
                .subcommand(Command::new("only").alias("solo"))
                .subcommand(
                    Command::new("quiet")
                        .hide(true)
                        .subcommand(Command::new("hush")),
                ),
        )
        .subcommand(Command::new("ci").subcommand(Command::new("go")));
    let line = |w: &str| vec!["hanzo".to_string(), w.to_string()];
    assert!(misspelled(&tree, &line("go")).is_none(), "two: a task");
    let e = misspelled(&tree, &line("solo")).expect("an alias");
    assert!(e.contains("did you mean `hanzo ops only`?"), "{e}");
    assert!(misspelled(&tree, &line("hush")).is_none(), "hidden");
    let e = misspelled(&tree, &line("stauts")).expect("exact, and near status");
    assert!(
        e.contains("did you mean `hanzo ops stauts` or `hanzo status`?"),
        "{e}"
    );
    let e = misspelled(&tree, &line("statuss")).expect("a near top-level command");
    assert!(e.contains("did you mean `hanzo status`?"), "{e}");
}

#[test]
fn code_session_alias_rewrites_to_dev_while_code_index_ops_stay() {
    let merged = commands::product::augment(Cli::command());
    let argv = |line: &str| -> Vec<String> {
        std::iter::once("hanzo")
            .chain(line.split_whitespace())
            .map(String::from)
            .collect()
    };
    for (said, means) in [
        ("code", "dev"),
        ("code --claude", "dev --claude"),
        ("code --dev", "dev --dev"),
        ("code fix", "dev fix"),
        ("--as admin code --codex", "--as admin dev --codex"),
        ("-v code", "-v dev"),
    ] {
        assert_eq!(
            alias(&merged, hoist(&merged, argv(said))),
            hoist(&merged, argv(means)),
            "`hanzo {said}`"
        );
    }
    for kept in [
        "code search",
        "code search --help",
        "code index",
        "code context",
        "code file",
        "code tree",
        "code ask",
        "code --help",
        "code -h",
        "--as admin code search",
        "dev",
        "dev --claude",
        "desktop",
        "",
        "fix the failing test",
        "billing balance",
    ] {
        let line = hoist(&merged, argv(kept));
        assert_eq!(alias(&merged, line.clone()), line, "`hanzo {kept}`");
    }
}

#[test]
fn dev_takes_the_tool_as_a_flag() {
    let cli = Cli::try_parse_from(["hanzo", "dev", "claude"]).expect("parses");
    let Some(Commands::Dev(args)) = cli.command else {
        panic!("expected dev")
    };
    let err = args
        .dev("hanzo dev")
        .err()
        .expect("a tool named as a word is refused")
        .to_string();
    assert!(err.contains("`hanzo dev --claude`"), "got: {err}");

    let cli = Cli::try_parse_from(["hanzo", "dev", "fix the claude bug"]).expect("parses");
    let Some(Commands::Dev(args)) = cli.command else {
        panic!("expected dev")
    };
    assert_eq!(
        args.dev("hanzo dev").unwrap().positional.as_deref(),
        Some("fix the claude bug")
    );
}

#[test]
fn old_top_level_verbs_are_removed_and_canonical_nouns_exist() {
    let names: Vec<String> = Cli::command()
        .get_subcommands()
        .map(|s| s.get_name().to_string())
        .collect();
    for gone in ["logout", "deploy"] {
        assert!(
            !names.iter().any(|n| n == gone),
            "`{gone}` must no longer be a top-level subcommand"
        );
    }
    for gone in [
        "cluster", "node", "secret", "usage", "docs", "mdx", "ui",
    ] {
        assert!(
            !names.iter().any(|n| n == gone),
            "`{gone}` is a deleted hand command and must not be a top-level"
        );
    }
    for present in [
        "app", "auth", "build", "code", "config", "desktop", "dev", "engine", "login", "model",
        "ps", "run", "runner", "scan", "service", "switch", "up", "whoami",
    ] {
        assert!(
            names.iter().any(|n| n == present),
            "`{present}` must be a top-level command"
        );
    }
}

#[test]
fn auth_owns_the_identity_verbs() {
    Cli::command().debug_assert();
    assert!(Cli::try_parse_from(["hanzo", "auth", "login"]).is_ok());
    assert!(Cli::try_parse_from(["hanzo", "auth", "logout"]).is_ok());
    assert!(Cli::try_parse_from(["hanzo", "auth", "show"]).is_ok());
    assert!(Cli::try_parse_from(["hanzo", "auth", "list"]).is_ok());
    assert!(Cli::try_parse_from(["hanzo", "auth", "use", "admin/z"]).is_ok());
    assert!(Cli::try_parse_from(["hanzo", "auth", "switch", "carbon-district"]).is_ok());
    assert!(
        Cli::try_parse_from(["hanzo", "auth", "switch", "hanzo/james", "--org", "acme"]).is_ok()
    );
    assert!(Cli::try_parse_from(["hanzo", "auth", "switch", "--clear-org"]).is_ok());
    assert!(Cli::try_parse_from(["hanzo", "switch", "carbon-district"]).is_ok());
    assert!(Cli::try_parse_from(["hanzo", "switch", "--org", "carbon-district"]).is_ok());
    assert!(Cli::try_parse_from(["hanzo", "switch", "--clear-org"]).is_ok());
    assert!(
        Cli::try_parse_from(["hanzo", "switch", "--org", "acme", "--clear-org"]).is_err()
    );
    assert!(Cli::try_parse_from(["hanzo", "auth", "token"]).is_ok());
    assert!(Cli::try_parse_from(["hanzo", "auth", "show", "--org", "x"]).is_err());
}

#[test]
fn chain_and_leading_globals_and_vm_passthrough_parse() {
    assert!(Cli::try_parse_from(["hanzo", "chain", "status"]).is_ok());
    assert!(Cli::try_parse_from(["hanzo", "chain", "up", "testnet"]).is_ok());
    assert!(Cli::try_parse_from(["hanzo", "chain", "up"]).is_err());
    assert!(Cli::try_parse_from(["hanzo", "chain", "down"]).is_ok());
    let cli = Cli::try_parse_from(["hanzo", "fabric", "status"]).expect("alias parses");
    assert!(matches!(cli.command, Some(Commands::Chain { .. })));

    let merged = commands::product::augment(Cli::command());
    let parse = |line: &str| {
        let argv = std::iter::once("hanzo")
            .chain(line.split(' '))
            .map(String::from)
            .collect();
        merged
            .clone()
            .try_get_matches_from(hoist(&merged, argv))
            .unwrap()
    };
    let path = |m: &clap::ArgMatches| {
        let mut out = Vec::new();
        let mut at = m;
        while let Some((name, sub)) = at.subcommand() {
            out.push(name.to_string());
            at = sub;
        }
        out
    };
    let m = parse("--as admin auth login");
    assert_eq!(path(&m), ["auth", "login"]);
    assert_eq!(
        m.get_one::<String>("org").map(String::as_str),
        Some("admin")
    );
    assert_eq!(path(&parse("--as admin link status")), ["link", "status"]);
    let m = parse("--as admin claude status");
    assert!(path(&m).is_empty());

    let cli =
        Cli::try_parse_from(["hanzo", "vm", "run", "--cpus", "2", "echo", "hi"]).expect("vm parses");
    let Some(Commands::Vm { args }) = cli.command else {
        panic!("expected vm")
    };
    assert_eq!(args, ["run", "--cpus", "2", "echo", "hi"]);

    let cli = Cli::try_parse_from(["hanzo", "vm", "--help"]).expect("--help passes through");
    let Some(Commands::Vm { args }) = cli.command else {
        panic!("expected vm")
    };
    assert_eq!(args, ["--help"]);
}

#[test]
fn link_speaks_the_network_from_this_machine() {
    let parse = |argv: &[&str]| Cli::try_parse_from(argv).map(|c| c.command);
    assert!(parse(&["hanzo", "link", "--shell", "bash", "--read-only"]).is_ok());
    assert!(parse(&[
        "hanzo",
        "link",
        "host",
        "engine",
        "127.0.0.1:1235",
        "--token-command",
        "/etc/hanzo/link/token",
    ])
    .is_ok());
    assert!(parse(&["hanzo", "link", "dial", "k8s.hanzo", "26443", "--install"]).is_ok());
    assert!(parse(&["hanzo", "link", "status"]).is_ok());
    assert!(parse(&["hanzo", "link", "rm", "engine"]).is_ok());
    assert!(parse(&["hanzo", "link", "grant", "/home/me/site"]).is_ok());
    assert!(parse(&["hanzo", "link", "revoke", "site"]).is_ok());
    assert!(parse(&["hanzo", "link", "folders"]).is_ok());

    for bad in [
        &["hanzo", "link", "host", "engine"][..],
        &["hanzo", "link", "host", "--system"],
        &["hanzo", "link", "dial", "k8s.hanzo", "0"],
        &["hanzo", "link", "dial", "k8s.hanzo"],
        &["hanzo", "link", "--shell", "bash", "status"],
        &["hanzo", "link", "grant"],
    ] {
        assert!(parse(bad).is_err(), "{bad:?} parsed");
    }
    assert!(Cli::command().find_subcommand("net").is_none());
}

#[test]
fn broken_pipe_detection() {
    assert!(is_broken_pipe(
        &"failed printing to stdout: Broken pipe (os error 32)".to_string()
    ));
    assert!(is_broken_pipe(
        &"failed printing to stderr: Broken pipe (os error 32)".to_string()
    ));
    assert!(!is_broken_pipe(&"index out of bounds".to_string()));
}
