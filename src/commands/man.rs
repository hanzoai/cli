//! The root man page — what a bare `hanzo`, `hanzo --help` or `hanzo help`
//! prints. The man-page form: NAME / SYNOPSIS / DESCRIPTION / GLOBAL FLAGS /
//! GROUPS / COMMANDS, each group on one line. GROUPS is composed from the SAME
//! generated data the parser uses (`product::catalog`) plus the hand-written
//! resource groups — never a second table of products.

use colored::Colorize;

use crate::commands::product;

/// The hand-written resource GROUPS (they take subcommands), with the same
/// one-line prose their clap definitions carry.
const GROUPS: &[(&str, &str)] = &[
    ("auth", "Manage identities and credentials"),
    ("billing", "Prepaid wallet money: read the balance, mint a deposit"),
    ("config", "Manage local CLI settings"),
    ("connector", "Connect an external provider account to your org"),
    ("engine", "Run AI engines on this machine"),
    ("fabric", "Run / join hanzo.network (the L1 fabric) and query its cluster"),
    ("host", "The local cloud host: every cloud command, served from a checkout"),
    ("network", "Network selection + custom/sovereign networks"),
    ("runner", "Provide this machine as a CI runner"),
    ("wallet", "Wallet identity: PQ cloud custody (KMS/MPC) or local keychain"),
];

/// The terminal COMMANDS (they take no subcommand).
const COMMANDS: &[(&str, &str)] = &[
    (
        "code",
        "Start a coding session on our own `dev` agent — or name another: \
         `code claude` / `code codex` (also `--dev` / `--claude` / `--codex`). \
         A trailing task runs headless",
    ),
    ("dev", "Start a coding session on the `dev` backend: `hanzo code dev`, spelled shorter"),
    ("desktop", "Point an agent at the desktop and browser instead of the repo"),
    ("init", "Initialize a new Hanzo project"),
    ("scan", "Scan local files for exposed secrets"),
    ("serve", "Run a Hanzo service: `cloud` for the whole API, or one service"),
    ("share", "Publish a local service to a public share.hanzo.ai URL"),
    ("version", "Print the CLI version"),
    ("help", "Print this page, or a subcommand's own help"),
];

fn b(s: &str) -> String {
    s.bold().to_string()
}

/// One indented, wrapped entry: name on its own line, prose under it — the
/// same form for every entry. Prose is wrapped at one width throughout.
fn entry(out: &mut String, name: &str, about: &str) {
    out.push_str(&format!("     {}\n", b(name)));
    let mut line = String::from("       ");
    for w in about.split_whitespace() {
        if line.len() + w.len() + 1 > 78 {
            out.push_str(line.trim_end());
            out.push('\n');
            line = String::from("       ");
        }
        line.push_str(w);
        line.push(' ');
    }
    let tail = line.trim_end();
    if !tail.is_empty() {
        out.push_str(tail);
        // Every group line ends with a period; a truncation ellipsis is
        // already terminal punctuation.
        if !tail.ends_with('.') && !tail.ends_with('\u{2026}') {
            out.push('.');
        }
        out.push('\n');
    }
    out.push('\n');
}

/// Render the page. Composed live so the GROUPS section is always exactly the
/// commands the parser accepts — `cmd` is the hand-written derive tree, the same
/// value `product::augment` is given, so `product::catalog` drops exactly the
/// generated products augmentation drops.
pub fn page(cmd: &clap::Command) -> String {
    let mut o = String::with_capacity(16 * 1024);

    o.push_str(&format!("{}\n", b("NAME")));
    o.push_str("    hanzo - manage Hanzo AI cloud resources and developer workflow\n\n");

    o.push_str(&format!("{}\n", b("SYNOPSIS")));
    o.push_str(&format!(
        "    {} {} | {} [{}] [--config=FILE] [--verbose] [--help]\n",
        b("hanzo"),
        "GROUP".underline(),
        "COMMAND".underline(),
        "FLAGS".underline()
    ));
    o.push_str(&format!(
        "    {} [{}] [{}] [-- PASSTHROUGH...]\n\n",
        b("hanzo"),
        "FLAGS".underline(),
        "TASK".underline()
    ));

    o.push_str(&format!("{}\n", b("DESCRIPTION")));
    o.push_str(
        "    The hanzo CLI manages authentication, billing, and every product of the\n\
         \x20   Hanzo AI cloud. Each GROUP below is one product; its subcommands are the\n\
         \x20   product's operations, generated from the same contract the API, SDKs and\n\
         \x20   MCP tools serve.\n\n\
         \x20   `hanzo \"fix the failing test\"` starts an AI coding session on the task\n\
         \x20   (`hanzo agent run` for the interactive form). Sign in with `hanzo auth\n\
         \x20   login`; see your money with `hanzo billing balance` and `hanzo usage`.\n\n",
    );

    o.push_str(&format!("{}\n", b("GLOBAL FLAGS")));
    for (f, d) in [
        ("--config=FILE", "Use a custom CLI config file for this invocation."),
        ("--verbose, -v", "Increase logging verbosity (repeat for more)."),
        ("--help", "Display this page, or a subcommand's detailed help."),
    ] {
        o.push_str(&format!("     {}\n        {}\n\n", b(f), d));
    }

    o.push_str(&format!("{}\n", b("GROUPS")));
    o.push_str(&format!("    {} is one of the following:\n\n", "GROUP".underline()));
    // Hand groups and generated products, one alphabetical list — the reader
    // does not care which half of the binary answers.
    let mut groups: Vec<(&str, String)> =
        GROUPS.iter().map(|(n, a)| (*n, (*a).to_string())).collect();
    for (name, about) in product::catalog(cmd) {
        groups.push((name, about));
    }
    groups.sort_by_key(|(n, _)| *n);
    for (name, about) in &groups {
        entry(&mut o, name, about);
    }

    o.push_str(&format!("{}\n", b("COMMANDS")));
    o.push_str(&format!("    {} is one of the following:\n\n", "COMMAND".underline()));
    for (name, about) in COMMANDS {
        entry(&mut o, name, about);
    }
    o
}
