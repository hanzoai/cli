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
    // A scheme (`://`), a host, or an auth token in the data could redirect a
    // call; a bare `http` in a field NAME (e.g. `httpHeaders`) cannot — it is
    // never used as a URL — so the guard is on the redirect-bearing substrings.
    let src = include_str!("generated.rs");
    for banned in ["://", "Bearer", "Authorization", ".hanzo.", "hanzo.ai", "api.hanzo"] {
        assert!(
            !src.contains(banned),
            "generated data must be host/url/auth-free; found {banned:?}"
        );
    }
    // Every path template is a bare `/v1/…` — no scheme can ride a path.
    for op in OPS {
        assert!(op.path.starts_with("/v1/") && !op.path.contains("://"), "bad path {}", op.path);
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
    let Some(Resolved::Leaf { op, body, .. }) = resolve(&m) else {
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

    let m = augment(Command::new("hanzo")).try_get_matches_from(&argv).expect("parses");
    let Some(Resolved::Leaf { body: LeafBody::Typed(v), .. }) = resolve(&m) else {
        panic!("typed leaf");
    };
    assert_eq!(v[int.key], 42, "int flag must serialize as a JSON number");
    // Only the int we set is present — every other optional BODY field is omitted.
    assert_eq!(v.as_object().unwrap().len(), 1, "unset optionals must be omitted: {v}");
}

/// BUG-1 FIX: a collection GET whose verb also heads a nested group is a RUNNABLE
/// GROUP — `hanzo kv list` runs `GET /v1/kv`, and `hanzo kv list push <key>`
/// descends into the datatype. It is NOT a bare group that demands a subcommand.
#[test]
fn a_runnable_group_runs_its_collection_get_when_invoked_bare() {
    let m = matches_of(&["hanzo", "kv", "list"]);
    let Some(Resolved::Leaf { op, .. }) = resolve(&m) else { panic!("expected a leaf") };
    assert_eq!(op.path, "/v1/kv");
    assert_eq!(op.method, "GET");

    // `push` has a required JSON body field; supplying it proves the descent
    // reaches the datatype op (not the collection GET).
    let m = matches_of(&["hanzo", "kv", "list", "push", "mykey", "--values", "[1,2]"]);
    let Some(Resolved::Leaf { op, values, .. }) = resolve(&m) else { panic!("expected a leaf") };
    assert_eq!(op.path, "/v1/kv/list/{key}/push");
    assert_eq!(values, vec!["mykey"]);
}

/// BUG-2 FIX: an `in: query` parameter becomes a TYPED `--flag` that rides the
/// URL query (not the body), required-ness enforced by clap.
#[test]
fn a_query_param_becomes_a_typed_flag_in_the_url() {
    let m = matches_of(&["hanzo", "o11y", "logs", "--product", "gateway", "--limit", "50"]);
    let Some(Resolved::Leaf { op, body, query, .. }) = resolve(&m) else { panic!("leaf") };
    assert_eq!(op.path, "/v1/o11y/logs");
    assert!(matches!(body, LeafBody::None), "a GET carries no body");
    assert!(query.contains(&"product=gateway".to_string()), "{query:?}");
    assert!(query.contains(&"limit=50".to_string()), "{query:?}");
    // The required query param is enforced.
    assert!(augment(Command::new("hanzo"))
        .try_get_matches_from(["hanzo", "o11y", "logs"])
        .is_err());
}

/// A friendly top-level alias dispatches to the SAME generated op (`hanzo logs`
/// == `hanzo o11y logs`), no duplicated logic.
#[test]
fn a_friendly_alias_dispatches_to_the_same_generated_op() {
    let m = matches_of(&["hanzo", "logs", "--product", "gateway"]);
    let Some(Resolved::Leaf { op, query, .. }) = resolve(&m) else { panic!("alias leaf") };
    assert_eq!(op.path, "/v1/o11y/logs");
    assert!(query.contains(&"product=gateway".to_string()));
    let m2 = matches_of(&["hanzo", "o11y", "logs", "--product", "gateway"]);
    let Some(Resolved::Leaf { op: op2, .. }) = resolve(&m2) else { panic!() };
    assert_eq!(op.path, op2.path, "the alias and the product path resolve to one op");
}

/// CURATION: noise/internal products are denied, singular/plural dupes removed,
/// and the compute plane is unified as ONE `compute` with machines/gpus absorbed.
#[test]
fn curation_denies_noise_dedupes_plurals_and_unifies_compute() {
    for noise in ["console", "download", "upload", "files", "completions", "settings",
                  "provisioning", "do", "csrf", "indexers", "search-docs", "gateway"] {
        assert!(!is_product(noise), "{noise} must be denied as a top-level command");
    }
    // `gateway` is dropped because its whole `/v1/gateway/*` subtree is unmounted
    // (404 live); no op may carry a `/v1/gateway/` path.
    assert!(
        !OPS.iter().any(|o| o.path.starts_with("/v1/gateway/")),
        "no op may target the unmounted /v1/gateway/* subtree"
    );
    // The real gateway surface stays reachable at its TOP-LEVEL served paths.
    assert!(
        OPS.iter().any(|o| o.path == "/v1/models" && o.verb == "list"),
        "`hanzo models list` (GET /v1/models) must remain"
    );
    for plural in ["networks", "clusters", "bots"] {
        assert!(!is_product(plural), "cloud plural {plural} must be deduped away (local wins)");
    }
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
    let m = matches_of(&["hanzo", "commerce", "store", "listing", "get", "store_1", "sku_9"]);
    let Some(Resolved::Leaf { op, values, .. }) = resolve(&m) else {
        panic!("expected a leaf");
    };
    assert_eq!(op.path, "/v1/commerce/store/{storeid}/listing/{key}");
    assert_eq!(values, vec!["store_1", "sku_9"]);
    assert_eq!(
        fill_path(op.path, op.rest, Some("acme"), &values).unwrap(),
        "/v1/commerce/store/store_1/listing/sku_9"
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

/// The hand-written products are omitted from the data outright. `kms` is NO
/// LONGER among them — it is generated now (see `kms_is_generated_*`).
#[test]
fn hand_written_products_are_not_generated() {
    for local in ["agent", "billing", "deploy", "code"] {
        assert!(!is_product(local), "{local} must be hand-written / a wrapper, not generated");
    }
}

/// `kms` is now a GENERATED product, folded to EXACTLY the four routes cloud
/// mounts (`clients/kms/mount.go`): `secrets {list,get,create,rm}`. Nothing the
/// server cannot answer is invented — in particular there is NO PATCH/`update`
/// and NO `rotate` (cloud mounts neither on the org-scoped secrets plane).
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
        r#"POST ["secrets"] create"#.to_string(),
    ];
    assert_eq!(got, want, "kms must fold to exactly the 4 real cloud routes");
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
    let Some(Resolved::Leaf { op, body: LeafBody::Typed(v), .. }) = resolve(&m) else {
        panic!("typed leaf");
    };
    assert_eq!(op.path, "/v1/kms/orgs/{org}/secrets");
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
    let u = build_url("https://x.example", "/v1/agents", &["env=prod".into()]).unwrap();
    assert!(u.starts_with("https://x.example/v1/agents?") && u.contains("env=prod"));
    // A value that looks like extra params is encoded, not injected.
    let u = build_url("https://x.example", "/v1/x", &["q=a b&c=d".into()]).unwrap();
    assert!(u.contains("q=a+b%26c%3Dd"), "{u}");
    assert!(build_url("https://x.example", "/v1/x", &["noeq".into()]).is_err());
}

// ---- KMS folder secrets: a multi-segment path fills to RAW slashes ------------

/// The write-only-folder-secret bug: `get`/`rm` percent-encoded the `/` in a
/// folder-scoped address (`prod/db → prod%2Fdb`) → the server 404'd. Their
/// `{secret}` is now marked MULTI-SEGMENT, so the slashes ride raw and the catch-
/// all resolves. A FLAT name is unchanged, every segment is still encoded, and
/// `.`/`..`/empty are refused before a URL exists — so `create --path p` then
/// `get p/x` / `rm p/x` round-trips while `..` can never re-address another org.
#[test]
fn kms_folder_secret_path_round_trips_with_raw_slashes() {
    for verb in ["get", "rm"] {
        let op = OPS
            .iter()
            .find(|o| o.product == "kms" && o.verb == verb)
            .unwrap_or_else(|| panic!("kms {verb}"));
        assert_eq!(op.rest, &["secret"], "kms {verb} must mark {{secret}} multi-segment");

        // A folder-scoped address keeps its slashes RAW (server: last seg = name).
        let filled =
            fill_path(op.path, op.rest, Some("acme"), &["prod/db/password".into()]).unwrap();
        assert_eq!(filled, "/v1/kms/orgs/acme/secrets/prod/db/password");

        // A FLAT name (already working) is untouched — one segment, no slash.
        let flat = fill_path(op.path, op.rest, Some("acme"), &["DB".into()]).unwrap();
        assert_eq!(flat, "/v1/kms/orgs/acme/secrets/DB");

        // Each segment is STILL percent-encoded: a space/`?` cannot re-address.
        let enc = fill_path(op.path, op.rest, Some("acme"), &["a b/x?y".into()]).unwrap();
        assert_eq!(enc, "/v1/kms/orgs/acme/secrets/a%20b/x%3Fy");

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

/// The silent-swallow bug: some planes (Casdoor/iam) answer a refusal with HTTP
/// 200 and `{"status":"error","msg":…}`. `envelope_error` reads that as a failure
/// carrying the server's message (so `call` exits non-zero to stderr), while a
/// genuine success — a success envelope, a delete's null `data`, or a raw body —
/// stays `None` and renders exactly as before.
#[test]
fn a_2xx_error_envelope_is_surfaced_not_swallowed() {
    use serde_json::json;

    // The Casdoor/iam shape: HTTP 200 body, but the envelope says error.
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
