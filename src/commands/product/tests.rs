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
    // A scheme (`://`), a host, or an auth token in CALL-BEARING data could
    // redirect a call. `sum` is display-only prose the runtime never dials, and a
    // doc comment may truthfully name a host ("imports into git.hanzo.ai") — so
    // the guard runs over the source with the sum strings blanked, then bans a
    // SCHEME even in prose: naming a host is a fact, carrying a link is a vector.
    let src = include_str!("generated.rs");
    let mut scrubbed = String::with_capacity(src.len());
    let mut in_products = false;
    for line in src.lines() {
        // PRODUCTS entries are prose too — a product's own doc may truthfully
        // name the URL shape it mints (share's `<token>.share.hanzo.ai`).
        if line.starts_with("pub(crate) static PRODUCTS") {
            in_products = true;
        }
        if in_products {
            if line == "];" {
                in_products = false;
            }
            scrubbed.push('\n');
            continue;
        }
        match line.find("sum: \"") {
            Some(i) => scrubbed.push_str(&line[..i]),
            None => scrubbed.push_str(line),
        }
        scrubbed.push('\n');
    }
    for banned in ["://", "Bearer", "Authorization", ".hanzo.", "hanzo.ai", "api.hanzo"] {
        assert!(
            !scrubbed.contains(banned),
            "generated call data must be host/url/auth-free; found {banned:?}"
        );
    }
    for op in OPS {
        assert!(!op.sum.contains("://"), "prose may name a host, never carry a link: {}", op.sum);
    }
    // A product description is display-only prose, but it still must not carry
    // an auth token — and the tuple shape means it can never be dialed.
    for (name, d) in PRODUCTS {
        assert!(!d.contains("Bearer") && !d.contains("Authorization"), "auth in {name} prose");
    }
    // Every path template is a bare `/v1/…` — no scheme can ride a path.
    for op in OPS {
        assert!(op.path.starts_with("/v1/") && !op.path.contains("://"), "bad path {}", op.path);
    }
}

// ---- prose is not optional ---------------------------------------------------

/// Every command states what it does for the person running it, and the route is
/// never allowed to stand in for that sentence.
///
/// `genproduct` already refuses to emit an undescribed op, so this is the pin
/// that keeps the refusal honest against a hand-edited tree: the two together
/// mean an empty `sum` cannot exist, which is why `leaf_named` has no branch for
/// one. The remedy for a failure here is never in this repo — the sentence is the
/// Go doc comment on the handler in hanzoai/cloud, lifted by zipdoc into the
/// published route table and joined in by `genspec`.
#[test]
fn every_command_says_what_it_does() {
    let bare: Vec<String> = OPS
        .iter()
        .filter(|o| o.sum.trim().is_empty())
        .map(|o| format!("  hanzo {} … {} <- {} {}", o.product, o.verb, o.method, o.path))
        .collect();
    assert!(
        bare.is_empty(),
        "{} command(s) have no prose:\n{}\nWrite the Go doc comment in hanzoai/cloud, \
         `make openapi`, then re-run genspec + genproduct.",
        bare.len(),
        bare.join("\n"),
    );
    // …and it is prose, not the implementation detail wearing prose's clothes. A
    // help line that IS the route is the exact failure this rules out; a summary
    // may still mention a route inside a sentence.
    for o in OPS {
        assert_ne!(
            o.sum.trim(),
            format!("{} {}", o.method, o.path),
            "`hanzo {} … {}` prints its route where its description belongs",
            o.product,
            o.verb
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
        let filled = fill_path(op.path, op.rest, Some("acme"), &values).expect("fills");
        assert!(!filled.contains('{') && !filled.contains('}'), "unfilled: {filled}");
        // `rest` is a subset of `params` (never names the tenant scope).
        for r in op.rest {
            assert!(op.params.contains(r), "rest {r} must be a positional: {}", op.path);
        }
        if scope > 0 {
            assert!(filled.contains("/orgs/acme"), "scope must bind owner: {filled}");
            // ...and a signed-out caller is refused rather than sending a blank org.
            assert!(
                fill_path(op.path, op.rest, None, &values).is_err(),
                "signed-out scope must refuse"
            );
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

/// The tenant scope mechanism: an `orgs/{org}` segment binds to `owner`, never a
/// positional. `kms` is the authored route that uses it (`kms secrets`), and the
/// loop below pins that no op ever leaks the scope as a positional or flag; this
/// also exercises `fill_path` directly on the shape.
#[test]
fn the_org_scope_is_bound_from_owner_never_asked() {
    // Template with a scope pair + one ordinary positional (kms's own shape).
    let t = "/v1/kms/orgs/{org}/secrets/{secret}";
    // `{org}` is filled from owner; only `{secret}` consumes a positional (and it
    // is the real multi-segment param, so pass its `rest` marker).
    let filled = fill_path(t, &["secret"], Some("acme"), &["DB".to_string()]).unwrap();
    assert_eq!(filled, "/v1/kms/orgs/acme/secrets/DB");
    // Signed out with a scope present → refuse rather than send a blank org.
    assert!(fill_path(t, &["secret"], None, &["DB".to_string()]).is_err());
    // No authored op leaks the org as a positional or a flag.
    for op in OPS {
        assert!(!op.params.contains(&"org") || scope_count(op.path) == 0);
        for f in op.fields {
            // A body field MAY legitimately be named `org` (the server re-checks
            // it); what must never exist is a scope-derived `--org`. None do.
            let _ = f;
        }
    }
}

/// The tenant SCOPE (`orgs/{org}`) is never a user-facing argument — it is bound
/// from `owner`. A NON-scope `org` (a git `{org}` path segment, or the admin
/// god-view's `org` query parameter for a SuperAdmin) is a legitimate parameter
/// and is allowed; only the scope pair must never surface as a positional or flag.
#[test]
fn the_org_scope_is_never_a_positional_or_flag() {
    for op in OPS {
        let segs: Vec<&str> = op.path.split('/').collect();
        for (i, s) in segs.iter().enumerate() {
            let scope = s.starts_with('{') && s.ends_with('}') && i > 0 && segs[i - 1] == "orgs";
            if !scope {
                continue;
            }
            let name = s.trim_start_matches('{').trim_end_matches('}');
            assert!(!op.params.contains(&name), "scope {name} leaked as a positional: {}", op.path);
            assert!(
                !op.fields.iter().any(|f| f.key == name),
                "scope {name} leaked as a flag: {}",
                op.path
            );
        }
    }
}

// ---- resolve: a parse becomes a call, through the tree -----------------------

/// The hand-written tree these parses are grafted onto. Empty here on purpose:
/// with no local command, nothing is absorbed, so every generated coordinate is
/// reachable at its own name and a test can address it without knowing which
/// names `main` happens to claim. The absorbed case is asserted where the real
/// derive tree is — `main::tests::a_local_command_absorbs_the_product_of_its_own_name`.
fn hand() -> Command {
    Command::new("hanzo")
}

fn matches_of(argv: &[&str]) -> clap::ArgMatches {
    augment(hand()).try_get_matches_from(argv).expect("parses")
}

#[test]
fn a_simple_leaf_resolves_and_fills() {
    let m = matches_of(&["hanzo", "agents", "sessions", "get", "sess_1"]);
    let Some(Resolved::Leaf { op, values, .. }) = resolve(&hand(), &m) else {
        panic!("expected a leaf");
    };
    assert_eq!(op.path, "/v1/agents/sessions/{id}");
    assert_eq!(op.method, "GET");
    assert_eq!(values, vec!["sess_1"]);
    assert_eq!(
        fill_path(op.path, op.rest, Some("acme"), &values).unwrap(),
        "/v1/agents/sessions/sess_1"
    );
}

/// THE headline: a write op with an authored schema takes TYPED flags, and the
/// JSON body is assembled from them at their schema types — never `--data`.
#[test]
fn a_typed_write_assembles_a_json_body_from_flags() {
    // `hanzo authz check --sub alice --obj doc:1 --act read`
    let m = matches_of(&["hanzo", "authz", "check", "--sub", "alice", "--obj", "doc:1", "--act", "read"]);
    let Some(Resolved::Leaf { op, body, .. }) = resolve(&hand(), &m) else {
        panic!("expected a leaf");
    };
    assert_eq!(op.method, "POST");
    assert_eq!(op.path, "/v1/authz/check");
    assert!(!op.fields.is_empty(), "authz check must be typed, not --data");
    let LeafBody::Typed(v) = body else { panic!("typed leaf must build a JSON body") };
    assert_eq!(v["sub"], "alice");
    assert_eq!(v["obj"], "doc:1");
    assert_eq!(v["act"], "read");
    // A typed leaf exposes NO `--data` — the flags ARE the body.
    assert!(matches_of(&["hanzo", "authz", "check", "--sub", "a", "--obj", "b", "--act", "c"])
        .subcommand()
        .is_some());
}

/// An INTEGER-typed flag reaches the body as a JSON number (not a string), and an
/// unset optional flag is OMITTED (the server's default stands), never sent null.
#[test]
fn a_typed_int_flag_is_a_json_number_and_optionals_are_omitted() {
    // Pick a typed op with a BODY int field (not a query param), no path params,
    // and NO required fields — so the only flag we pass is the int, and the body
    // holds exactly it.
    let op = OPS
        .iter()
        .find(|o| {
            o.params.is_empty()
                && o.fields.iter().any(|f| matches!(f.ty, Ty::Int) && !f.query)
                && o.fields.iter().all(|f| !f.required)
        })
        .expect("a typed op with a body int field and no required fields exists");
    let int = op.fields.iter().find(|f| matches!(f.ty, Ty::Int) && !f.query).unwrap();

    let mut argv = vec!["hanzo".to_string(), op.product.to_string()];
    argv.extend(op.nodes.iter().map(|n| n.to_string()));
    argv.push(op.verb.to_string());
    argv.push(format!("--{}", int.flag));
    argv.push("42".into());

    let m = augment(hand()).try_get_matches_from(&argv).expect("parses");
    let Some(Resolved::Leaf { body: LeafBody::Typed(v), .. }) = resolve(&hand(), &m) else {
        panic!("typed leaf");
    };
    assert_eq!(v[int.key], 42, "int flag must serialize as a JSON number");
    // Only the int we set is present — every other optional BODY field is omitted.
    assert_eq!(v.as_object().unwrap().len(), 1, "unset optionals must be omitted: {v}");
}

/// BUG-1 FIX: a collection GET is a runnable LEAF on a node that may also head a
/// group — `hanzo kv list` runs `GET /v1/kv` rather than demanding a subcommand.
///
/// The descent half of this test used to run `hanzo kv list push mykey` against
/// `/v1/kv/list/{key}/push`. Cloud serves exactly `/v1/kv` and `/v1/kv/{name}`, so
/// the whole datatype subtree was a 404 the authored spec invented, and the
/// registry refuted it. The resolver still makes such a node runnable — that is a
/// property of the tree builder, not of any one coordinate — but the served
/// surface no longer contains an example, and the CLI no longer offers the
/// commands that were never answerable.
#[test]
fn a_runnable_group_runs_its_collection_get_when_invoked_bare() {
    let m = matches_of(&["hanzo", "kv", "list"]);
    let Some(Resolved::Leaf { op, .. }) = resolve(&hand(), &m) else { panic!("expected a leaf") };
    assert_eq!(op.path, "/v1/kv");
    assert_eq!(op.method, "GET");

    assert!(
        !OPS.iter().any(|o| o.path.starts_with("/v1/kv/list")),
        "the unserved kv datatype subtree must stay refuted"
    );
}

/// BUG-2 FIX: an `in: query` parameter becomes a TYPED `--flag` that rides the
/// URL query (not the body), required-ness enforced by clap.
#[test]
fn a_query_param_becomes_a_typed_flag_in_the_url() {
    // WHICH op carries an optional query flag is the spec's answer, not this
    // test's — the same rule its second half already follows. Pinning
    // `o11y logs` is what broke it: cloud grew /v1/o11y/logs/{aggregate,
    // livetail,fields,pipelines}, so `logs` stopped being a verb and became a
    // node with `get` beneath it. The COORDINATE moved; the property never did.
    let target = OPS
        .iter()
        .find(|o| {
            o.method == "GET"
                && o.params.is_empty()
                && o.fields.iter().filter(|f| f.query && !f.required).count() >= 1
        })
        .expect("some GET takes an optional query parameter");
    let flag = target.fields.iter().find(|f| f.query && !f.required).unwrap();

    let mut argv: Vec<String> = vec!["hanzo".into(), target.product.into()];
    argv.extend(target.nodes.iter().map(|n| n.to_string()));
    argv.push(target.verb.into());
    argv.push(format!("--{}", flag.flag));
    argv.push("gateway".into());

    let m = augment(hand()).try_get_matches_from(&argv).expect("parses");
    let Some(Resolved::Leaf { op, body, query, .. }) = resolve(&hand(), &m) else { panic!("leaf") };
    assert_eq!(op.path, target.path);
    assert!(matches!(body, LeafBody::None), "a GET carries no body");
    assert!(
        query.contains(&format!("{}=gateway", flag.key)),
        "the flag must land in the URL: {query:?}",
    );
    // Required-ness rides through to clap. Which PARAMETERS are required is the
    // spec's answer, not this test's, so it takes whichever op carries a required
    // query flag rather than pinning one — pinning is how a test starts asserting
    // a snapshot instead of a property.
    let req = OPS
        .iter()
        .find(|o| o.params.is_empty() && o.fields.iter().any(|f| f.query && f.required))
        .expect("some op takes a required query parameter");
    let mut argv = vec!["hanzo".to_string(), req.product.to_string()];
    argv.extend(req.nodes.iter().map(|n| n.to_string()));
    argv.push(req.verb.to_string());
    assert!(
        augment(hand()).try_get_matches_from(&argv).is_err(),
        "`{}` must refuse to run without its required query flag",
        argv.join(" ")
    );
}

/// A top-level name means the product cloud serves at that name — there is no
/// second source of top-level names.
///
/// `hanzo logs` was a curated alias for `hanzo o11y logs` while nothing was mounted
/// at `/v1/logs`. Cloud serves `/v1/logs/{query,write,health}` now, so the name is
/// the product's, and the alias table — a hand-kept claim about the route surface,
/// the same shape of defect as a captured route table one layer up — is gone.
///
/// It had to go rather than "stand down": `augment` skipped the shadowed alias but
/// `resolve` still preferred it, so the parser MOUNTED the product and dispatch
/// sent it to the o11y op, and `hanzo logs query` PANICKED on an argument id its
/// own command never defined. Asserting the mount alone never saw that — the two
/// halves disagreed in the gap between them. So this walks all the way to the op.
#[test]
fn a_top_level_name_resolves_to_the_product_cloud_serves_there() {
    assert!(is_product("logs"), "cloud serves /v1/logs/* — `logs` is a product");
    let m = matches_of(&["hanzo", "logs", "query"]);
    let Some(Resolved::Leaf { op, .. }) = resolve(&hand(), &m) else { panic!("expected a leaf") };
    assert_eq!(op.path, "/v1/logs/query", "parse and dispatch must agree on a name");
    // The o11y op the alias pointed at is still reachable under its own product —
    // one capability, one place, never duplicated to keep a nickname alive.
    // Reached by PATH, because where it sits is the spec's to move: it was
    // `o11y logs` until cloud gave /v1/o11y/logs children, and it is `o11y logs
    // get` now. What must stay true is that it is reachable at all, and only
    // from its own product.
    let o11y = OPS
        .iter()
        .find(|o| o.path == "/v1/o11y/logs")
        .expect("/v1/o11y/logs is served under the o11y product");
    assert_eq!(o11y.product, "o11y", "it belongs to o11y and to nothing else");
    let mut argv: Vec<String> = vec!["hanzo".into(), o11y.product.into()];
    argv.extend(o11y.nodes.iter().map(|n| n.to_string()));
    argv.push(o11y.verb.into());
    let m = augment(hand()).try_get_matches_from(&argv).expect("o11y parses");
    let Some(Resolved::Leaf { op, .. }) = resolve(&hand(), &m) else { panic!("o11y leaf") };
    assert_eq!(op.path, "/v1/o11y/logs");
    assert!(
        !include_str!("mod.rs").contains("ALIASES"),
        "no second source of top-level names may come back"
    );
}

/// CURATION states facts about what a COMMAND LINE is, never about what the server
/// serves — that is the document's answer, made in `genspec`. Each denial here
/// survives because it would still hold if its route were served perfectly; the ones
/// that did not survive are now commands.
#[test]
fn curation_speaks_only_of_what_a_command_line_is() {
    // A command line is not a browser, and the document that decides the commands
    // is not one of them. `completions` names shell completion here — the operation
    // is reachable where it reads correctly, as `hanzo chat completions`.
    for client_fact in ["csrf", "openapi.json", "completions"] {
        assert!(!is_product(client_fact), "{client_fact} must be denied as a top-level command");
    }
    // …and what the document carries is a command, whatever a table used to say.
    // These were denied as "noise: sub-operations or enumeration artifacts", which
    // described the SHAPE of what cloud publishes for them (a relay door), not what
    // a person wants to type. Each has its own prose and 1–11 served operations.
    for served in ["download", "upload", "files", "indexers", "settings"] {
        assert!(is_product(served), "{served} is served and described — it must be a command");
    }
    // `gateway` exists exactly as far as the registry DESCRIBES it — no more, no
    // less. The 27-op authored subtree stayed refuted (dead routes never come
    // back), and the one route cloud genuinely serves and documents
    // (/v1/gateway/config, typed 2026-07-30) came in through the registry side of
    // genspec. (That every gateway op carries prose was once asserted here; it is
    // now the fleet-wide invariant `every_command_says_what_it_does` enforces.)
    let gw: Vec<_> = OPS.iter().filter(|o| o.path.starts_with("/v1/gateway")).collect();
    assert!(!gw.is_empty(), "the served /v1/gateway/config must surface");
    // The real gateway surface stays reachable at its TOP-LEVEL served paths.
    assert!(
        OPS.iter().any(|o| o.path == "/v1/models" && o.verb == "list"),
        "`hanzo models list` (GET /v1/models) must remain"
    );
    // The "redundant plural" dedupe was a claim about the SERVER, and the document
    // refutes it: `/v1/bots` is bot RUNS while `/v1/bot` is bot nodes plus a relay
    // door, and `/v1/networks` is the org's Zero Trust overlay while the local
    // `network` SELECTS one. Three surfaces, three names, no redundancy to remove.
    // (`agent` is a different case and stays denied: `/v1/agent` and `/v1/agents`
    // are two products on ONE noun, which is a collision, not a duplication.)
    for plural in ["networks", "bots"] {
        assert!(is_product(plural), "{plural} is its own served surface, not a dupe of a singular");
    }
    assert!(is_product("bot"), "…and the singular stays exactly as served");
    // The hand `cluster` proxy is deleted, so the SERVED clusters product is
    // the one way to reach dedicated clusters.
    assert!(is_product("clusters"), "`hanzo clusters` is the cloud product, no local shadow");
    assert!(!is_product("machines") && !is_product("gpus"), "absorbed into compute");
    assert!(is_product("compute"));
    assert!(
        OPS.iter().any(|o| o.product == "compute" && o.nodes == ["machines"] && o.verb == "list"),
        "machines list must be reachable as `compute machines list`"
    );
}

/// The deep-nested case the naive case-tables broke on: arbitrary depth resolves
/// to the right op and fills every positional in order (no scope here — `org` is
/// the literal `org`, not the `orgs/{org}` scope pair, so it stays a positional).
#[test]
fn a_deep_nested_leaf_resolves_and_fills_in_order() {
    let m = matches_of(&["hanzo", "platform", "projects", "apps", "get", "site", "web"]);
    let Some(Resolved::Leaf { op, values, .. }) = resolve(&hand(), &m) else {
        panic!("expected a leaf");
    };
    assert_eq!(op.path, "/v1/platform/projects/{project}/apps/{app}");
    assert_eq!(values, vec!["site", "web"]);
    assert_eq!(
        fill_path(op.path, op.rest, Some("acme"), &values).unwrap(),
        "/v1/platform/projects/site/apps/web"
    );
}

// ---- the whole cloud is subcommands: no `api`, no passthrough ----------------

/// There is NO raw-path escape. `hanzo api` does not exist, and no product falls
/// through to a passthrough — a matched top-level command is either a generated
/// product leaf or a local command, never a `<subpath>` forwarder.
#[test]
fn there_is_no_passthrough_or_raw_path_escape() {
    let src = include_str!("mod.rs");
    assert!(!src.contains("Resolved::Pass"), "no passthrough variant may remain");
    assert!(!src.contains("fn passthrough"), "no passthrough builder may remain");
    // The dispatcher speaks the seam directly, in-module (no `api` command).
    assert!(!src.contains("super::api"), "the seam moved in-module; no `api` command remains");
}

// ---- collisions: a local command always wins its bare name -------------------

/// A name a LOCAL command owns is ABSORBED, never dropped — the local command
/// keeps every verb it declares and gains every one the document does.
///
/// This test used to assert the opposite: that `billing` and `code` "must be
/// hand-written, not generated". That is how 25 served `/v1/billing` operations
/// and 7 `/v1/code` ones came to be reachable by nothing, with a curation entry
/// calling it a decision. Written as a PROPERTY over whatever collides, so the
/// next local command to share a cloud name is covered without an edit.
#[test]
fn a_local_name_absorbs_the_product_instead_of_dropping_it() {
    use clap::CommandFactory;
    let hand = crate::Cli::command();
    let merged = augment(crate::Cli::command());
    let mut absorbed = 0usize;
    for p in OPS.iter().map(|o| o.product).collect::<std::collections::BTreeSet<_>>() {
        let Some(local) = hand.find_subcommand(p) else { continue };
        absorbed += 1;
        let here = merged.find_subcommand(p).expect("a product mounts under some name");
        // Every operation of the product reaches a node under the local command…
        for op in OPS.iter().filter(|o| o.product == p) {
            let node = *op.nodes.first().unwrap_or(&op.verb);
            assert!(
                here.find_subcommand(node).is_some(),
                "`hanzo {p} {node}` must exist — otherwise {} {} reaches nobody",
                op.method,
                op.path
            );
        }
        // …and every name the local command declared is still its own.
        for name in local.get_subcommands().map(clap::Command::get_name) {
            assert!(
                here.find_subcommand(name).is_some(),
                "`hanzo {p} {name}` is the local command's and was overwritten"
            );
        }
    }
    assert!(absorbed > 0, "no product shares a local name — the collision law is untested");
}

/// A NAME MAY ONLY BE RESERVED BY A COMMAND THAT EXISTS.
///
/// `genproduct`'s DENY list withheld `deploy` and `agent` on the stated grounds
/// that "a LOCAL hand-written command owns these names". Both had been deleted —
/// `main.rs::old_top_level_verbs_are_removed` asserts `deploy` is not a top-level
/// command — so the reservation was held for nobody and the 21 served
/// `/v1/deploy/*` routes, the control plane behind the Hanzo CD console at
/// `/v1/cd`, reached no one at all.
///
/// This is the gate, and it is a PROPERTY rather than a list: for every generated
/// product, either it mounts, or the parser really does define that name. A future
/// DENY entry that claims a local owner it does not have fails here.
#[test]
fn a_reservation_must_name_a_command_that_exists() {
    use clap::CommandFactory;
    let hand = crate::Cli::command();
    let local: std::collections::HashSet<String> =
        hand.get_subcommands().map(|s| s.get_name().to_string()).collect();
    let merged = augment(crate::Cli::command());
    for p in OPS.iter().map(|o| o.product).collect::<std::collections::BTreeSet<_>>() {
        let mounted = merged.find_subcommand(p).is_some();
        assert!(
            mounted,
            "generated product `{p}` mounts under no name — nothing shadows it"
        );
        if !local.contains(p) {
            continue;
        }
        // Absorbed: the local command must genuinely exist, which `local` just
        // proved, and it is not advertised as a top-level product because it is
        // not one. Its operations are reachable inside it — see
        // [`a_local_name_absorbs_the_product_instead_of_dropping_it`].
        assert!(super::mounted(&hand).iter().all(|m| *m != p), "{p} is absorbed, not mounted");
    }
    // The reservation that had lost its owner: `deploy` is generated again, and it
    // is the CD control plane, not a wrapper.
    assert!(is_product("deploy"), "cloud serves /v1/deploy/* — `deploy` is a product");
    assert!(!local.contains("deploy"), "no local `deploy` command may reclaim the name silently");
    assert!(
        OPS.iter().any(|o| o.product == "deploy" && o.path == "/v1/deploy/applications"),
        "`hanzo deploy applications` must reach GET /v1/deploy/applications"
    );
}

/// The man page is composed from the SAME filter the parser mounts, so a product
/// a local command shadows can never be advertised as a group. `catalog` used to
/// read every product name in `OPS`, which printed `share` twice — once as the
/// hand command and once as the generated product `augment` had dropped.
#[test]
fn the_man_page_names_exactly_what_the_parser_mounts() {
    use clap::CommandFactory;
    let hand = crate::Cli::command();
    let advertised: Vec<&str> = super::catalog(&hand).into_iter().map(|(n, _)| n).collect();
    let merged = augment(crate::Cli::command());
    for n in &advertised {
        let sub = merged.find_subcommand(*n).expect("advertised group must parse");
        assert!(sub.has_subcommands(), "`{n}` is advertised as a GROUP; it must take subcommands");
    }
    let page = crate::commands::man::page(&hand);
    for local in hand.get_subcommands().map(clap::Command::get_name) {
        assert!(
            !advertised.contains(&local),
            "`{local}` is a local command; the generated tree must not also advertise it"
        );
    }
    assert!(page.contains("deploy"), "the CD control plane must appear on the page");
}

/// `kms` is a GENERATED product folded to EXACTLY the routes cloud mounts —
/// and that set is now the REGISTRY's answer, not a count kept here by hand.
/// `/v1/kms/{health,auth/login}` join `secrets {list,get,create,rm}` because
/// cloud's live route table serves them; nothing it cannot answer is invented,
/// in particular NO PATCH/`update` and NO `rotate` on the org-scoped secrets
/// plane. Add a kms route and this list is what must change.
#[test]
fn kms_is_generated_with_exactly_the_real_cloud_routes() {
    assert!(is_product("kms"), "kms must be generated now, not hand-written");
    let mut got: Vec<String> = OPS
        .iter()
        .filter(|o| o.product == "kms")
        .map(|o| format!("{} {:?} {}", o.method, o.nodes, o.verb))
        .collect();
    got.sort();
    let want = vec![
        r#"DELETE ["secrets"] rm"#.to_string(),
        r#"GET ["secrets"] get"#.to_string(),
        r#"GET ["secrets"] list"#.to_string(),
        r#"GET [] config"#.to_string(),
        r#"GET [] health"#.to_string(),
        r#"POST ["auth"] login"#.to_string(),
        r#"POST ["secrets"] create"#.to_string(),
    ];
    assert_eq!(got, want, "kms must fold to exactly the real cloud routes");
    // No unanswerable verb (PATCH/PUT → update/replace) and no rotate.
    for o in OPS.iter().filter(|o| o.product == "kms") {
        assert!(o.method != "PATCH" && o.method != "PUT", "cloud mounts no kms write besides POST");
        assert!(
            !matches!(o.verb, "update" | "rotate" | "replace" | "clear"),
            "kms must not surface a verb cloud cannot answer: {}",
            o.verb
        );
    }
}

/// THE invariant of a secrets CLI, now on the GENERATED path: the `value` is a
/// stdin-secret (`format: password`), so it has NO flag and NO positional. A
/// value-bearing argv is a PARSE ERROR — a property of the grammar, not the
/// handler's discipline — and `resolve` never sees the value (it is injected
/// from stdin only at dispatch).
#[test]
fn kms_secret_value_can_never_reach_argv() {
    // The `create` op carries a `value` field explicitly marked secret.
    let create = OPS.iter().find(|o| o.product == "kms" && o.verb == "create").expect("kms create");
    let value = create.fields.iter().find(|f| f.key == "value").expect("value field");
    assert!(value.secret, "the value field must be a stdin-secret");
    assert!(!value.query, "a secret is a body field, never a query param");

    // No flag or positional can carry it: every value-bearing argv is rejected.
    let base = || augment(Command::new("hanzo"));
    let leaky: &[&[&str]] = &[
        &["hanzo", "kms", "secrets", "create", "--name", "DB", "--env", "prod", "--value", "hunter2"],
        &["hanzo", "kms", "secrets", "create", "--name", "DB", "--env", "prod", "--secret", "hunter2"],
        &["hanzo", "kms", "secrets", "create", "--name", "DB", "--env", "prod", "hunter2"],
    ];
    for argv in leaky {
        assert!(base().try_get_matches_from(*argv).is_err(), "value-bearing argv must not parse: {argv:?}");
    }

    // What DOES parse carries only the address + env; the body OMITS the secret
    // (it is read from stdin at dispatch, never assembled from a flag).
    let m = matches_of(&["hanzo", "kms", "secrets", "create", "--name", "DB", "--env", "prod"]);
    let Some(Resolved::Leaf { op, body: LeafBody::Typed(v), .. }) = resolve(&hand(), &m) else {
        panic!("typed leaf");
    };
    assert_eq!(op.path, "/v1/kms/secrets");
    assert_eq!(v["name"], "DB");
    assert_eq!(v["env"], "prod");
    assert!(v.get("value").is_none(), "the secret must NOT be assembled from flags: {v}");

    // `--env` is required on the write (the server refuses to guess a default).
    assert!(
        base().try_get_matches_from(["hanzo", "kms", "secrets", "create", "--name", "DB"]).is_err(),
        "create must require --env"
    );

    // No `--org` on any kms verb — the org binds to the active identity's owner.
    let orged: &[&[&str]] = &[
        &["hanzo", "kms", "secrets", "list", "--org", "other"],
        &["hanzo", "kms", "secrets", "get", "DB", "--org", "other"],
        &["hanzo", "kms", "secrets", "rm", "DB", "--org", "other"],
    ];
    for argv in orged {
        assert!(base().try_get_matches_from(*argv).is_err(), "no --org may exist: {argv:?}");
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

// ---- the moved executor: method / body / url helpers -------------------------

#[test]
fn method_maps_from_the_op_string() {
    assert_eq!(parse_method("GET").unwrap(), reqwest::Method::GET);
    assert_eq!(parse_method("DELETE").unwrap(), reqwest::Method::DELETE);
    assert!(parse_method("CONNECT").is_err());
}

#[test]
fn a_data_body_on_a_read_is_a_named_error() {
    use reqwest::Method;
    assert!(read_body(Some("{}".into()), &Method::GET).is_err());
    assert!(read_body(Some("{}".into()), &Method::HEAD).is_err());
    assert!(read_body(Some(r#"{"a":1}"#.into()), &Method::POST).is_ok());
    assert!(read_body(None, &Method::GET).unwrap().is_none());
    assert!(read_body(Some("not json".into()), &Method::POST).is_err());
}

#[test]
fn query_pairs_are_appended_and_encoded() {
    let t = target("/v1/agents", &["env=prod".into()]).unwrap();
    assert_eq!(t, "/v1/agents?env=prod");
    // A value that looks like extra params is encoded, not injected.
    let t = target("/v1/x", &["q=a b&c=d".into()]).unwrap();
    assert!(t.contains("q=a+b%26c%3Dd"), "{t}");
    assert!(target("/v1/x", &["noeq".into()]).is_err());
    // No query means no trailing `?` — the target is the bare path, which is what
    // both wires expect.
    assert_eq!(target("/v1/agents", &[]).unwrap(), "/v1/agents");
}

// ---- KMS folder secrets: a multi-segment path fills to RAW slashes ------------

/// The write-only-folder-secret bug: `get`/`rm` percent-encoded the `/` in a
/// folder-scoped address (`prod/db → prod%2Fdb`) → the server 404'd. Their
/// terminal param is marked MULTI-SEGMENT, so the slashes ride raw and the catch-
/// all resolves. A FLAT name is unchanged, every segment is still encoded, and
/// `.`/`..`/empty are refused before a URL exists — so `create --path p` then
/// `get p/x` / `rm p/x` round-trips while `..` can never re-address another secret.
///
/// The MARKING is derived, not declared: cloud mounts this route as
/// `/v1/kms/secrets/*`, the registry reports the `*` as `{wildcard1}`, and
/// `genspec` records it on the operation. The client keeps no list of which
/// parameters are paths.
///
/// The ORG is not in this URL and cannot be: cloud reads it from the validated
/// principal (`clients/kms/mount.go` — "the org is the CALLER'S … never named in
/// the URL", because a path segment made the tenant caller-selectable). So no
/// address a caller can spell reaches another tenant, traversal or not; the
/// refusals below are about not re-addressing another SECRET.
#[test]
fn kms_folder_secret_path_round_trips_with_raw_slashes() {
    for verb in ["get", "rm"] {
        let op = OPS
            .iter()
            .find(|o| o.product == "kms" && o.verb == verb)
            .unwrap_or_else(|| panic!("kms {verb}"));
        assert_eq!(op.rest, op.params, "kms {verb} must mark its address param multi-segment");

        // A folder-scoped address keeps its slashes RAW (server: last seg = name).
        let filled =
            fill_path(op.path, op.rest, Some("acme"), &["prod/db/password".into()]).unwrap();
        assert_eq!(filled, "/v1/kms/secrets/prod/db/password");

        // A FLAT name (already working) is untouched — one segment, no slash.
        let flat = fill_path(op.path, op.rest, Some("acme"), &["DB".into()]).unwrap();
        assert_eq!(flat, "/v1/kms/secrets/DB");

        // Each segment is STILL percent-encoded: a space/`?` cannot re-address.
        let enc = fill_path(op.path, op.rest, Some("acme"), &["a b/x?y".into()]).unwrap();
        assert_eq!(enc, "/v1/kms/secrets/a%20b/x%3Fy");

        // The active org is handed in and does NOT appear: it is the token's, and
        // an org that cannot be spelled cannot be swapped.
        assert!(!filled.contains("acme"), "the org must not reach the URL: {filled}");

        // Traversal / empty segments are refused BEFORE a URL is built.
        for evil in ["../../evil/k", "a/../b", "a//b", "/leading", "trailing/", "."] {
            assert!(
                fill_path(op.path, op.rest, Some("acme"), &[evil.into()]).is_err(),
                "kms {verb} must refuse {evil:?}"
            );
        }
    }

    // A single-segment param (the default) still `%2F`-escapes a slash, so a value
    // can never split into extra segments and re-address a different route.
    let single =
        fill_path("/v1/agents/sessions/{id}", &[], Some("acme"), &["a/b".into()]).unwrap();
    assert_eq!(single, "/v1/agents/sessions/a%2Fb");
}

// ---- a 2xx with an error envelope is a failure, never a silent success --------

/// The silent-swallow bug: some planes (IAM) answer a refusal with HTTP
/// 200 and `{"status":"error","msg":…}`. `envelope_error` reads that as a failure
/// carrying the server's message (so `call` exits non-zero to stderr), while a
/// genuine success — a success envelope, a delete's null `data`, or a raw body —
/// stays `None` and renders exactly as before.
#[test]
fn a_2xx_error_envelope_is_surfaced_not_swallowed() {
    use serde_json::json;

    // The IAM shape: HTTP 200 body, but the envelope says error.
    assert_eq!(
        envelope_error(&json!({"status": "error", "msg": "Unauthorized operation"})).as_deref(),
        Some("Unauthorized operation")
    );
    // Case-insensitive status; a `data:null` alongside the error is still an error.
    assert_eq!(
        envelope_error(&json!({"status": "Error", "msg": "nope", "data": null})).as_deref(),
        Some("nope")
    );
    // A `failed` status with `error` (no `msg`) uses `error` as the message.
    assert_eq!(
        envelope_error(&json!({"status": "failed", "error": "boom"})).as_deref(),
        Some("boom")
    );
    // An error status with no message still fails (generic) — never silent.
    assert!(envelope_error(&json!({"status": "error"})).is_some());
    // A bare `{"error":…}` with no data is an error.
    assert_eq!(envelope_error(&json!({"error": "bad request"})).as_deref(), Some("bad request"));

    // GENUINE SUCCESS is untouched (None → renders normally):
    assert!(envelope_error(&json!({"status": "ok", "data": {"id": 1}})).is_none());
    // A success carrying a `msg` and a null `data` (e.g. a delete) is NOT an error
    // — the explicit non-error status wins.
    assert!(envelope_error(&json!({"status": "ok", "msg": "deleted", "data": null})).is_none());
    // A raw non-enveloped body (an array, or an object that IS the data) succeeds.
    assert!(envelope_error(&json!([1, 2, 3])).is_none());
    assert!(envelope_error(&json!({"id": 1, "name": "x"})).is_none());
    // An `error` string but real data present is not treated as a failure.
    assert!(envelope_error(&json!({"error": "warn", "data": {"ok": true}})).is_none());
    // Null / non-object bodies never error here.
    assert!(envelope_error(&serde_json::Value::Null).is_none());
    assert!(envelope_error(&json!("plain string")).is_none());
}

// ---- naming: the reader is a customer, not a maintainer ---------------------

/// The vendors this CLI's OWN help text may not name.
///
/// Not a blanket ban on the word: `hanzo auth login --provider openai` has to say
/// whose key the customer is pasting, and the module that drives a third-party
/// binary has to name the binary. What is banned is naming somebody else's product
/// in the copy a customer reads — as the STANDARD ("an OpenAI-compatible
/// endpoint" makes their API the real one and ours the imitation), as an ANALOGY
/// ("ngrok on our own fabric", "like `gh auth switch`" — an analogy where a
/// property belongs), or as a VALUE in a flag whose vocabulary is ours to choose
/// (`--backend-mode caddy`; the customer is picking "serve a directory", and
/// share.rs translates at the one place the value leaves for the helper).
const FOREIGN: &[&str] = &[
    "OpenAI-compatible", "Anthropic-compatible", "ChatGPT", "ngrok", "zrok",
    "caddy", "Caddy", "nginx", "PostHog", "Stripe", "Casbin", "Casdoor",
    "gh auth", "Prometheus", "Grafana", "Datadog", "Auth0", "Okta", "Vercel",
    "Heroku", "Cloudflare Workers", "LangChain", "Ollama", "HuggingFace",
];

/// Walk every `about` and `long_about` in the HAND-WRITTEN tree and refuse a
/// foreign name.
///
/// Scoped to the hand-written tree on purpose, and the scope is the honest part:
/// the generated half's prose is the Go doc comment on the handler in
/// hanzoai/cloud, lifted by zip into the published document. This repo carries it
/// faithfully and cannot fix it — a rename here would be a second source of truth
/// and the next `genproduct` run would erase it. The five foreign names in there
/// today (PostHog, Prometheus, Stripe-Atlas, Casbin, one Claude plan example) are
/// tracked where they can actually be changed.
#[test]
fn hand_written_help_names_no_outside_vendor() {
    use clap::CommandFactory;
    fn walk(cmd: &Command, path: &str, out: &mut Vec<(String, String)>) {
        for text in [cmd.get_about(), cmd.get_long_about()].into_iter().flatten() {
            out.push((path.to_string(), text.to_string()));
        }
        for arg in cmd.get_arguments() {
            for text in [arg.get_help(), arg.get_long_help()].into_iter().flatten() {
                out.push((format!("{path} --{}", arg.get_id()), text.to_string()));
            }
        }
        for sub in cmd.get_subcommands() {
            walk(sub, &format!("{path} {}", sub.get_name()), out);
        }
    }
    let mut copy = Vec::new();
    walk(&crate::Cli::command(), "hanzo", &mut copy);
    assert!(copy.len() > 100, "walked {} strings — the walk is broken", copy.len());

    let mut found = Vec::new();
    for (where_, text) in &copy {
        for v in FOREIGN {
            if text.contains(v) {
                found.push(format!("{where_}: {v:?} in {text:?}"));
            }
        }
    }
    assert!(found.is_empty(), "outside vendor named in customer-facing help:\n  {}", found.join("\n  "));
}

/// A command name says the thing; the METHOD-shaped word is the command's own
/// position, not part of the noun. `hanzo iam run-casbin-command` says the verb
/// twice and names an outside vendor once.
///
/// This reads the HAND-WRITTEN tree, for the same reason as above: a generated
/// name is a path segment, and the fix for a bad one is a route move in the
/// serving repo. hanzoai/iam moved nine of them and hanzoai/openapi's merge now
/// keeps the legacy spellings out of the document; the rest follow their own
/// repos.
#[test]
fn hand_written_command_names_are_not_verb_nouns() {
    use clap::CommandFactory;
    const VERBS: &[&str] = &[
        "add", "get", "set", "put", "delete", "update", "create", "remove", "list",
        "run", "check", "send", "mint", "issue", "revoke", "reset", "refresh",
        "sync", "is", "place", "pay", "commit", "query", "upload", "resolve",
    ];
    fn walk(cmd: &Command, path: &str, out: &mut Vec<String>) {
        for sub in cmd.get_subcommands() {
            let name = sub.get_name();
            if let Some((head, _)) = name.split_once('-') {
                if VERBS.contains(&head) {
                    out.push(format!("{path} {name}"));
                }
            }
            walk(sub, &format!("{path} {name}"), out);
        }
    }
    let mut bad = Vec::new();
    walk(&crate::Cli::command(), "hanzo", &mut bad);
    assert!(bad.is_empty(), "verb-noun command names — name the thing, let the position say the verb:\n  {}", bad.join("\n  "));
}
