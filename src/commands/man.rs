//! The root man page — what a bare `hanzo`, `hanzo --help` or `hanzo help`
//! prints. The man-page form: NAME / SYNOPSIS / DESCRIPTION / GLOBAL FLAGS /
//! GROUPS / COMMANDS, each group on one line.
//!
//! EVERY name on this page is read off the PARSER — the hand-written commands
//! from `cmd` itself, the generated products from `product::catalog`, which reads
//! the same `cmd`. There is no table of commands here, for the same reason there
//! is no second table of products: a page that keeps its own list can name a
//! command the parser does not have, and an unrecognized first word is read as a
//! TASK, so the reader who types it gets a coding session about their own words
//! instead of an error. Both halves of that defect had shipped — `agent run` and
//! `connector` named nothing, `serve` had been renamed `up`, `billing` was listed
//! twice because it is a product now, and `up` and `link` appeared nowhere.

use colored::Colorize;

use crate::commands::product;

/// A command's name beside its own clap `about` line.
type Entry<'a> = (&'a str, String);

/// The hand-written commands, split the way the page presents them: a GROUP takes
/// subcommands, a COMMAND is terminal. Prose is each command's own clap `about`,
/// so a command states what it does in exactly one place.
fn hand(cmd: &clap::Command) -> (Vec<Entry<'_>>, Vec<Entry<'_>>) {
    let (mut groups, mut commands) = (Vec::new(), Vec::new());
    for sub in cmd.get_subcommands() {
        let name = sub.get_name();
        let entry = (name, sub.get_about().map(ToString::to_string).unwrap_or_default());
        // clap's `help` builtin mirrors the whole tree beneath itself so that
        // `hanzo help <command>` reaches every page. That mirror is not a product
        // group; to a reader `help` is one terminal command.
        if sub.has_subcommands() && name != "help" {
            groups.push(entry);
        } else {
            commands.push(entry);
        }
    }
    (groups, commands)
}

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
    // Read a BUILT tree: clap materializes its own `help` subcommand at build
    // time, so an unbuilt one is missing a command the binary really answers to.
    // Built once and read by both halves below — two readings is how a page comes
    // to disagree with its parser in the first place.
    let mut cmd = cmd.clone();
    cmd.build();
    let cmd = &cmd;

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
         \x20   (`hanzo code` for the interactive form). Sign in with `hanzo auth\n\
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

    let (mut groups, mut commands) = hand(cmd);

    o.push_str(&format!("{}\n", b("GROUPS")));
    o.push_str(&format!("    {} is one of the following:\n\n", "GROUP".underline()));
    // Hand groups and generated products, one alphabetical list — the reader
    // does not care which half of the binary answers.
    groups.extend(product::catalog(cmd));
    groups.sort_by_key(|(n, _)| *n);
    for (name, about) in &groups {
        entry(&mut o, name, about);
    }

    o.push_str(&format!("{}\n", b("COMMANDS")));
    o.push_str(&format!("    {} is one of the following:\n\n", "COMMAND".underline()));
    commands.sort_by_key(|(n, _)| *n);
    for (name, about) in &commands {
        entry(&mut o, name, about);
    }
    o
}

#[cfg(test)]
mod tests {
    use clap::CommandFactory;

    /// EVERY command this page names must be one the parser ACCEPTS, and every
    /// command the parser accepts must be NAMED here. Both halves had failed at
    /// once, because the page kept its own tables: `agent run` and `connector`
    /// named nothing, `serve` had been renamed `up`, `billing` was printed twice
    /// once it became a generated product, and `up` and `link` were named nowhere.
    ///
    /// This is not a typo class. An unrecognized first word is read as a TASK, so
    /// `hanzo agent run` off this page did not fail — it started a coding session
    /// about the words "agent run" and reported nothing wrong.
    #[test]
    fn the_page_names_every_command_and_only_real_ones() {
        let hand = crate::Cli::command();
        let merged = crate::commands::product::augment(crate::Cli::command());
        let mut built = hand.clone();
        built.build();
        let (groups, commands) = super::hand(&built);
        // A name is real if either half of the binary answers to it: the built
        // hand tree (which is where clap materializes its own `help`), or the
        // generated products mounted beside it.
        let resolve = |n: &str| built.find_subcommand(n).or_else(|| merged.find_subcommand(n));

        // The tables: every entry parses, and every entry says what it does.
        for (name, about) in groups.iter().chain(commands.iter()) {
            assert!(
                resolve(name).is_some(),
                "the page names `hanzo {name}`, which the parser does not accept"
            );
            assert!(!about.is_empty(), "`hanzo {name}` is on the page with nothing to say");
        }

        // The other direction: a command absent from the page is a command nobody
        // can find. `hanzo up` and `hanzo link` both were.
        let named: Vec<&str> = groups.iter().chain(commands.iter()).map(|(n, _)| *n).collect();
        for sub in built.get_subcommands().map(clap::Command::get_name) {
            assert!(named.contains(&sub), "`hanzo {sub}` is a command the page never names");
        }

        // The page's own PROSE names commands too, and only this walks it. Scoped
        // to the text this file writes: a product summary is cloud's sentence, and
        // a command it misnames is fixed upstream, never here.
        let page = super::page(&hand);
        let prose = page.split("GLOBAL FLAGS").next().expect("the page opens with prose");
        for quoted in prose.split('`').skip(1).step_by(2) {
            let mut words = quoted.split_whitespace().peekable();
            if words.next() != Some("hanzo") {
                continue;
            }
            let mut at = None;
            for w in words {
                if w.starts_with('-') || w.starts_with('"') {
                    break;
                }
                at = match at {
                    None => resolve(w),
                    Some(c) => clap::Command::find_subcommand(c, w),
                };
                assert!(
                    at.is_some(),
                    "the page's prose names `{quoted}`, which the parser does not accept"
                );
            }
        }
    }
}
