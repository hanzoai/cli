use super::*;
use super::tests::{hand, matches_of};
use crate::commands::cloud_sessions::finished;
use clap::Command;
use hanzo_client::Reply;

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
    assert_eq!(
        envelope_error(&json!({"error": "bad request"})).as_deref(),
        Some("bad request")
    );

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

/// A resource whose own state is `error` or `failed` is a success to render.
/// `agent sessions get` on a run that died at clone answered with the run's error
/// as the CLI's, so a failed run could not be read at all.
#[test]
fn a_failed_resource_is_rendered_not_refused() {
    use serde_json::json;

    let run = json!({
        "id": "sess_39509ae3f553c09110030c113eda4382",
        "kind": "coding",
        "status": "error",
        "error": "coding: clone: exit 128: fatal: unable to access 'https://github.com/hanzo-inc/cloud.git/'",
        "events": [{"kind": "message", "content": "the prompt"}],
    });
    assert!(envelope_error(&run).is_none());
    assert!(envelope_error(&json!({"id": "b1", "status": "failed", "error": "exit 1"})).is_none());
    assert!(envelope_error(&json!({"id": "d1", "error": "rollout stalled"})).is_none());

    // IAM's whole envelope is still an envelope.
    assert_eq!(
        envelope_error(&json!({
            "status": "error", "msg": "Unauthorized operation",
            "sub": "", "name": "", "data": null, "data2": null,
        }))
        .as_deref(),
        Some("Unauthorized operation")
    );
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
    "OpenAI-compatible",
    "Anthropic-compatible",
    "ChatGPT",
    "ngrok",
    "zrok",
    "caddy",
    "Caddy",
    "nginx",
    "PostHog",
    "Stripe",
    "Casbin",
    "Casdoor",
    "gh auth",
    "Prometheus",
    "Grafana",
    "Datadog",
    "Auth0",
    "Okta",
    "Vercel",
    "Heroku",
    "Cloudflare Workers",
    "LangChain",
    "Ollama",
    "HuggingFace",
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
        for text in [cmd.get_about(), cmd.get_long_about()]
            .into_iter()
            .flatten()
        {
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
    assert!(
        copy.len() > 100,
        "walked {} strings — the walk is broken",
        copy.len()
    );

    let mut found = Vec::new();
    for (where_, text) in &copy {
        for v in FOREIGN {
            if text.contains(v) {
                found.push(format!("{where_}: {v:?} in {text:?}"));
            }
        }
    }
    assert!(
        found.is_empty(),
        "outside vendor named in customer-facing help:\n  {}",
        found.join("\n  ")
    );
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
        "add", "get", "set", "put", "delete", "update", "create", "remove", "list", "run", "check",
        "send", "mint", "issue", "revoke", "reset", "refresh", "sync", "is", "place", "pay",
        "commit", "query", "upload", "resolve",
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
    assert!(
        bad.is_empty(),
        "verb-noun command names — name the thing, let the position say the verb:\n  {}",
        bad.join("\n  ")
    );
}

/// A UNION flag says what it looks like: a JSON object or array reaches the body
/// as that value, anything else as the text typed. The field is stated here, read
/// through the builder every generated command uses: since cloud 8.5.640 the
/// document types `state` on POST /v1/decisions as an untyped value, so no
/// operation declares a `string | object | array` property to find.
#[test]
fn a_union_flag_sends_json_as_json_and_text_as_text() {
    static STATE: Field = Field {
        key: "state",
        id: "field.state",
        flag: "state",
        ty: Ty::Union,
        required: false,
        choices: &[],
        query: false,
        secret: false,
        repeat: false,
    };
    let send = |value: &str| {
        let m = Command::new("t")
            .arg(field_arg(&STATE))
            .try_get_matches_from(["t", "--state", value])
            .expect("parses");
        m.get_one::<Value>(STATE.id).cloned().expect("a value")
    };
    assert_eq!(
        send(r#"{"message":"charged twice"}"#),
        serde_json::json!({"message": "charged twice"})
    );
    assert_eq!(send(r#"["a","b"]"#), serde_json::json!(["a", "b"]));
    assert_eq!(send("charged twice"), serde_json::json!("charged twice"));
    // A scalar that happens to parse as JSON is still text: the union admits no number.
    assert_eq!(send("42"), serde_json::json!("42"));
}

// ---- a machine caller's key --------------------------------------------------

/// HANZO_API_KEY is the bearer of a caller that holds only a key — a bot in a
/// sandbox, a CI step. Blank or unset is no key at all, so the stored login (or the
/// `not signed in` refusal) still decides; a pasted key's whitespace is dropped.
#[test]
fn a_machine_key_is_the_variable_trimmed_and_nothing_when_blank() {
    use std::ffi::OsString;
    assert_eq!(machine_key(None), None);
    assert_eq!(machine_key(Some(OsString::from(""))), None);
    assert_eq!(machine_key(Some(OsString::from("  \n"))), None);
    assert_eq!(
        machine_key(Some(OsString::from("sk-live-x"))),
        Some("sk-live-x".into())
    );
    assert_eq!(
        machine_key(Some(OsString::from(" sk-live-x\n"))),
        Some("sk-live-x".into())
    );
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStringExt;
        assert_eq!(
            machine_key(Some(OsString::from_vec(vec![0xff, 0xfe]))),
            None,
            "not text is no key"
        );
    }
}

// ---- the answer: one formatter, `-o table|json|yaml` -------------------------

#[test]
fn a_list_is_a_table_with_the_scalar_keys_as_columns() {
    let list = json!([
        {"id": "zen6", "owned_by": "hanzo", "context": 262144, "tags": ["a"]},
        {"id": "zen5", "owned_by": "hanzo", "context": null},
    ]);
    let text = table(&list, usize::MAX);
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines[0], "ID    CONTEXT  OWNED_BY");
    assert_eq!(lines[1], "zen6  262144   hanzo");
    assert_eq!(lines[2], "zen5  -        hanzo");
    assert_eq!(lines.len(), 3, "a nested value is not a column");
}

#[test]
fn a_column_too_wide_is_left_out_and_the_narrow_ones_after_it_are_kept() {
    let list = json!([{"id": "a", "long": "x".repeat(50), "n": 1}]);
    assert_eq!(table(&list, 20).lines().next().unwrap(), "ID  N");
}

#[test]
fn the_columns_that_fit_are_shown_and_the_first_always() {
    let list = json!([{"id": "zen6", "owned_by": "hanzo", "context": 1}]);
    assert_eq!(table(&list, 13).lines().next().unwrap(), "ID    CONTEXT");
    assert_eq!(table(&list, 1).lines().next().unwrap(), "ID");
}

#[test]
fn the_one_array_an_object_carries_is_its_list_and_an_object_is_field_rows() {
    let envelope = json!({"object": "list", "data": [{"id": "a"}, {"id": "b"}]});
    assert_eq!(table(&envelope, usize::MAX), "ID\na\nb");
    let one = json!({"id": "a", "n": 2, "nested": {"x": 1}});
    assert_eq!(table(&one, usize::MAX), "FIELD  VALUE\nid     a\nn      2");
}

#[test]
fn json_and_yaml_render_the_same_value() {
    let v = json!({"id": "a", "n": [1, 2]});
    assert_eq!(
        render(&v, Output::Json, 80).unwrap(),
        serde_json::to_string_pretty(&v).unwrap()
    );
    let yaml = render(&v, Output::Yaml, 80).unwrap();
    assert_eq!(serde_norway::from_str::<Value>(&yaml).unwrap(), v, "{yaml}");
    assert_eq!(render(&Value::Null, Output::Table, 80), None);
    assert_eq!(render(&json!("plain"), Output::Yaml, 80).unwrap(), "plain");
}

/// `-o` is on every generated command and names the format; without it the command line gives none.
#[test]
fn the_output_flag_is_on_every_generated_command() {
    let op = OPS
        .iter()
        .find(|o| o.method == "GET" && o.params.is_empty() && o.fields.is_empty())
        .expect("a bare read");
    let mut argv = vec!["hanzo".to_string(), op.product.to_string()];
    argv.extend(op.nodes.iter().map(|n| n.to_string()));
    argv.push(op.verb.to_string());
    let bare: Vec<&str> = argv.iter().map(String::as_str).collect();
    let Some(Resolved::Leaf { output, .. }) = resolve(&hand(), &matches_of(&bare)) else {
        panic!("leaf")
    };
    assert_eq!(output, None);
    for (flag, want) in [
        ("table", Output::Table),
        ("json", Output::Json),
        ("yaml", Output::Yaml),
    ] {
        let mut given = bare.clone();
        given.extend(["-o", flag]);
        let Some(Resolved::Leaf { output, .. }) = resolve(&hand(), &matches_of(&given)) else {
            panic!("leaf")
        };
        assert_eq!(output, Some(want));
    }
    let mut wrong = bare.clone();
    wrong.extend(["--output", "xml"]);
    assert!(
        augment(hand()).try_get_matches_from(&wrong).is_err(),
        "xml is not a format"
    );
}

/// An operation that declares a `requestId` is given one per invocation, and keeps the one a caller gave.
#[test]
fn an_operation_that_declares_a_request_id_is_given_one() {
    let op = OPS
        .iter()
        .find(|o| o.fields.iter().any(|f| f.key == "requestId" && !f.query))
        .expect("cloud declares a requestId on a write");
    let body = named(op, json!({}));
    let id = body["requestId"].as_str().unwrap().to_string();
    assert!(id.starts_with("cli-") && id.len() > 20, "{id}");
    assert_ne!(
        named(op, json!({}))["requestId"],
        id,
        "one id per invocation"
    );
    assert_eq!(named(op, json!({"requestId": "mine"}))["requestId"], "mine");
}

/// `hanzo completions create` is chat completions: it goes to
/// `/v1/chat/completions`, never to `/v1/completions`.
#[test]
fn completions_create_posts_to_chat_completions() {
    let op = OPS
        .iter()
        .find(|o| o.product == "completions" && o.verb == "create")
        .expect("completions create");
    assert_eq!((op.method, op.path), ("POST", "/v1/chat/completions"));
    assert!(OPS.iter().all(|o| o.path != "/v1/completions"));
}

/// A generated boolean body field must distinguish omission from explicit
/// false, while keeping the original bare-flag spelling for true.
#[test]
fn scalar_boolean_field_preserves_explicit_false_and_optional_omission() {
    static ENABLED: Field = Field {
        key: "enabled",
        id: "field.enabled",
        flag: "enabled",
        ty: Ty::Bool,
        required: false,
        choices: &[],
        query: false,
        secret: false,
        repeat: false,
    };
    static NAME: Field = Field {
        key: "name",
        id: "field.name",
        flag: "name",
        ty: Ty::Str,
        required: false,
        choices: &[],
        query: false,
        secret: false,
        repeat: false,
    };
    let value = |args: Vec<&str>| {
        let matches = Command::new("test")
            .arg(field_arg(&ENABLED))
            .arg(field_arg(&NAME))
            .try_get_matches_from(args)
            .expect("parses");
        field_value(&ENABLED, &matches)
    };
    assert_eq!(value(vec!["test"]), None);
    assert_eq!(value(vec!["test", "--enabled"]), Some(Value::Bool(true)));
    assert_eq!(
        value(vec!["test", "--enabled", "--name", "auto"]),
        Some(Value::Bool(true))
    );
    assert_eq!(
        value(vec!["test", "--enabled", "true"]),
        Some(Value::Bool(true))
    );
    assert_eq!(
        value(vec!["test", "--enabled=true"]),
        Some(Value::Bool(true))
    );
    assert_eq!(
        value(vec!["test", "--enabled", "false"]),
        Some(Value::Bool(false))
    );
    assert_eq!(
        value(vec!["test", "--enabled=false"]),
        Some(Value::Bool(false))
    );
}

// ---- one-shot bots: agent run may leave the gateway before the run does -----

/// The async controls belong to ONE op. A generated command is a projection of
/// cloud's document; a control that leaked to every write would invent a shape
/// the document never declared.
#[test]
fn exactly_one_agent_run_has_the_async_controls() {
    let hits: Vec<_> = OPS.iter().filter(|o| is_agent_run(o)).collect();
    assert_eq!(hits.len(), 1, "the async controls must name one op");
    assert_eq!(hits[0].path, "/v1/agent/{ref}/run");
    assert_eq!(hits[0].method, "POST");

    let merged = augment(hand());
    assert!(
        merged
            .clone()
            .try_get_matches_from(["hanzo", "agent", "coding", "create", "--async"])
            .is_err(),
        "--async must not leak to another op"
    );
    assert!(
        merged
            .try_get_matches_from(["hanzo", "agent", "run", "agt_1", "--wait"])
            .is_err(),
        "--wait without --async must be refused"
    );
}

#[test]
fn agent_run_stays_sync_unless_asked_and_parses_the_wait_deadline() {
    let submit = |argv: &[&str]| -> Submit {
        let m = matches_of(argv);
        let Some(Resolved::Leaf { submit, .. }) = resolve(&hand(), &m) else {
            panic!("expected a leaf: {argv:?}")
        };
        submit
    };
    assert_eq!(
        submit(&["hanzo", "agent", "run", "agt_1"]),
        Submit::Sync,
        "the default contract is unchanged"
    );
    assert_eq!(
        submit(&[
            "hanzo",
            "agent",
            "run",
            "agt_1",
            "--data",
            r#"{"prompt":"x"}"#
        ]),
        Submit::Sync
    );
    assert_eq!(
        submit(&["hanzo", "agent", "run", "agt_1", "--async"]),
        Submit::Async
    );
    assert_eq!(
        submit(&["hanzo", "agent", "run", "agt_1", "--async", "--wait"]),
        Submit::Wait(300)
    );
    assert_eq!(
        submit(&[
            "hanzo",
            "agent",
            "run",
            "agt_1",
            "--async",
            "--wait",
            "--timeout",
            "12"
        ]),
        Submit::Wait(12)
    );
    assert!(
        augment(hand())
            .try_get_matches_from([
                "hanzo",
                "agent",
                "run",
                "agt_1",
                "--async",
                "--timeout",
                "12"
            ])
            .is_err(),
        "--timeout without --wait must be refused"
    );
}

#[test]
fn the_async_body_targets_the_same_agent_without_inventing_fields() {
    let body = async_body(
        "agt_1",
        Some(json!({"prompt": "fix it", "repo": "acme/app"})),
    )
    .unwrap();
    assert_eq!(body["agentRef"], json!("agt_1"));
    assert_eq!(body["prompt"], json!("fix it"));
    assert_eq!(body["repo"], json!("acme/app"));
    assert!(
        body["requestId"]
            .as_str()
            .is_some_and(|id| id.starts_with("cli-")),
        "a retry must be one run"
    );
    let body = async_body("agt_1", Some(json!({"requestId": "mine"}))).unwrap();
    assert_eq!(body["requestId"], json!("mine"), "a caller id wins");
    assert!(async_body("agt_1", Some(json!(["x"]))).is_err());
    assert!(async_body("", None).is_err());
    assert_eq!(
        async_body("agt_1", None).unwrap()["agentRef"],
        json!("agt_1")
    );
}

#[test]
fn the_async_handle_is_unwrapped_and_validated_before_it_is_followed() {
    assert_eq!(
        accepted_session(Reply {
            status: 202,
            body: json!({"data": {"sessionId": "sess_1"}}),
        })
        .unwrap(),
        "sess_1"
    );
    assert_eq!(
        accepted_session(Reply {
            status: 200,
            body: json!({"sessionId": "sess_2"}),
        })
        .unwrap(),
        "sess_2"
    );
    assert!(accepted_session(Reply {
        status: 524,
        body: json!({"error": "gateway timeout"}),
    })
    .is_err());
    assert!(accepted_session(Reply {
        status: 200,
        body: json!({"status": "error", "msg": "held"}),
    })
    .is_err());
    for id in ["", "../x", "s/x", "s?x=1"] {
        assert!(
            accepted_session(Reply {
                status: 202,
                body: json!({"sessionId": id}),
            })
            .is_err(),
            "{id} must never reach a follow-up URL"
        );
    }
    assert!(finished("done") && finished("error") && finished("stopped"));
    assert!(!finished("running") && !finished("paused"));
}

#[test]
fn repeatable_json_flag_accepts_both_array_and_repeated_objects() {
    let body_of = |argv: &[&str]| -> Value {
        let m = matches_of(argv);
        let Some(Resolved::Leaf {
            body: LeafBody::Typed(v),
            ..
        }) = resolve(&hand(), &m)
        else {
            panic!("expected typed leaf body: {argv:?}");
        };
        v
    };

    // Single JSON array passed to a repeatable JSON flag (e.g. `git repos push --files '[…]'`)
    // must NOT double-wrap into `[[{"path":…}]]`, which fails server json.Unmarshal.
    let from_array = body_of(&[
        "hanzo",
        "git",
        "repos",
        "push",
        "cd-clock-test",
        "--files",
        r#"[{"path":"index.html","content":"<h1>hi</h1>"},{"path":"a.txt","content":"ok"}]"#,
    ]);
    assert_eq!(
        from_array["files"],
        json!([
            {"path": "index.html", "content": "<h1>hi</h1>"},
            {"path": "a.txt", "content": "ok"}
        ])
    );

    // Repeated item objects still collect into a flat JSON array.
    let from_repeated = body_of(&[
        "hanzo",
        "git",
        "repos",
        "push",
        "cd-clock-test",
        "--files",
        r#"{"path":"index.html","content":"<h1>hi</h1>"}"#,
        "--files",
        r#"{"path":"a.txt","content":"ok"}"#,
    ]);
    assert_eq!(from_repeated["files"], from_array["files"]);
}
