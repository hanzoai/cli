//! Merged product-tree, code-index, channels, and build command tests.

use crate::*;
use clap::{CommandFactory, FromArgMatches, Parser};

#[test]
fn the_merged_command_tree_is_valid() {
    Cli::command().debug_assert();
    commands::product::augment(Cli::command()).debug_assert();
}

#[test]
fn help_flag_is_preserved() {
    let err = Cli::try_parse_from(["hanzo", "--help"])
        .err()
        .expect("`--help` exits via a clap error");
    assert_eq!(err.kind(), clap::error::ErrorKind::DisplayHelp);
}

#[test]
fn build_is_a_command_not_a_task() {
    let hand = Cli::command();
    let m = commands::product::augment(hand.clone())
        .try_get_matches_from([
            "hanzo",
            "build",
            "app",
            "-t",
            "ghcr.io/hanzoai/app:1.0.0",
            "--push",
        ])
        .expect("build parses");
    assert!(commands::product::resolve(&hand, &m).is_none());
    let Some(Commands::Build(args)) = Cli::from_arg_matches(&m).unwrap().command else {
        panic!("expected build")
    };
    assert_eq!(args.context, PathBuf::from("app"));
    assert_eq!(args.tags, ["ghcr.io/hanzoai/app:1.0.0"]);
    assert!(args.push);
}

#[test]
fn linked_ai_accounts_live_under_auth() {
    let merged = commands::product::augment(Cli::command());
    let auth = merged.find_subcommand("auth").expect("auth");
    let accounts = auth.find_subcommand("link").expect("auth link");
    for verb in ["list", "get", "rm", "route", "create", "devices", "usage"] {
        assert!(accounts.find_subcommand(verb).is_some(), "auth link {verb}");
    }
    let link = merged.find_subcommand("link").expect("link");
    assert!(link.find_subcommand("list").is_none());
    assert!(link.find_subcommand("route").is_none());
}

#[test]
fn a_generated_product_resolves_and_a_local_command_does_not() {
    let hand = Cli::command();
    let merged = commands::product::augment(hand.clone());
    let m = merged
        .clone()
        .try_get_matches_from(["hanzo", "agent", "list"])
        .unwrap();
    assert!(commands::product::resolve(&hand, &m).is_some());

    let m = merged.try_get_matches_from(["hanzo", "version"]).unwrap();
    assert!(commands::product::resolve(&hand, &m).is_none());
}

#[test]
fn code_is_the_index_and_dev_is_the_session() {
    let hand = Cli::command();
    let merged = commands::product::augment(hand.clone());
    for (argv, path) in [
        (["hanzo", "code", "search"].as_slice(), "/v1/code/search"),
        (["hanzo", "code", "index"].as_slice(), "/v1/code/index"),
        (["hanzo", "code", "context"].as_slice(), "/v1/code/context"),
        (["hanzo", "code", "file"].as_slice(), "/v1/code/file"),
        (["hanzo", "code", "tree"].as_slice(), "/v1/code/tree"),
    ] {
        let m = merged
            .clone()
            .try_get_matches_from(argv)
            .unwrap_or_else(|e| panic!("{argv:?} must parse: {e}"));
        let Some(commands::product::Resolved::Leaf { op, .. }) =
            commands::product::resolve(&hand, &m)
        else {
            panic!("{argv:?} must resolve to a cloud operation")
        };
        assert_eq!(op.path, path, "{argv:?}");
    }
}

#[test]
fn channels_and_connect_and_global_flags_parse_cleanly() {
    assert!(Cli::try_parse_from(["hanzo", "channels"]).is_ok());
    assert!(Cli::try_parse_from(["hanzo", "channels", "--group", "Hanzo AI", "--json"]).is_ok());
    assert!(Cli::try_parse_from(["hanzo", "connect", "x", "--open"]).is_ok());
    assert!(Cli::try_parse_from(["hanzo", "post", "list", "--status", "scheduled"]).is_ok());

    let hand = Cli::command();
    let merged = commands::product::augment(hand.clone());
    let m = merged
        .try_get_matches_from(["hanzo", "billing", "--config", "/tmp/x", "invoices", "list"])
        .expect("a global flag is valid before an absorbed subcommand");
    assert!(commands::product::resolve(&hand, &m).is_some());
}

#[test]
fn dev_serve_local_bot_and_sandbox_computer_parse_while_bot_absorbs_cloud_ops() {
    let hand = Cli::command();
    let merged = commands::product::augment(hand.clone());

    let m = merged
        .clone()
        .try_get_matches_from(["hanzo", "dev", "--serve", "--port", "5173"])
        .expect("dev --serve parses");
    assert!(commands::product::resolve(&hand, &m).is_none());
    let Some(Commands::Dev(args)) = Cli::from_arg_matches(&m).unwrap().command else {
        panic!("expected dev")
    };
    assert!(args.remote.serve);
    assert_eq!(args.remote.port, 5173);

    let bot_sub = merged.find_subcommand("bot").expect("bot exists");
    for cloud_sub in ["members", "runs"] {
        assert!(bot_sub.find_subcommand(cloud_sub).is_some());
    }
    assert!(Cli::try_parse_from(["hanzo", "sandbox", "exec", "--", "uname", "-a"]).is_ok());
    assert!(
        Cli::try_parse_from(["hanzo", "sandbox", "computer", "http://127.0.0.1:5173"]).is_ok()
    );
}
