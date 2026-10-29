//! FRIENDLY ALIASES — `hanzo git issues|labels|milestones|projects …`.
//!
//! Forge issues, labels and milestones are served by cloud under `/v1/task`
//! (apps/task: a board IS a forge repository, a work item IS a forge issue), so
//! the generated tree already carries them as `hanzo task projects issues …`.
//! People and agents look for them under `hanzo git`, find nothing, and conclude
//! the capability does not exist. This table mounts the SAME generated ops a
//! second time under `git` — no new op, no path, no host, no auth: each entry
//! names an existing generated coordinate and [`leaf_named`] renders it
//! verbatim. An entry whose target is not in the generated table (labels and
//! milestones until cloud serves them and `spec/cloud.json` is regenerated) is
//! simply not mounted, so nothing here can advertise a command that 404s.

use clap::Command;
use serde_json::Value;

use super::{find_op, leaf_named, LeafBody, Op, Resolved};

/// One alias: `git <group> <verb>` → the generated op at `target`, optionally
/// with a body field PRESET (`close` is `update` with `status=done`).
pub(super) struct Alias {
    pub group: &'static str,
    pub verb: &'static str,
    pub target: &'static [&'static str],
    pub preset: Option<(&'static str, &'static str)>,
}

pub(super) const ALIASES: &[Alias] = &[
    Alias { group: "issues", verb: "list", target: &["task", "projects", "issues", "list"], preset: None },
    Alias { group: "issues", verb: "get", target: &["task", "projects", "issues", "get"], preset: None },
    Alias { group: "issues", verb: "create", target: &["task", "projects", "issues", "create"], preset: None },
    Alias { group: "issues", verb: "edit", target: &["task", "projects", "issues", "update"], preset: None },
    Alias { group: "issues", verb: "close", target: &["task", "projects", "issues", "update"], preset: Some(("status", "done")) },
    Alias { group: "issues", verb: "comment", target: &["task", "projects", "issues", "update"], preset: None },
    Alias { group: "issues", verb: "comments", target: &["task", "projects", "issues", "comments"], preset: None },
    Alias { group: "issues", verb: "claim", target: &["task", "projects", "issues", "claim"], preset: None },
    Alias { group: "projects", verb: "list", target: &["task", "projects", "list"], preset: None },
    Alias { group: "projects", verb: "get", target: &["task", "projects", "get"], preset: None },
    Alias { group: "labels", verb: "list", target: &["task", "projects", "labels", "list"], preset: None },
    Alias { group: "labels", verb: "create", target: &["task", "projects", "labels", "create"], preset: None },
    Alias { group: "labels", verb: "edit", target: &["task", "projects", "labels", "update"], preset: None },
    Alias { group: "milestones", verb: "list", target: &["task", "projects", "milestones", "list"], preset: None },
    Alias { group: "milestones", verb: "create", target: &["task", "projects", "milestones", "create"], preset: None },
    Alias { group: "milestones", verb: "edit", target: &["task", "projects", "milestones", "update"], preset: None },
    Alias { group: "milestones", verb: "close", target: &["task", "projects", "milestones", "update"], preset: Some(("state", "closed")) },
];

const ABOUT: &[(&str, &str)] = &[
    ("issues", "Forge issues (served by /v1/task; a board is a repository)"),
    ("projects", "Boards: the org's forge repositories as work boards"),
    ("labels", "A repository's labels on the forge"),
    ("milestones", "A repository's milestones on the forge"),
];

fn target(a: &Alias) -> Option<&'static Op> {
    find_op(a.target)
}

/// Mount every alias whose target exists under `git`. A group with no live
/// target is not mounted at all.
pub(super) fn mount(cmd: Command) -> Command {
    if cmd.find_subcommand("git").is_none() {
        return cmd;
    }
    cmd.mut_subcommand("git", |mut git| {
        for &(group, about) in ABOUT {
            if git.find_subcommand(group).is_some() {
                continue; // the collision law: whoever was there first keeps the name
            }
            let mut g = Command::new(group).about(about).subcommand_required(true).arg_required_else_help(true);
            let mut any = false;
            for a in ALIASES.iter().filter(|a| a.group == group) {
                if let Some(op) = target(a) {
                    let mut leaf = leaf_named(a.verb, op);
                    if let Some((k, v)) = a.preset {
                        leaf = leaf.about(format!("{} (sends {k}={v})", op.sum));
                    }
                    g = g.subcommand(leaf);
                    any = true;
                }
            }
            if any {
                git = git.subcommand(g);
            }
        }
        git
    })
}

/// The alias a matched chain names, if any.
pub(super) fn find(chain: &[&str]) -> Option<(&'static Alias, &'static Op)> {
    match chain {
        ["git", group, verb] => ALIASES
            .iter()
            .find(|a| a.group == *group && a.verb == *verb)
            .and_then(|a| target(a).map(|op| (a, op))),
        _ => None,
    }
}

/// Apply an alias's preset to a resolved leaf's body.
pub(super) fn preset(a: &Alias, r: Resolved) -> Resolved {
    let Some((k, v)) = a.preset else { return r };
    let Resolved::Leaf { op, values, body, query } = r;
    let body = match body {
        LeafBody::Typed(Value::Object(mut m)) => {
            m.insert(k.to_string(), Value::String(v.to_string()));
            LeafBody::Typed(Value::Object(m))
        }
        other => other,
    };
    Resolved::Leaf { op, values, body, query }
}
