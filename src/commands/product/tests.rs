//! The generated tree is DATA; these tests pin the fold's totality, the trust
//! boundary, the scope-elision, and that resolve/dispatch never leave the seam.

use super::*;
use clap::Command;

// ---- trust boundary: the committed data is host/url/auth-free ----------------

/// The whole point of build-time codegen: the data can shape a call but can
/// never redirect it. If a host, an absolute URL or an auth token ever appears
/// in the generated file, the build fails here rather than shipping a redirect.
#[test]
fn generated_data_carries_no_host_url_or_auth() {
    let src = include_str!("generated.rs");
    for banned in ["http", "://", "Bearer", "Authorization", ".hanzo.", "hanzo.ai"] {
        assert!(
            !src.contains(banned),
            "generated data must be host/url/auth-free; found {banned:?}"
        );
    }
}

// ---- the fold is total: every op fills to a concrete path --------------------

/// The generator and `fill_path` must agree on which templated segment is the
/// tenant scope (a param preceded by `orgs`) and which are positionals. This
/// pins that contract: `params.len()` equals the non-scope templated segments,
/// and filling with an owner + dummy positionals leaves no `{}` behind.
#[test]
fn every_op_fills_to_a_path() {
    for op in OPS {
        let templated =
            op.path.split('/').filter(|s| s.starts_with('{') && s.ends_with('}')).count();
        let scope = scope_count(op.path);
        assert_eq!(
            op.params.len(),
            templated - scope,
            "params must equal the non-scope templated segments: {}",
            op.path
        );
        let values: Vec<String> = op.params.iter().map(|p| format!("v-{p}")).collect();
        let filled = fill_path(op.path, Some("acme"), &values).expect("fills");
        assert!(!filled.contains('{') && !filled.contains('}'), "unfilled: {filled}");
        if scope > 0 {
            assert!(filled.contains("/orgs/acme"), "scope must bind owner: {filled}");
            // ...and a signed-out caller is refused rather than sending a blank org.
            assert!(fill_path(op.path, None, &values).is_err(), "signed-out scope must refuse");
        }
    }
}

fn scope_count(path: &str) -> usize {
    let segs: Vec<&str> = path.split('/').collect();
    segs.iter()
        .enumerate()
        .filter(|(i, s)| {
            s.starts_with('{') && s.ends_with('}') && *i > 0 && segs[i - 1] == "orgs"
        })
        .count()
}

/// A coordinate `(product, nodes, verb)` is unique — the fold resolved every
/// collision at generation time (proven 0 unresolvable), so the runtime tree has
/// no ambiguous leaf.
#[test]
fn no_two_ops_share_a_coordinate() {
    let mut seen = std::collections::HashSet::new();
    for op in OPS {
        assert!(
            seen.insert((op.product, op.nodes, op.verb)),
            "duplicate coordinate: {} {:?} {}",
            op.product,
            op.nodes,
            op.verb
        );
    }
}

/// A leaf's positionals are unique (clap requires unique arg ids) and never
/// collide with the shared shape-only controls.
#[test]
fn op_params_are_unique_and_not_reserved() {
    for op in OPS {
        let mut seen = std::collections::HashSet::new();
        for p in op.params {
            assert!(seen.insert(*p), "duplicate positional {p} in {}", op.path);
            assert!(
                !["data", "query", "raw", "method", "subpath"].contains(p),
                "positional {p} collides with a reserved control in {}",
                op.path
            );
        }
    }
}

// ---- scope elision: the CLI never asks for (or sends) an org -----------------

/// The tenant org is ADDRESSED from the active identity's `owner`, never a
/// positional and never a flag. This is the invariant that keeps "the CLI never
/// sends an org" true across every scoped route.
#[test]
fn the_org_scope_is_bound_from_owner_never_asked() {
    let scoped: Vec<&Op> = OPS.iter().filter(|o| scope_count(o.path) > 0).collect();
    assert!(!scoped.is_empty(), "there must be at least one org-scoped route");
    for op in scoped {
        assert!(
            !op.params.contains(&"org"),
            "the org scope must never be a user positional: {}",
            op.path
        );
        let values: Vec<String> = op.params.iter().map(|p| format!("v-{p}")).collect();
        let filled = fill_path(op.path, Some("myorg"), &values).unwrap();
        assert!(filled.contains("/orgs/myorg"), "org bound from owner: {filled}");
    }
}

/// No generated leaf anywhere exposes an `--org` (or `--project`) flag — scope is
/// derived, not chosen. A build of the whole product tree carries no such arg.
#[test]
fn no_generated_leaf_has_an_org_flag() {
    fn walk(c: &Command) {
        for a in c.get_arguments() {
            let id = a.get_id().as_str();
            assert!(id != "org" || a.is_positional(), "an --org flag leaked in");
            assert_ne!(a.get_long(), Some("org"), "no --org flag on {}", c.get_name());
        }
        for s in c.get_subcommands() {
            walk(s);
        }
    }
    let cmd = augment(Command::new("hanzo"));
    walk(&cmd);
}

// ---- resolve: a parse becomes a call, through the tree -----------------------

fn matches_of(argv: &[&str]) -> clap::ArgMatches {
    augment(Command::new("hanzo")).try_get_matches_from(argv).expect("parses")
}

#[test]
fn a_simple_leaf_resolves_and_fills() {
    let m = matches_of(&["hanzo", "agents", "sessions", "get", "sess_1"]);
    let Some(Resolved::Leaf { op, values, .. }) = resolve(&m) else {
        panic!("expected a leaf");
    };
    assert_eq!(op.path, "/v1/agents/sessions/{id}");
    assert_eq!(op.method, "GET");
    assert_eq!(values, vec!["sess_1"]);
    assert_eq!(fill_path(op.path, Some("acme"), &values).unwrap(), "/v1/agents/sessions/sess_1");
}

#[test]
fn a_write_leaf_carries_data_and_the_org_is_not_asked() {
    // `hanzo agents sessions create --data '{"a":1}'`
    let m = matches_of(&["hanzo", "agents", "sessions", "create", "--data", "{\"a\":1}"]);
    let Some(Resolved::Leaf { op, data, .. }) = resolve(&m) else {
        panic!("expected a leaf");
    };
    assert_eq!(op.method, "POST");
    assert_eq!(data.as_deref(), Some("{\"a\":1}"));
}

/// The deep-nested case the naive case-tables broke on: arbitrary depth resolves
/// to the right op and fills every positional in order.
#[test]
fn a_deep_nested_leaf_resolves_and_fills_in_order() {
    let m = matches_of(&[
        "hanzo", "platform", "projects", "apps", "deployments", "logs", "p1", "a1", "d1",
    ]);
    let Some(Resolved::Leaf { op, values, .. }) = resolve(&m) else {
        panic!("expected a leaf");
    };
    assert_eq!(op.path, "/v1/platform/projects/{project}/apps/{app}/deployments/{id}/logs");
    assert_eq!(values, vec!["p1", "a1", "d1"]);
    assert_eq!(
        fill_path(op.path, Some("acme"), &values).unwrap(),
        "/v1/platform/projects/p1/apps/a1/deployments/d1/logs"
    );
}

// ---- passthrough: pure catch-all products forward, never emit a broken tree --

#[test]
fn a_passthrough_product_forwards_a_subpath() {
    let m = matches_of(&["hanzo", "tasks", "queues/default", "-X", "POST", "--data", "{}"]);
    let Some(Resolved::Pass { product, subpath, method, .. }) = resolve(&m) else {
        panic!("expected a passthrough");
    };
    assert_eq!(product, "tasks");
    assert_eq!(method, "POST");
    assert_eq!(passthrough_path(product, subpath.as_deref()).unwrap(), "/v1/tasks/queues/default");
}

#[test]
fn a_bare_passthrough_hits_the_product_root() {
    assert_eq!(passthrough_path("tasks", None).unwrap(), "/v1/tasks");
    assert_eq!(passthrough_path("tasks", Some("")).unwrap(), "/v1/tasks");
}

#[test]
fn a_passthrough_refuses_traversal() {
    assert!(passthrough_path("tasks", Some("../billing/deposit")).is_err());
    assert!(passthrough_path("tasks", Some("a/./b")).is_err());
}

#[test]
fn passthrough_products_are_disjoint_from_generated_products() {
    for &p in PASSTHROUGH {
        assert!(!is_product(p), "{p} is both a generated product and a passthrough");
    }
}

// ---- collisions: a local command always wins its bare name -------------------

/// The hand-written products are omitted from the data outright.
#[test]
fn hand_written_products_are_not_generated() {
    for local in ["agent", "kms", "billing", "deploy"] {
        assert!(!is_product(local), "{local} must be hand-written, not generated");
        assert!(!PASSTHROUGH.contains(&local));
    }
}

/// Defense in depth: if the derive tree already owns a name that a FUTURE spec
/// turns into a product, the local command still wins — augment skips it.
#[test]
fn augment_never_clobbers_an_existing_command() {
    // `world` IS a generated product; pin that a same-named local wins.
    assert!(is_product("world"), "precondition: world is a product");
    let base = Command::new("hanzo")
        .subcommand(Command::new("world").about("LOCAL-MARKER"));
    let merged = augment(base);
    let world = merged.find_subcommand("world").expect("world present");
    assert_eq!(world.get_about().map(|s| s.to_string()).as_deref(), Some("LOCAL-MARKER"));
    // exactly one `world`, and it is the local.
    assert_eq!(merged.get_subcommands().filter(|s| s.get_name() == "world").count(), 1);
}

// ---- `code`: the wrapper keeps its bare name; verbs mount under it -----------

/// A stub that mimics the derive `code` command: an optional positional and no
/// required subcommand, so both `code "task"` and `code <verb>` can parse.
fn code_base() -> Command {
    Command::new("hanzo").subcommand(
        Command::new("code")
            .arg(clap::Arg::new("task").required(false))
            .about("WRAPPER"),
    )
}

#[test]
fn a_code_verb_resolves_to_the_generated_leaf() {
    let m = augment(code_base())
        .try_get_matches_from(["hanzo", "code", "search"])
        .expect("parses");
    let Some(Resolved::Leaf { op, .. }) = resolve(&m) else {
        panic!("expected a code leaf");
    };
    assert_eq!(op.product, "code");
    assert_eq!(op.verb, "search");
    assert_eq!(op.path, "/v1/code/search");
}

#[test]
fn bare_code_and_a_task_stay_the_wrapper() {
    // bare `hanzo code` -> the wrapper (resolve declines).
    let m = augment(code_base()).try_get_matches_from(["hanzo", "code"]).expect("parses");
    assert!(resolve(&m).is_none(), "bare code is the wrapper, not a cloud verb");

    // `hanzo code "do a thing"` -> a task for the wrapper (not a subcommand).
    let m = augment(code_base())
        .try_get_matches_from(["hanzo", "code", "do a thing"])
        .expect("parses");
    assert!(resolve(&m).is_none(), "a free-text task is the wrapper");
}
