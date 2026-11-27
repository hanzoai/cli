use super::*;
use super::tests::{hand, matches_of};
use clap::Command;

/// A top-level name means the product cloud serves at that name — there is no
/// second source of top-level names, and PARSE and DISPATCH must agree on every
/// one of them.
///
/// It used to walk one coordinate (`hanzo logs query`) and it walks all of them
/// now. Asserting the MOUNT alone never saw the defect it was written for:
/// `augment` skipped a shadowed alias while `resolve` still preferred it, so the
/// parser mounted the product, dispatch sent it to a different op, and the command
/// PANICKED on an argument id its own command never defined. The two halves
/// disagreed in the gap between them, so this walks all the way to the op.
#[test]
fn every_coordinate_parses_and_dispatches_to_its_own_route() {
    let merged = augment(hand());
    for op in OPS {
        let argv = argv_for(op);
        let line = argv.join(" ");
        let m = merged
            .clone()
            .try_get_matches_from(&argv)
            .unwrap_or_else(|e| panic!("`{line}` does not parse:\n{e}"));
        let Some(Resolved::Leaf { op: got, .. }) = resolve(&hand(), &m) else {
            panic!("`{line}` parses and reaches no operation")
        };
        assert_eq!(
            (got.method, got.path),
            (op.method, op.path),
            "`{line}` resolved to {} {} instead",
            got.method,
            got.path
        );
    }
    assert!(
        !include_str!("mod.rs").contains("ALIASES"),
        "no second source of top-level names may come back"
    );
}

/// The deep-nested case the naive case-tables broke on: arbitrary depth resolves
/// to the right op and fills every positional in order (no scope here — `org` is
/// the literal `org`, not the `orgs/{org}` scope pair, so it stays a positional).
#[test]
fn a_deep_nested_leaf_resolves_and_fills_in_order() {
    let m = matches_of(&[
        "hanzo", "platform", "projects", "apps", "get", "site", "web",
    ]);
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
    assert!(
        !src.contains("Resolved::Pass"),
        "no passthrough variant may remain"
    );
    assert!(
        !src.contains("fn passthrough"),
        "no passthrough builder may remain"
    );
    // The dispatcher speaks the seam directly, in-module (no `api` command).
    assert!(
        !src.contains("super::api"),
        "the seam moved in-module; no `api` command remains"
    );
}

// ---- collisions: a local command always wins its bare name -------------------

/// The shortest argv that reaches an op: its address, then a value for every
/// required input, invented from the DECLARED type. Nothing here knows an
/// operation by name — a test that names one proves the property for one.
pub(super) fn argv_for(op: &'static Op) -> Vec<String> {
    let mut a: Vec<String> = vec!["hanzo".into(), op.product.into()];
    a.extend(op.nodes.iter().map(|n| (*n).to_string()));
    a.push(op.verb.into());
    // A path parameter is a required positional. The value is never sent.
    a.extend(op.params.iter().map(|_| "x".to_string()));
    for f in op.fields.iter().filter(|f| f.required && !f.secret) {
        a.push(format!("--{}", field_flag(f)));
        if let Some(v) = f.choices.first() {
            a.push((*v).to_string());
            continue;
        }
        match f.ty {
            // A scalar bool is a bare presence flag; an array of them is not.
            Ty::Bool if !f.repeat => {}
            Ty::Bool => a.push("true".into()),
            Ty::Int | Ty::Num => a.push("1".into()),
            Ty::Json => a.push("{}".into()),
            Ty::Str | Ty::Union => a.push("x".into()),
        }
    }
    a
}

/// A name a LOCAL command owns is ABSORBED, never dropped — the local command
/// keeps every verb it declares and gains every one the document does.
///
/// This test used to assert the opposite: that `billing` and `code` "must be
/// hand-written, not generated". That is how 25 served `/v1/billing` operations
/// and 7 `/v1/code` ones came to be reachable by nothing, with a curation entry
/// calling it a decision. Written as a PROPERTY over whatever collides, so the
/// next local command to share a cloud name is covered without an edit.
///
/// AND THEN IT ASSERTED THE NAME. `here.find_subcommand(node).is_some()` passes
/// when the node under that name IS the local command — the name exists, the
/// operation reaches nobody, and the test says the law holds. That is the exact
/// defect one layer up, where `hanzo logs` MOUNTED and dispatched somewhere else
/// and "a test that asserted only the MOUNT never saw it". The cure there was to
/// walk parse → resolve → op; it was never applied here. It is now: every
/// operation of every absorbed product is PARSED from an argv a person could
/// type and RESOLVED, and the op that comes back must be the op that went in.
#[test]
fn every_operation_of_an_absorbed_product_resolves_to_its_own_route() {
    use clap::CommandFactory;
    let hand = crate::Cli::command();
    let merged = augment(crate::Cli::command());
    let mut absorbed = 0usize;
    for p in OPS
        .iter()
        .map(|o| o.product)
        .collect::<std::collections::BTreeSet<_>>()
    {
        let Some(local) = hand.find_subcommand(p) else {
            continue;
        };
        absorbed += 1;
        let here = merged
            .find_subcommand(p)
            .expect("a product mounts under some name");
        // Every operation of the product is reachable AS ITSELF…
        for op in OPS.iter().filter(|o| o.product == p) {
            let argv = argv_for(op);
            let line = argv.join(" ");
            // A SHADOW is a name that means two acts. `hanzo wallet list` reads
            // the local keychain and `GET /v1/wallet` lists the org's cloud
            // wallets; one word, two things, and no rule can give the second a
            // name without inventing one — so the local command keeps it, and the
            // check below is that the shadow really is terminal on both sides.
            // A shadowing GROUP would be a different fact: `absorb` descends into
            // one, so an operation losing its route to a group is drift here.
            let mut cur = local;
            let mut shadowed = true;
            for &seg in op.nodes.iter().chain(std::iter::once(&op.verb)) {
                match cur.find_subcommand(seg) {
                    Some(next) => cur = next,
                    None => {
                        shadowed = false;
                        break;
                    }
                }
            }
            let shadowed = shadowed && cur.get_subcommands().next().is_none();
            // `argv` is the GENERATED spelling. A shadow's name belongs to the local
            // command, whose own operands it need not satisfy (`hanzo post create`
            // takes a required `<CONTENT>`), so a shadowed argv may fail to parse:
            // the local command is the route, and `shadowed` proved it terminal.
            let m = match merged.clone().try_get_matches_from(&argv) {
                Ok(m) => m,
                Err(_) if shadowed => continue,
                Err(e) => panic!("`{line}` does not parse:\n{e}"),
            };
            let Some(Resolved::Leaf { op: got, .. }) = resolve(&hand, &m) else {
                assert!(
                    shadowed,
                    "`{line}` reaches no operation — {} {} is served, described, and reachable by \
                     nobody, and no local command of that name terminates there to explain it",
                    op.method, op.path
                );
                continue;
            };
            if shadowed && (got.method, got.path) != (op.method, op.path) {
                continue;
            }
            assert_eq!(
                (got.method, got.path),
                (op.method, op.path),
                "`{line}` resolved to {} {} instead",
                got.method,
                got.path
            );
        }
        // …and every name the local command declared is still ITS OWN command,
        // not a generated node wearing the same name.
        for sub in local.get_subcommands() {
            let mine = here.find_subcommand(sub.get_name()).unwrap_or_else(|| {
                panic!(
                    "`hanzo {p} {}` is the local command's and was overwritten",
                    sub.get_name()
                )
            });
            assert_eq!(
                mine.get_about().map(ToString::to_string),
                sub.get_about().map(ToString::to_string),
                "`hanzo {p} {}` is no longer the command the local tree declares",
                sub.get_name()
            );
        }
    }
    assert!(
        absorbed > 0,
        "no product shares a local name — the collision law is untested"
    );
}

/// A local command owns EVERY spelling it answers to, its aliases included. The
/// hand-written `hanzo post delete` answers to `rm`; once cloud's public document
/// carried `DELETE /v1/post/{id}` (genproduct's verb: `rm`), `absorb` compared
/// names only, mounted the generated `rm` beside the local alias, and clap
/// panicked on every `hanzo post` ("command name `rm` is duplicated") — the
/// drift gate then saw no `Usage: hanzo post` and called a served product an
/// orphan. `resolve` already reads an alias as the local command's (clap's
/// `find_subcommand` matches aliases), so the mount has to agree with it.
#[test]
fn a_local_alias_owns_its_spelling_and_the_generated_verb_stands_down() {
    use clap::CommandFactory;
    let hand = crate::Cli::command();
    let merged = augment(crate::Cli::command());
    let mut asked = 0usize;
    for p in OPS
        .iter()
        .map(|o| o.product)
        .collect::<std::collections::BTreeSet<_>>()
    {
        let (Some(local), Some(here)) = (hand.find_subcommand(p), merged.find_subcommand(p)) else {
            continue;
        };
        for sub in local.get_subcommands() {
            for alias in sub.get_all_aliases() {
                asked += 1;
                let answering: Vec<&str> = here
                    .get_subcommands()
                    .filter(|s| s.get_name() == alias || s.get_all_aliases().any(|a| a == alias))
                    .map(clap::Command::get_name)
                    .collect();
                assert_eq!(
                    answering,
                    vec![sub.get_name()],
                    "`hanzo {p} {alias}` must mean the local `{p} {}` and nothing else",
                    sub.get_name()
                );
            }
        }
    }
    assert!(
        asked > 0,
        "no absorbed product's local command has an alias — this rule is untested"
    );
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
    let local: std::collections::HashSet<String> = hand
        .get_subcommands()
        .map(|s| s.get_name().to_string())
        .collect();
    let merged = augment(crate::Cli::command());
    for p in OPS
        .iter()
        .map(|o| o.product)
        .collect::<std::collections::BTreeSet<_>>()
    {
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
        assert!(
            super::mounted(&hand).iter().all(|m| *m != p),
            "{p} is absorbed, not mounted"
        );
    }
    // The reservation that had lost its owner: `deploy` is generated again, and it
    // is the CD control plane, not a wrapper.
    assert!(
        is_product("deploy"),
        "cloud serves /v1/deploy/* — `deploy` is a product"
    );
    assert!(
        !local.contains("deploy"),
        "no local `deploy` command may reclaim the name silently"
    );
    assert!(
        OPS.iter()
            .any(|o| o.product == "deploy" && o.path == "/v1/deploy/applications"),
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
        let sub = merged
            .find_subcommand(*n)
            .expect("advertised group must parse");
        assert!(
            sub.has_subcommands(),
            "`{n}` is advertised as a GROUP; it must take subcommands"
        );
    }
    let page = crate::commands::man::page(&hand);
    for local in hand.get_subcommands().map(clap::Command::get_name) {
        assert!(
            !advertised.contains(&local),
            "`{local}` is a local command; the generated tree must not also advertise it"
        );
    }
    assert!(
        page.contains("deploy"),
        "the CD control plane must appear on the page"
    );
}

/// WHAT KILLED THE SECOND CONNECTOR COMMAND, pinned so it cannot come back
/// silently. `hanzo connector add` existed beside `hanzo integrations connect`
/// over the same four routes for ONE reason: it kept the provider credential off
/// argv, and the generated command could not, because the handler declared no
/// body. Cloud types it now — `connectIn.token` — and a credential is a secret by
/// NAME, so the generated command has no `--token` at all and a value-bearing
/// argv is a PARSE ERROR rather than a runtime refusal. If this fails, the
/// handler lost its type upstream and a credential just became typeable on the
/// command line; the fix is in hanzoai/cloud, and it is urgent.
#[test]
fn the_credential_of_a_connect_has_no_flag_to_carry_it() {
    let op = OPS
        .iter()
        .find(|o| o.path == "/v1/provider/{provider}/connect" && o.method == "POST")
        .expect("cloud serves the connector plane");
    let token = op
        .fields
        .iter()
        .find(|f| f.key == "token")
        .expect("the document types the body");
    assert!(token.secret, "a provider credential must never be a flag");
    let argv = [
        "hanzo",
        op.product,
        op.verb,
        "cloudflare",
        "--token",
        "cf-live-key",
    ];
    assert!(
        augment(Command::new("hanzo"))
            .try_get_matches_from(argv)
            .is_err(),
        "`--token <literal>` must not parse"
    );
}

/// A SECRET FIELD IS NEVER READ WITH THE PIPE READER ON A TERMINAL. `dispatch`
/// read stdin either way, so a person running a secret-taking command
/// interactively typed their credential in the clear — which is exactly why a
/// hand-written command with a hidden prompt outlived the generated one. The
/// branch is asserted over the SOURCE because the alternative needs a tty: what
/// must never come back is an unconditional pipe read.
#[test]
fn a_terminal_is_prompted_and_never_read_as_a_pipe() {
    let src = include_str!("dispatch.rs");
    let body = &src[src.find("fn inject_secret").expect("inject_secret exists")..];
    let body = &body[..body.find("\n}\n").expect("a function ends")];
    assert!(
        body.contains("secret::secret_source("),
        "the source of a secret is the law's to decide"
    );
    assert!(
        body.contains("SecretSource::Prompt =>"),
        "a terminal must be prompted"
    );
    assert!(
        !body.contains("read_secret(std::io::stdin().lock())?;\n    let obj"),
        "unconditional pipe read"
    );
}

/// THE invariant of a secrets CLI, on the GENERATED path: a `format: password`
/// body property has NO flag and NO positional, so a value-bearing argv is a PARSE
/// ERROR — a property of the grammar, not of the handler's discipline — and
/// `resolve` never sees the value (it is injected from stdin at dispatch).
///
/// Asserted over every op that declares one, and the COUNT is pinned so the day
/// cloud types one this test starts doing its real work instead of passing
/// vacuously forever.
#[test]
fn a_stdin_secret_can_never_reach_argv() {
    let secrets: Vec<(&Op, &Field)> = OPS
        .iter()
        .flat_map(|o| o.fields.iter().filter(|f| f.secret).map(move |f| (o, f)))
        .collect();
    assert!(
        !secrets.is_empty(),
        "not one body property reads as a secret, so the law below is asserted over nothing. \
         That is a hanzoai/cloud regression — a typed secret body stopped being typed — and not \
         something to paper over here."
    );

    let base = || augment(Command::new("hanzo"));
    for (op, f) in &secrets {
        assert!(
            !f.query,
            "{}: a secret is a body field, never a query param",
            op.path
        );
        let mut argv = vec!["hanzo".to_string(), op.product.to_string()];
        argv.extend(op.nodes.iter().map(|n| n.to_string()));
        argv.push(op.verb.to_string());
        for p in op.params {
            argv.push(format!("v-{p}"));
        }
        for leak in [f.flag, "secret", "value"] {
            let mut a = argv.clone();
            a.push(format!("--{leak}"));
            a.push("hunter2".into());
            assert!(
                base().try_get_matches_from(&a).is_err(),
                "value-bearing argv must not parse: {a:?}"
            );
        }
    }
}

/// THE THREE OPERATIONS THAT STORE A SEALED VALUE take it from stdin, never a
/// flag: `environment secrets set`, `kms secrets create` and `notify credentials
/// set` call the field `value`, a name no name rule catches, so it is the
/// operation that marks it. A `--value`
/// would put a live credential in shell history and the process list, so it must
/// not parse, and the one secret field is the `value` stdin fills (`inject_secret`
/// inserts it under its key).
#[test]
fn the_value_of_a_stored_secret_has_no_flag() {
    let store = [
        ("/v1/environment/{repo}/secrets/{name}", "PUT"),
        ("/v1/kms/secrets", "POST"),
        ("/v1/notify/credentials/{provider}/{key}", "PUT"),
    ];
    for (path, method) in store {
        let op = OPS
            .iter()
            .find(|o| o.path == path && o.method == method)
            .unwrap_or_else(|| panic!("{method} {path} is served"));
        let secrets: Vec<&Field> = op.fields.iter().filter(|f| f.secret).collect();
        assert_eq!(secrets.len(), 1, "{path}: stdin fills exactly one field");
        assert_eq!(
            secrets[0].key, "value",
            "{path}: the body is {{\"value\": <stdin>}}"
        );
    }
    let base = || augment(Command::new("hanzo"));
    assert!(
        base()
            .try_get_matches_from([
                "hanzo",
                "environment",
                "secrets",
                "set",
                "r",
                "N",
                "--value",
                "s"
            ])
            .is_err(),
        "`--value <literal>` must not parse"
    );
    assert!(
        base()
            .try_get_matches_from([
                "hanzo", "kms", "secrets", "create", "--name", "N", "--value", "s"
            ])
            .is_err(),
        "`--value <literal>` must not parse"
    );
    assert!(
        base()
            .try_get_matches_from([
                "hanzo",
                "notify",
                "credentials",
                "provider",
                "set",
                "p",
                "k",
                "--value",
                "s"
            ])
            .is_err(),
        "`--value <literal>` must not parse"
    );
}

/// The hidden prompt names the command a person is running, in full: product,
/// resource nodes and verb. It used to drop the nodes, so a person storing an
/// environment secret was asked for a value for `hanzo environment set` (another
/// real command) and a kms one for `hanzo kms create` (no such command).
#[test]
fn the_prompt_names_the_command_being_run() {
    let label = |path: &str, method: &str| {
        let op = OPS
            .iter()
            .find(|o| o.path == path && o.method == method)
            .unwrap_or_else(|| panic!("{method} {path} is served"));
        let f = op.fields.iter().find(|f| f.secret).expect("a secret field");
        prompt_label(op, f)
    };
    assert_eq!(
        label("/v1/environment/{repo}/secrets/{name}", "PUT"),
        "value for `hanzo environment secrets set`"
    );
    assert_eq!(
        label("/v1/kms/secrets", "POST"),
        "value for `hanzo kms secrets create`"
    );
}

/// The org binds to the active identity's owner and is never an argument: kms
/// moved the org out of the URL, and no verb may take it back as a flag.
#[test]
fn no_kms_verb_takes_an_org() {
    let base = || augment(Command::new("hanzo"));
    let orged: &[&[&str]] = &[
        &["hanzo", "kms", "secrets", "list", "--org", "other"],
        &["hanzo", "kms", "secrets", "get", "DB", "--org", "other"],
        &["hanzo", "kms", "secrets", "rm", "DB", "--org", "other"],
    ];
    for argv in orged {
        assert!(
            base().try_get_matches_from(*argv).is_err(),
            "no --org may exist: {argv:?}"
        );
    }
}

/// Defense in depth: if the derive tree already owns a name that a FUTURE spec
/// turns into a product, the local command still wins — augment skips it.
#[test]
fn augment_never_clobbers_an_existing_command() {
    // `world` IS a generated product; pin that a same-named local wins.
    assert!(is_product("world"), "precondition: world is a product");
    let base = Command::new("hanzo").subcommand(Command::new("world").about("LOCAL-MARKER"));
    let merged = augment(base);
    let world = merged.find_subcommand("world").expect("world present");
    assert_eq!(
        world.get_about().map(|s| s.to_string()).as_deref(),
        Some("LOCAL-MARKER")
    );
    // exactly one `world`, and it is the local.
    assert_eq!(
        merged
            .get_subcommands()
            .filter(|s| s.get_name() == "world")
            .count(),
        1
    );
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

// ---- an address param: one segment by default, raw slashes when marked --------

/// `fill_path` is asserted here as a FUNCTION, over an address written out in
/// full, because that is what it is. Which routes carry a multi-segment param is
/// the document's answer and it moves: cloud mounted `/v1/kms/secrets/*` when this
/// was written and mounts no catch-all at all today, so a test that reached for a
/// live example asserted nothing the day the last one was typed away.
///
/// Two behaviours, and the difference is the whole point. A param the operation
/// marks MULTI-SEGMENT lets the slashes ride raw, so `prod/db/password` addresses
/// the folder-scoped secret it names rather than arriving as `prod%2Fdb%2F…` at a
/// route that then 404s. A param that is NOT marked escapes the slash, so a value
/// can never split into extra segments and re-address a different route.
///
/// Both refuse `.`/`..`/empty BEFORE a URL exists, and neither lets the active org
/// into the URL — an org is read from the validated principal, and one that cannot
/// be spelled cannot be swapped.
#[test]
fn a_marked_address_param_keeps_its_slashes_and_an_unmarked_one_escapes_them() {
    let path = "/v1/kms/secrets/{name}";
    let multi: &[&str] = &["name"];

    let filled = fill_path(path, multi, Some("acme"), &["prod/db/password".into()]).unwrap();
    assert_eq!(filled, "/v1/kms/secrets/prod/db/password");

    // A FLAT name is untouched — one segment, no slash.
    assert_eq!(
        fill_path(path, multi, Some("acme"), &["DB".into()]).unwrap(),
        "/v1/kms/secrets/DB"
    );

    // Each segment is STILL percent-encoded: a space or `?` cannot re-address.
    assert_eq!(
        fill_path(path, multi, Some("acme"), &["a b/x?y".into()]).unwrap(),
        "/v1/kms/secrets/a%20b/x%3Fy"
    );

    // The active org is handed in and does NOT appear.
    assert!(
        !filled.contains("acme"),
        "the org must not reach the URL: {filled}"
    );

    // Traversal and empty segments are refused before a URL is built.
    for evil in [
        "../../evil/k",
        "a/../b",
        "a//b",
        "/leading",
        "trailing/",
        ".",
    ] {
        assert!(
            fill_path(path, multi, Some("acme"), &[evil.into()]).is_err(),
            "a multi-segment address must refuse {evil:?}"
        );
    }

    // UNMARKED is the default, and it escapes the slash.
    assert_eq!(
        fill_path(
            "/v1/agents/sessions/{id}",
            &[],
            Some("acme"),
            &["a/b".into()]
        )
        .unwrap(),
        "/v1/agents/sessions/a%2Fb"
    );
}

// ---- a 2xx with an error envelope is a failure, never a silent success --------

