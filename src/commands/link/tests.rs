use super::fabric::{listens_in, tunnels, Tunnel};
use super::network::tests::Fake;
use super::*;

    /// A tmux server of the test's own, at a socket in a temp dir, stopped when
    /// the test ends: a link test must never reach the server a person works in.
    struct Server {
        _dir: tempfile::TempDir,
        tmux: Tmux,
    }

    impl Server {
        fn start() -> Server {
            let dir = tempfile::tempdir().unwrap();
            let tmux = Tmux(Some(dir.path().join("tmux.sock")));
            Server { _dir: dir, tmux }
        }
    }

    impl Drop for Server {
        fn drop(&mut self) {
            if let (Some(bin), Some(socket)) = (hanzo_gen::exe::find("tmux"), &self.tmux.0) {
                let _ = hanzo_gen::exe::command(bin)
                    .arg("-S")
                    .arg(socket)
                    .arg("kill-server")
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .status();
            }
        }
    }
    use anyhow::anyhow;

    /// The tunnel signs in with `--as <org>` only for an identity held in that
    /// org; for any other `--as` the default signs in, as before.
    #[test]
    fn the_tunnel_carries_as_only_for_a_held_identity() {
        let mut cfg = Config::default();
        for owner in ["hanzo", "admin"] {
            cfg.auth.identities.push(crate::config::StoredIdentity {
                brand: "hanzo".into(),
                owner: owner.into(),
                name: "z".into(),
            });
        }
        cfg.auth.active.insert("hanzo".into(), "hanzo/z".into());
        let command = |cfg: &Config| Signer::default().tunnel_command(cfg).unwrap();
        assert!(command(&cfg).ends_with(" auth token") && !command(&cfg).contains("--as"));
        cfg.org = Some("admin".into());
        assert!(
            command(&cfg).ends_with(" --as admin auth token"),
            "{}",
            command(&cfg)
        );
        cfg.org = Some("lux".into());
        assert!(!command(&cfg).contains("--as"), "{}", command(&cfg));
    }

    /// While a link serves, the machine's own heartbeat carries the shell's url, and
    /// when it ends the machine says it is gone — before its beat is dropped.
    #[tokio::test]
    async fn while_serving_the_machines_beat_carries_the_terminal() {
        struct Signed;
        impl hanzo_gen::Host for Signed {
            fn profile(&self) -> hanzo_gen::host::Pending<'_, hanzo_gen::Profile> {
                Box::pin(async { hanzo_gen::Profile::unread() })
            }
            fn account<'a>(
                &'a self,
                _: &'a str,
            ) -> hanzo_gen::host::Pending<'a, Result<Option<hanzo_gen::Account>>> {
                Box::pin(async {
                    Ok(Some(hanzo_gen::Account {
                        owner: "acme".into(),
                        name: "box".into(),
                        token: "T".into(),
                    }))
                })
            }
            fn refresh(&self, _: &str) -> Vec<String> {
                Vec::new()
            }
            fn key(&self, _: hanzo_gen::Provider) -> Result<Option<String>> {
                Ok(None)
            }
            fn meter<'a>(
                &'a self,
                _: &'a str,
                _: &'a [String],
            ) -> hanzo_gen::host::Pending<'a, std::result::Result<serde_json::Value, String>> {
                Box::pin(async { Err("no meter".to_string()) })
            }
        }
        let fake = Fake::serve(200, r#"{"id":"tgt_1"}"#).await;
        let dir = tempfile::tempdir().unwrap();
        let root = Root::at(dir.path());
        let machine = target::beat(std::sync::Arc::new(Signed), &root, "hanzo", &fake.base, "mach_1", "box");
        let said = |url: &str| {
            fake.all()
                .iter()
                .any(|r| r.1.starts_with("/v1/agent/targets") && r.4.contains(&format!(r#""terminal":"{url}""#)))
        };

        let held = async {
            for _ in 0..3000 {
                if said("https://box.share.hanzo.ai") {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            panic!("the machine never stated the terminal");
        };
        on_machine(&machine, "https://box.share.hanzo.ai", held).await;
        assert!(said(""), "the machine withdrew the terminal when the link ended");
    }

    /// A link that ended cleanly is DONE — and saying so is what stops the row
    /// from outliving the shell. The ending is one PATCH carrying both facts, so
    /// the console can never see a finished session still advertising a terminal.
    #[tokio::test]
    async fn a_clean_exit_closes_the_session_and_withdraws_the_terminal() {
        let fake = Fake::serve(200, "{}").await;
        let client = SessionClient::new(&fake.base, "T").unwrap();

        let server = Server::start();
        finish(&server.tmux, &client, Some("sess_1"), Ok(()))
            .await
            .unwrap();
        // The bar it cleared was the test's own server's, not the one a person works in.
        if hanzo_gen::exe::find("tmux").is_some() {
            assert!(
                server.tmux.0.as_ref().unwrap().exists(),
                "finish spoke to some other tmux server"
            );
        }

        // One act, one request.
        let (method, path, _, _, body) = fake.one();
        let body: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(method, "PATCH");
        assert_eq!(path, "/v1/agents/sessions/sess_1");
        assert_eq!(body["status"], "done");
        assert_eq!(body["terminal"], "");
    }

    /// The failing exits are the ones that used to leak: a tunnel that died, a
    /// publish that never landed, a shell that could not start. They END the
    /// session too — as an error — and the caller still gets its error back
    /// unchanged, because recording an ending must not swallow one.
    #[tokio::test]
    async fn a_failed_link_ends_as_an_error_and_still_reports_it() {
        let fake = Fake::serve(200, "{}").await;
        let client = SessionClient::new(&fake.base, "T").unwrap();

        let server = Server::start();
        let err = finish(
            &server.tmux,
            &client,
            Some("sess_1"),
            Err(anyhow!("share ended: exit 1")),
        )
        .await
        .unwrap_err();

        assert!(err.to_string().contains("share ended"), "got: {err}");
        let reqs = fake.all();
        assert_eq!(
            reqs.len(),
            2,
            "failed link emits closing status event then terminal PATCH: {reqs:?}"
        );
        let ev: serde_json::Value = serde_json::from_str(&reqs[0].4).unwrap();
        assert_eq!(reqs[0].0, "POST");
        assert_eq!(reqs[0].1, "/v1/agents/sessions/sess_1/events");
        assert_eq!(ev["kind"], "status");
        assert_eq!(ev["payload"]["status"], "error");
        assert_eq!(ev["payload"]["error"], "share ended: exit 1");

        let body: serde_json::Value = serde_json::from_str(&reqs[1].4).unwrap();
        assert_eq!(reqs[1].0, "PATCH");
        assert_eq!(reqs[1].1, "/v1/agents/sessions/sess_1");
        assert_eq!(body["status"], "error");
        assert_eq!(body["terminal"], "");
        assert_eq!(body["error"], "share ended: exit 1");
    }

    /// Cloud being unreachable costs a stale row, never a failed command: the
    /// shell already ran, and refusing to return its result because a PATCH 403'd
    /// would be reporting the wrong thing.
    #[tokio::test]
    async fn a_control_plane_that_refuses_the_close_does_not_fail_the_link() {
        let fake = Fake::serve(403, "{}").await;
        let client = SessionClient::new(&fake.base, "T").unwrap();

        let server = Server::start();
        assert!(finish(&server.tmux, &client, Some("sess_1"), Ok(()))
            .await
            .is_ok());
    }

    // ONE LINK, MANY SHELLS. The default runs a wrapper rather than a fixed tmux
    // command, so a pane can ask for a shell by name over the SAME tunnel — one
    // port, one gate sign-in, N independent tmux sessions. Proven against ttyd
    // 1.7.7: connecting with ?arg=alpha and ?arg=beta created two separate
    // sessions from one server.
    #[test]
    fn the_default_serves_a_shell_the_url_can_name() {
        let sh = Shell::from_flag(None);
        assert!(sh.url_arg(), "the query has to be able to name a shell");
        let argv = sh.argv();
        // `sh -c SCRIPT $0` — the placeholder is what puts the client's arg in $1.
        assert_eq!(argv[0], "sh");
        assert_eq!(argv[1], "-c");
        assert_eq!(
            argv[3], "hanzo-shell",
            "a $0 placeholder, so the arg lands in $1"
        );
        assert!(argv[2].contains("tmux new -A -s"), "{}", argv[2]);
    }

    // `tmux` names what the default already is; it must not become a second way.
    #[test]
    fn asking_for_tmux_asks_for_the_default() {
        assert_eq!(
            Shell::from_flag(Some("tmux")).argv(),
            Shell::from_flag(None).argv()
        );
    }

    // A NAMED COMMAND TAKES NO URL ARGUMENT, and this is the security-critical
    // half. `--url-arg` appends the query to argv, so `--shell bash` plus
    // `?arg=-c&arg=whoami` would be `bash -c whoami` — remote execution handed
    // over by a query string. The wrapper is written to be given arguments; a raw
    // command is not, so it is never offered them.
    #[test]
    fn a_named_shell_is_never_given_the_url() {
        for cmd in ["bash", "zsh", "fish"] {
            let sh = Shell::from_flag(Some(cmd));
            assert_eq!(sh.argv(), vec![cmd.to_string()]);
            assert!(!sh.url_arg(), "`--shell {cmd}` must not read the query");
        }
    }

    /// The kernel's own table decides whether a proxy still listens: LISTEN is
    /// state 0A, and the port is the hex after the local address's colon.
    #[test]
    fn a_listener_is_read_off_the_kernels_socket_table() {
        let table = "  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode
   0: 00000000:66DB 00000000:0000 0A 00000000:00000000 00:00000000 00000000  1000        0 1 1 0 100 0 0 10 0
   1: 0100007F:1F90 0100007F:D431 01 00000000:00000000 00:00000000 00000000  1000        0 2 1 0 20 4 30 10 -1";
        assert!(listens_in(table, 26331), "0x66DB listens");
        assert!(
            !listens_in(table, 8080),
            "0x1F90 is an established connection, not a listener"
        );
        assert!(!listens_in(table, 26443));
    }

    /// A tunnel is found by what it runs, whichever unit or shell started it.
    #[test]
    fn tunnels_are_read_off_the_process_table() {
        let ps = "\
            1 /sbin/init\n\
            4021838 /usr/local/bin/zt tunnel host --controller https://zt-api.hanzo.ai --token-command /etc/hanzo/link/token\n\
            3804505 /usr/local/bin/zt tunnel proxy k8s.hanzo:26443 --controller https://zt-api.hanzo.ai --token-command /home/z/.local/bin/hanzo auth token\n\
            77 /usr/bin/vim zt tunnel\n\
            78 zt version\n";
        assert_eq!(
            tunnels(ps),
            [
                Tunnel {
                    pid: 4021838,
                    what: "host".into(),
                    port: None
                },
                Tunnel {
                    pid: 3804505,
                    what: "proxy k8s.hanzo:26443".into(),
                    port: Some(26443)
                },
            ]
        );
    }

    // The name reaching tmux is bounded BEFORE tmux sees it: `;` is tmux's own
    // command separator, and --url-arg lets a caller send several args.
    #[test]
    fn named_shell_skips_tmux_check_and_falls_back_cleanly_without_ttyd() {
        let zsh = Shell::from_flag(Some("zsh"));
        assert_eq!(
            dependencies::missing(matches!(zsh, Shell::Multiplexed), |_| false),
            ["ttyd"]
        );
        let mux = Shell::from_flag(None);
        assert_eq!(
            dependencies::missing(matches!(mux, Shell::Multiplexed), |b| b == "sh"
                || b == "tr"
                || b == "cut"),
            ["ttyd", "tmux"]
        );
    }

    #[test]
    fn the_wrapper_strips_a_name_down_to_something_safe() {
        let script = &Shell::from_flag(None).argv()[2];
        assert!(
            script.contains("tr -cd"),
            "the name must be reduced to a rune set"
        );
        assert!(script.contains("cut -c1-32"), "and bounded in length");
        assert!(script.contains("${1:-hanzo}"), "and read from $1 alone");
        assert!(
            !script.contains("$2"),
            "extra arguments are ignored, never used"
        );
    }
