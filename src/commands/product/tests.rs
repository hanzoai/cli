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
    for banned in [
        "://",
        "Bearer",
        "Authorization",
        ".hanzo.",
        "hanzo.ai",
        "api.hanzo",
    ] {
        assert!(
            !scrubbed.contains(banned),
            "generated call data must be host/url/auth-free; found {banned:?}"
        );
    }
    // A LINK is a scheme with somewhere to go. Banning the bare characters `://`
    // banned the word "otpauth://" as well — the URI SCHEME cloud's MFA enrollment
    // hands back for a person to render as a QR code, named in prose as a scheme
    // and followed by a space. Naming a scheme is a fact, exactly as naming a host
    // is; what may never appear is an authority after it, because that is the part
    // a client could dial instead of the configured origin.
    for op in OPS {
        let link = op.sum.match_indices("://").any(|(i, _)| {
            op.sum[i + 3..]
                .chars()
                .next()
                .is_some_and(|c| c.is_ascii_alphanumeric())
        });
        assert!(
            !link,
            "prose may name a host or a scheme, never carry a link: {}",
            op.sum
        );
    }
    // A product description is display-only prose, but it still must not carry
    // an auth token — and the tuple shape means it can never be dialed.
    for (name, d) in PRODUCTS {
        assert!(
            !d.contains("Bearer") && !d.contains("Authorization"),
            "auth in {name} prose"
        );
    }
    // Every path template is a bare `/v1/…` — no scheme can ride a path.
    for op in OPS {
        assert!(
            op.path.starts_with("/v1/") && !op.path.contains("://"),
            "bad path {}",
            op.path
        );
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
        .map(|o| {
            format!(
                "  hanzo {} … {} <- {} {}",
                o.product, o.verb, o.method, o.path
            )
        })
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
        let templated = op
            .path
            .split('/')
            .filter(|s| s.starts_with('{') && s.ends_with('}'))
            .count();
        let scope = scope_count(op.path);
        assert_eq!(
            op.params.len(),
            templated - scope,
            "params must equal the non-scope templated segments: {}",
            op.path
        );
        let values: Vec<String> = op.params.iter().map(|p| format!("v-{p}")).collect();
        let filled = fill_path(op.path, op.rest, Some("acme"), &values).expect("fills");
        assert!(
            !filled.contains('{') && !filled.contains('}'),
            "unfilled: {filled}"
        );
        // `rest` is a subset of `params` (never names the tenant scope).
        for r in op.rest {
            assert!(
                op.params.contains(r),
                "rest {r} must be a positional: {}",
                op.path
            );
        }
        if scope > 0 {
            assert!(
                filled.contains("/orgs/acme"),
                "scope must bind owner: {filled}"
            );
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
        .filter(|(i, s)| s.starts_with('{') && s.ends_with('}') && *i > 0 && segs[i - 1] == "orgs")
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
                !["data", "output", "query", "method", "subpath"].contains(p),
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
            assert!(
                !op.params.contains(&name),
                "scope {name} leaked as a positional: {}",
                op.path
            );
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
/// derive tree is — `every_operation_of_an_absorbed_product_resolves_to_its_own_route`.
pub(super) fn hand() -> Command {
    Command::new("hanzo")
}

pub(super) fn matches_of(argv: &[&str]) -> clap::ArgMatches {
    augment(hand()).try_get_matches_from(argv).expect("parses")
}

#[test]
fn a_simple_leaf_resolves_and_fills() {
    let m = matches_of(&["hanzo", "agent", "sessions", "get", "sess_1"]);
    let Some(Resolved::Leaf { op, values, .. }) = resolve(&hand(), &m) else {
        panic!("expected a leaf");
    };
    assert_eq!(op.path, "/v1/agent/sessions/{id}");
    assert_eq!(op.method, "GET");
    assert_eq!(values, vec!["sess_1"]);
    assert_eq!(
        fill_path(op.path, op.rest, Some("acme"), &values).unwrap(),
        "/v1/agent/sessions/sess_1"
    );
}

/// THE headline: a write op whose body the DOCUMENT types takes TYPED flags, and
/// the JSON body is assembled from them at their schema types — never `--data`.
///
/// The op is CHOSEN from the spec, not named here. It used to be `authz check
/// --sub --obj --act`, and that shape came from the hand-authored master: cloud's
/// own document declares `POST /v1/authz/check` with no requestBody at all, so
/// naming it pinned a snapshot of a second authority rather than the property.
/// Pick whichever write the document actually types and assert the property on it.
#[test]
fn a_typed_write_assembles_a_json_body_from_flags() {
    let op = OPS
        .iter()
        .find(|o| {
            o.method == "POST"
                && o.params.is_empty()
                && !o.fields.is_empty()
                && o.fields
                    .iter()
                    .all(|f| !f.query && !f.secret && !f.repeat && matches!(f.ty, Ty::Str))
        })
        .expect("cloud types at least one bodied POST with only scalar string properties");

    let mut argv = vec!["hanzo".to_string(), op.product.to_string()];
    argv.extend(op.nodes.iter().map(|n| n.to_string()));
    argv.push(op.verb.to_string());
    for f in op.fields {
        argv.push(format!("--{}", f.flag));
        argv.push(format!("v-{}", f.key));
    }
    let argv: Vec<&str> = argv.iter().map(String::as_str).collect();

    let m = matches_of(&argv);
    let Some(Resolved::Leaf { op: got, body, .. }) = resolve(&hand(), &m) else {
        panic!("expected a leaf for {argv:?}");
    };
    assert_eq!(got.path, op.path);
    let LeafBody::Typed(v) = body else {
        panic!("typed leaf must build a JSON body")
    };
    for f in op.fields {
        assert_eq!(
            v[f.key],
            format!("v-{}", f.key),
            "{} did not reach the body",
            f.key
        );
    }

    // A typed leaf takes no `--data` — the flags ARE the body — and using it is refused by name.
    let mut leaky = argv.clone();
    leaky.push("--data");
    leaky.push("{}");
    let m = matches_of(&leaky);
    let Some(Resolved::Leaf {
        body: LeafBody::Misused,
        ..
    }) = resolve(&hand(), &m)
    else {
        panic!("a typed write must refuse --data: {leaky:?}");
    };
    let said = flags_not_data(op);
    assert!(
        said.contains("takes its body as flags, not --data"),
        "{said}"
    );
    for f in op.fields.iter().filter(|f| !f.query && !f.secret) {
        assert!(
            said.contains(&format!("--{}", field_flag(f))),
            "{said} does not name --{}",
            field_flag(f)
        );
    }
}

/// An INTEGER-typed flag reaches the body as a JSON number (not a string), and an
/// unset optional flag is OMITTED (the server's default stands), never sent null.
#[test]
fn a_typed_int_flag_is_a_json_number_and_optionals_are_omitted() {
    // Pick a typed op with a top-level BODY int field (not a query param, not a
    // nested dotted key), no path params, and NO required fields — so the only
    // flag we pass is the int, and the body holds exactly it.
    let top_int = |f: &&Field| matches!(f.ty, Ty::Int) && !f.query && !f.key.contains('.');
    let op = OPS
        .iter()
        .find(|o| {
            o.params.is_empty()
                && o.fields.iter().any(|f| top_int(&f))
                && o.fields.iter().all(|f| !f.required)
        })
        .expect("a typed op with a top-level body int field and no required fields exists");
    let int = op.fields.iter().find(top_int).unwrap();

    let mut argv = vec!["hanzo".to_string(), op.product.to_string()];
    argv.extend(op.nodes.iter().map(|n| n.to_string()));
    argv.push(op.verb.to_string());
    argv.push(format!("--{}", int.flag));
    argv.push("42".into());

    let m = augment(hand()).try_get_matches_from(&argv).expect("parses");
    let Some(Resolved::Leaf {
        body: LeafBody::Typed(v),
        ..
    }) = resolve(&hand(), &m)
    else {
        panic!("typed leaf");
    };
    assert_eq!(v[int.key], 42, "int flag must serialize as a JSON number");
    // Only the int we set is present — every other optional BODY field is omitted.
    assert_eq!(
        v.as_object().unwrap().len(),
        1,
        "unset optionals must be omitted: {v}"
    );
}

/// An ARRAY property is a REPEATABLE flag, one element per occurrence, and the
/// body carries a real JSON array. It used to be one `Ty::Json` flag, so saying
/// "two plans" meant hand-writing `--plans '["a","b"]'` — JSON on a command line,
/// quoted past a shell, to state a list the schema had already described.
///
/// The op is chosen from the DATA (the first with a repeatable string body
/// field and no required flags), so this pins the property, never a coordinate.
#[test]
fn an_array_property_is_a_repeatable_flag_and_lands_as_a_json_array() {
    let op = OPS
        .iter()
        .find(|o| {
            o.params.is_empty()
                && o.fields.iter().all(|f| !f.required)
                && o.fields.iter().any(|f| {
                    f.repeat && !f.query && f.choices.is_empty() && matches!(f.ty, Ty::Str)
                })
        })
        .expect("cloud declares at least one array-of-string body property");
    let arr = op
        .fields
        .iter()
        .find(|f| f.repeat && !f.query && f.choices.is_empty() && matches!(f.ty, Ty::Str))
        .unwrap();

    let mut argv = vec!["hanzo".to_string(), op.product.to_string()];
    argv.extend(op.nodes.iter().map(|n| n.to_string()));
    argv.push(op.verb.to_string());
    for v in ["one", "two"] {
        argv.push(format!("--{}", arr.flag));
        argv.push(v.into());
    }
    let m = augment(hand()).try_get_matches_from(&argv).expect("parses");
    let Some(Resolved::Leaf {
        body: LeafBody::Typed(v),
        ..
    }) = resolve(&hand(), &m)
    else {
        panic!("typed leaf");
    };
    assert_eq!(v[arr.key], serde_json::json!(["one", "two"]), "in {v}");
    // Order is the order they were typed — a list is not a set.
    assert_eq!(
        v.as_object().unwrap().len(),
        1,
        "unset optionals stay out: {v}"
    );
}

/// A NESTED object is DOTTED flags, and the body rebuilds the object the schema
/// declared: `--metrics.load1 0.5` sends `{"metrics":{"load1":0.5}}`. Before this
/// the whole sub-object was one `Ty::Json` blob, so a caller had to know the
/// nested shape the document had already stated.
#[test]
fn a_nested_object_is_dotted_flags_that_rebuild_the_object() {
    let op = OPS
        .iter()
        .find(|o| {
            o.params.is_empty()
                && o.fields.iter().all(|f| !f.required)
                && o.fields.iter().any(|f| {
                    !f.query && !f.repeat && f.key.contains('.') && matches!(f.ty, Ty::Str)
                })
        })
        .expect("cloud declares at least one nested body object");
    let nested = op
        .fields
        .iter()
        .find(|f| !f.query && !f.repeat && f.key.contains('.') && matches!(f.ty, Ty::Str))
        .unwrap();

    let mut argv = vec!["hanzo".to_string(), op.product.to_string()];
    argv.extend(op.nodes.iter().map(|n| n.to_string()));
    argv.push(op.verb.to_string());
    argv.push(format!("--{}", nested.flag));
    argv.push("deep".into());
    let m = augment(hand()).try_get_matches_from(&argv).expect("parses");
    let Some(Resolved::Leaf {
        body: LeafBody::Typed(v),
        ..
    }) = resolve(&hand(), &m)
    else {
        panic!("typed leaf");
    };
    // Walk the dotted key: every step is a real JSON object, and the leaf holds
    // the value — the flag is FLAT and the body is NESTED.
    let mut node = &v;
    for step in nested.key.split('.') {
        node = node
            .get(step)
            .unwrap_or_else(|| panic!("{} missing in {v}", nested.key));
    }
    assert_eq!(node, "deep");
    assert!(
        v[nested.key.split('.').next().unwrap()].is_object(),
        "must nest, not flatten: {v}"
    );
}

/// The dotted-key law, held on the values rather than on a document: two flags
/// under one parent land in ONE object, and a `.` in a flag never reaches the
/// wire as a literal key.
#[test]
fn two_dotted_keys_under_one_parent_build_one_object() {
    let mut m = Map::new();
    insert_path(&mut m, "spec.replicas", json!(3));
    insert_path(&mut m, "spec.image.name", json!("hanzo"));
    insert_path(&mut m, "name", json!("web"));
    assert_eq!(
        Value::Object(m),
        json!({"name": "web", "spec": {"replicas": 3, "image": {"name": "hanzo"}}})
    );
}

/// A body property whose name repeats a PATH parameter is normally dropped —
/// it is the same value, already a positional. NOT when it is the body's ONLY
/// property: dropping it there types the whole write away and hands the caller
/// `--data`, which is strictly less than the schema stated. `POST
/// /v1/o11y/service_accounts/{id}/roles` is the case cloud actually serves — its
/// `{id}` is the service account and its body `id` is the ROLE being assigned.
#[test]
fn a_lone_body_property_survives_a_path_parameter_of_the_same_name() {
    for op in OPS
        .iter()
        .filter(|o| matches!(o.method, "POST" | "PUT" | "PATCH"))
    {
        let body: Vec<&Field> = op.fields.iter().filter(|f| !f.query).collect();
        // Where a body key echoes a path param, it may only be because it was the
        // body's whole shape.
        for f in &body {
            if op.params.contains(&f.key) {
                assert_eq!(
                    body.len(),
                    1,
                    "{} {} keeps `{}` beside {} other body field(s) — a path echo is only \
                     kept when dropping it would leave no typed body at all",
                    op.method,
                    op.path,
                    f.key,
                    body.len() - 1
                );
            }
        }
        // And the converse: a write with a declared body never falls back to
        // `--data` because the echo rule emptied it.
        assert!(
            !(body.is_empty()
                && op
                    .params
                    .iter()
                    .any(|p| op.fields.iter().any(|f| f.key == *p))),
            "{} {} typed its whole body away",
            op.method,
            op.path
        );
    }
}

/// BUG-1 FIX: an operation whose own name ALSO heads a group is still a runnable
/// leaf — `hanzo <…> <name>` runs it rather than demanding a subcommand.
///
/// WHICH coordinate has that shape is the document's answer and it moves: this
/// pinned `hanzo kv list` -> `GET /v1/kv` until cloud made kv bucket-scoped, and
/// then it asserted a route no server answers. So the example is chosen from the
/// tree — a leaf whose (nodes + verb) is also a node prefix of some sibling — and
/// the property is what is pinned.
#[test]
fn a_leaf_whose_name_also_heads_a_group_is_still_runnable() {
    let heads: std::collections::BTreeSet<(&str, Vec<&str>)> = OPS
        .iter()
        .flat_map(|o| (1..=o.nodes.len()).map(move |i| (o.product, o.nodes[..i].to_vec())))
        .collect();
    let op = OPS
        .iter()
        .find(|o| {
            let mut here = o.nodes.to_vec();
            here.push(o.verb);
            heads.contains(&(o.product, here))
        })
        .expect("some operation's own name also heads a group of its siblings");

    let mut argv = vec!["hanzo".to_string(), op.product.to_string()];
    argv.extend(op.nodes.iter().map(|n| n.to_string()));
    argv.push(op.verb.to_string());
    for p in op.params {
        argv.push(format!("v-{p}"));
    }
    let argv: Vec<&str> = argv.iter().map(String::as_str).collect();
    let m = matches_of(&argv);
    let Some(Resolved::Leaf { op: got, .. }) = resolve(&hand(), &m) else {
        panic!(
            "`{}` must run its own operation, not demand a subcommand",
            argv.join(" ")
        )
    };
    assert_eq!((got.method, got.path), (op.method, op.path));
}

/// BUG-2 FIX: an `in: query` parameter becomes a TYPED `--flag` that rides the
/// URL query (not the body), required-ness enforced by clap.
#[test]
fn a_query_param_becomes_a_typed_flag_in_the_url() {
    // WHICH op carries an optional query flag is the spec's answer, not this
    // test's — the same rule the second half already follows, and the first half
    // used to break. Pinning `o11y logs` is what broke it: cloud grew
    // /v1/o11y/logs/{aggregate,livetail,fields,pipelines}, so `logs` stopped
    // being a verb and became a node with `get` beneath it. The COORDINATE
    // moved; the property never did.
    let target = OPS
        .iter()
        .find(|o| {
            o.method == "GET"
                && o.params.is_empty()
                && o.fields.iter().filter(|f| f.query && !f.required).count() >= 1
        })
        .expect("some GET takes an optional query parameter");
    let flag = target
        .fields
        .iter()
        .find(|f| f.query && !f.required)
        .unwrap();

    let mut argv: Vec<String> = vec!["hanzo".into(), target.product.into()];
    argv.extend(target.nodes.iter().map(|n| n.to_string()));
    argv.push(target.verb.into());
    argv.push(format!("--{}", flag.flag));
    argv.push("gateway".into());

    let m = augment(hand()).try_get_matches_from(&argv).expect("parses");
    let Some(Resolved::Leaf {
        op, body, query, ..
    }) = resolve(&hand(), &m)
    else {
        panic!("leaf")
    };
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

/// A graph question asked at a point carries both times to the wire: `--as-of`
/// and `--as-known` land in a POST's body and a GET's query under the document's
/// own names. The ops are the document's — every graph op that takes `as_known`.
#[test]
fn a_graph_question_carries_both_times_to_the_wire() {
    let (of, known) = ("2026-09-01T00:00:00Z", "2026-09-15T00:00:00Z");
    let timed: Vec<&Op> = OPS
        .iter()
        .filter(|o| o.product == "graph" && o.fields.iter().any(|f| f.key == "as_known"))
        .collect();
    assert!(
        timed.len() >= 7,
        "the document carries {} graph ops with as_known",
        timed.len()
    );
    for op in timed {
        let mut argv = vec!["hanzo".to_string(), op.product.to_string()];
        argv.extend(op.nodes.iter().map(|n| n.to_string()));
        argv.push(op.verb.to_string());
        for f in op.fields.iter().filter(|f| f.required) {
            argv.push(format!("--{}", f.flag));
            argv.push(if matches!(f.ty, Ty::Int) {
                "1".into()
            } else {
                format!("v-{}", f.key)
            });
        }
        argv.extend(["--as-of", of, "--as-known", known].map(String::from));
        let argv: Vec<&str> = argv.iter().map(String::as_str).collect();
        let Some(Resolved::Leaf {
            op: got,
            body,
            query,
            ..
        }) = resolve(&hand(), &matches_of(&argv))
        else {
            panic!("expected a leaf for {argv:?}");
        };
        assert_eq!(got.path, op.path);
        match body {
            LeafBody::Typed(v) => assert_eq!(
                (&v["as_of"], &v["as_known"]),
                (&Value::from(of), &Value::from(known)),
                "{argv:?}"
            ),
            _ => assert!(
                query.contains(&format!("as_of={of}"))
                    && query.contains(&format!("as_known={known}")),
                "{argv:?} sent {query:?}"
            ),
        }
    }
}

/// EVERY CAPABILITY THE DOCUMENT CARRIES IS A COMMAND. Read straight off
/// `spec/cloud.json` — the projection `genproduct` derives the tree from — so this
/// is the contract asking the parser, not one list of names asking another.
///
/// A capability is the first segment after `/v1/`, which is how the document, the
/// SDKs and the MCP catalogue all count them. A PARAMETERISED first segment
/// (`/v1/{name}`, the capability index) is not one: it is the fallthrough wearing
/// a longer path, and `genspec` drops it at the same rule.
///
/// This replaces a list of names kept by hand — `logs`, `files`, `machines`,
/// `csrf`, `openapi.json` — half of which the document had stopped carrying and
/// half of which it had started. A fixture that names routes is a second authority
/// over what exists, which is the defect this whole pipeline was built to end.
#[test]
fn every_capability_the_document_carries_is_a_command() {
    let spec = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/spec/cloud.json"))
        .expect("spec/cloud.json is the projection this tree is derived from");
    let doc: serde_json::Value = serde_json::from_str(&spec).expect("valid json");
    let caps: std::collections::BTreeSet<&str> = doc["paths"]
        .as_object()
        .expect("paths")
        .keys()
        .filter_map(|p| p.strip_prefix("/v1/"))
        .map(|rest| rest.split('/').next().unwrap_or(""))
        .filter(|c| !c.is_empty() && !c.starts_with('{'))
        .collect();
    assert!(
        caps.len() > 100,
        "the document carries {} capabilities",
        caps.len()
    );

    // A capability `src/curation.rs` places under another command keeps its
    // routes and moves coordinate, so what is asked is whether its routes reach a
    // command — at its own name, or under the one the arrangement names.
    let reached = |c: &str| {
        is_product(c)
            || OPS.iter().any(|o| {
                o.path
                    .strip_prefix("/v1/")
                    .and_then(|r| r.split('/').next())
                    == Some(c)
            })
    };
    let missing: Vec<&str> = caps.iter().copied().filter(|c| !reached(c)).collect();
    assert!(
        missing.is_empty(),
        "{} capability(ies) the document serves reach no command: {missing:?}\n\n\
         A capability with no command is closed in the GENERATOR — by the fold that names it, \
         or by the arrangement that mounts it — never by a note saying it was skipped.",
        missing.len(),
    );
}

#[test]
fn model_table_renders_progressive_enso_glyphs_and_hides_enso_free() {
    let payload = json!({
        "object": "list",
        "data": [
            { "id": "enso-auto", "object": "model", "owned_by": "hanzo" },
            { "id": "enso-ultra", "object": "model", "owned_by": "hanzo" },
            { "id": "enso-max", "object": "model", "owned_by": "hanzo" },
            { "id": "enso-pro", "object": "model", "owned_by": "hanzo" },
            { "id": "enso", "object": "model", "owned_by": "hanzo" },
            { "id": "enso-flash", "object": "model", "owned_by": "hanzo" },
            { "id": "enso-free", "object": "model", "owned_by": "hanzo" },
            { "id": "zen6", "object": "model", "owned_by": "hanzo" }
        ]
    });
    let cleaned = dispatch::sanitize_model_catalog(&payload);
    let arr = cleaned["data"].as_array().unwrap();
    assert_eq!(arr.len(), 7);
    assert!(!arr.iter().any(|m| m["id"] == "enso-free"));

    let rendered = dispatch::render(&cleaned["data"], Output::Table, 120).unwrap();
    for expected in [
        "○ enso-auto",
        "◕ enso-ultra",
        "◑ enso-max",
        "◔ enso-pro",
        "◌ enso",
        "◦ enso-flash",
        "zen6",
    ] {
        assert!(rendered.contains(expected), "missing `{expected}` in:\n{rendered}");
    }
    assert!(!rendered.contains("enso-free"), "enso-free leaked:\n{rendered}");
    assert!(!rendered.contains("○ enso-ultra"));
    assert!(!rendered.contains("○ enso-flash"));
}

