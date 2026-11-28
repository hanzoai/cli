use super::kube::*;
use super::manifest::*;
use super::*;
use crate::commands::vm::wire::{launched, line, peer};
use base64::Engine;
use serde_json::json;

fn boot() -> Boot {
    Boot {
        cpus: 4,
        memory_mb: 4096,
        disk_mb: 16384,
        cloud: "ghcr.io/hanzoai/cloud@sha256:c10d".into(),
        staged: None,
        iam_issuer: None,
        ai_base_url: None,
        strict_registry: false,
        local: None,
    }
}

#[test]
fn select_cloud_image_prefers_hanzo_oci_and_enforces_strict_registry() {
    assert_eq!(select_cloud_image(CLOUD, false, true).unwrap(), CLOUD_OCI);
    assert_eq!(select_cloud_image(CLOUD, false, false).unwrap(), CLOUD);
    assert_eq!(select_cloud_image(CLOUD, true, false).unwrap(), CLOUD_OCI);
    assert!(select_cloud_image("ghcr.io/hanzoai/cloud-oss@sha256:aa", true, true).is_err());
    assert_eq!(
        select_cloud_image("oci.hanzo.ai/hanzoai/cloud-oss@sha256:aa", true, true).unwrap(),
        "oci.hanzo.ai/hanzoai/cloud-oss@sha256:aa"
    );
}

#[test]
fn workload_for_injects_staged_iam_and_ai_env_and_changes_measurement() {
    let plain = workload("oci.hanzo.ai/hanzoai/cloud-oss@sha256:aa");
    let fc = workload_for(&Boot {
        cpus: 4,
        memory_mb: 4096,
        disk_mb: 16384,
        cloud: "oci.hanzo.ai/hanzoai/cloud-oss@sha256:aa".into(),
        staged: Some("fanclub".into()),
        iam_issuer: Some("https://hanzo.ai".into()),
        ai_base_url: Some("https://api.hanzo.ai".into()),
        strict_registry: true,
        local: None,
    })
    .unwrap();
    assert!(plain.contains("name: IAM_DEV_BOOTSTRAP"), "{plain}");
    assert!(plain.contains("name: IAM_DEV_HOST_RELATIVE"), "{plain}");
    assert!(fc.contains("name: IAM_DEV_BOOTSTRAP"), "{fc}");
    assert!(!fc.contains("name: IAM_DEV_HOST_RELATIVE"), "{fc}");
    assert!(fc.contains("name: CLOUD_ENABLE_STAGED"), "{fc}");
    assert!(fc.contains("value: \"fanclub\""), "{fc}");
    assert!(fc.contains("name: CLOUD_IAM_ISSUER"), "{fc}");
    assert!(fc.contains("value: \"https://hanzo.ai\""), "{fc}");
    assert!(fc.contains("name: CLOUD_AI_BASE_URL"), "{fc}");
    assert!(fc.contains("value: \"https://api.hanzo.ai\""), "{fc}");
    assert_ne!(
        deployed("oci.hanzo.ai/hanzoai/cloud-oss@sha256:aa", &plain).register(),
        deployed("oci.hanzo.ai/hanzoai/cloud-oss@sha256:aa", &fc).register()
    );
}

#[test]
fn static_ts_and_json_app_manifest_parse_without_executing_ts() {
    let parsed = parse_static_ts_manifest(
        r#"
        export default {
          cloud: "oci.hanzo.ai/hanzoai/cloud-oss:main",
          staged: "fanclub",
          iamIssuer: "https://hanzo.ai",
          aiBaseUrl: "https://api.hanzo.ai",
          strictRegistry: true,
        };
        "#,
    );
    assert_eq!(parsed.cloud.as_deref(), Some(CLOUD_OCI));
    assert_eq!(parsed.staged.as_deref(), Some("fanclub"));
    assert_eq!(parsed.iam_issuer.as_deref(), Some("https://hanzo.ai"));
    assert_eq!(parsed.ai_base_url.as_deref(), Some("https://api.hanzo.ai"));
    assert!(parsed.strict_registry);
}

#[test]
fn the_vm_is_booted_with_the_stdio_wire_and_the_forwards() {
    let cp = checkpoint();
    assert_eq!(
        run_args(&boot()),
        [
            "run",
            "--stdio",
            "--allow-net",
            "--cpus",
            "4",
            "--memory",
            "4096",
            "--disk-size",
            "16384",
            "-p",
            "6443:6443",
            "-p",
            "36930:30080",
            "--from",
            cp.as_str(),
        ]
    );
}

#[test]
fn the_workload_deploys_the_pinned_image_on_the_forwarded_port() {
    let y = workload("ghcr.io/hanzoai/cloud@sha256:c10d");
    assert!(
        y.contains("image: ghcr.io/hanzoai/cloud@sha256:c10d"),
        "{y}"
    );
    assert!(y.contains("nodePort: 30080"), "{y}");
    assert!(y.contains("containerPort: 8080"), "{y}");
    assert!(y.contains("namespace: hanzo"), "{y}");
    assert!(
        y.contains("path: /readyz\n              port: 8080"),
        "readinessProbe must target API port 8080:\n{y}"
    );
    assert!(
        y.contains("path: /healthz\n              port: 8080"),
        "livenessProbe must target API port 8080:\n{y}"
    );
    assert!(
        !y.contains("port: 9090"),
        "probes must not target unbound legacy port 9090:\n{y}"
    );
    // Half of what the node does not keep for itself, so two fit (budget.rs), and
    // enough for the host to hold the apps it starts with itself.
    assert!(
        y.contains("memory: 1664Mi"),
        "default 4GiB VM must request 1664Mi: two fit the node, and Warm covers the pinned apps:\n{y}"
    );
    assert!(y.contains("serviceAccountName: cloud"), "{y}");
    assert!(y.contains("imagePullPolicy: IfNotPresent"), "{y}");
    assert!(y.contains("claimName: cloud-data"), "{y}");
    assert!(!y.contains("name: data\n          emptyDir"), "{y}");
    assert!(
        y.contains("name: CLOUD_PUBSUB_MAX_STORE\n              value: \"1073741824\""),
        "a 16 GiB disk gives the bus a 1 GiB store:\n{y}"
    );
    assert!(y.contains("fieldPath: spec.nodeName"), "{y}");
    for env in ["ZIP_RUNTIME_DIR", "CLOUD_MEMORY_REQUEST_MIB"] {
        assert!(y.contains(env), "{env} missing from\n{y}");
    }
    assert!(y.contains("secretKeyRef"), "{y}");
    assert!(!y.contains("stringData"), "{y}");
}

#[test]
fn the_key_is_fresh_every_time() {
    let (a, b) = (key(), key());
    assert_ne!(a, b);
    assert_eq!(
        base64::engine::general_purpose::STANDARD
            .decode(&a)
            .unwrap()
            .len(),
        32
    );
}

#[test]
fn the_ground_carries_no_secret() {
    let y = ground(8192);
    assert!(y.contains("kind: Namespace"), "{y}");
    for leak in ["kind: Secret", "stringData", "master:"] {
        assert!(
            !y.contains(leak),
            "the ground must not carry `{leak}`:\n{y}"
        );
    }
    // The workload is not in k3s's manifests directory: the supervisor applies it
    // once the pod's credentials exist.
    assert!(GROUND.contains("/server/manifests/"), "{GROUND}");
    assert!(!WORKLOAD.contains("/server/manifests/"), "{WORKLOAD}");
}

/// Cloud runs as its own account, and what that account may ask is the requests
/// Cloud makes: reads, each in the one place it is made. Never a wildcard, never a
/// verb that writes, never the namespace's default account.
#[test]
fn the_ground_grants_what_cloud_asks_and_no_more() {
    let y = ground(8192);
    assert!(
        y.contains("kind: ServiceAccount\nmetadata:\n  name: cloud\n  namespace: hanzo"),
        "{y}"
    );
    assert!(
        y.contains("kind: PersistentVolumeClaim\nmetadata:\n  name: cloud-data"),
        "{y}"
    );
    assert!(y.contains("storage: 8192Mi"), "{y}");
    for forbidden in [
        "cluster-admin",
        "\"*\"",
        "name: default",
        "create",
        "delete",
        "patch",
        "update",
        "secrets",
    ] {
        assert!(
            !y.contains(forbidden),
            "the ground must not grant `{forbidden}`:\n{y}"
        );
    }
    let rules: Vec<&str> = y
        .lines()
        .filter(|l| l.trim_start().starts_with("verbs:"))
        .collect();
    assert_eq!(rules.len(), 4, "{rules:?}");
    assert_eq!(
        rules
            .iter()
            .filter(|r| r.contains("[\"list\", \"watch\"]"))
            .count(),
        1,
        "{rules:?}"
    );
    assert_eq!(
        rules.iter().filter(|r| r.contains("[\"list\"]")).count(),
        3,
        "{rules:?}"
    );
    // Every binding names the one account, in the one namespace.
    assert_eq!(
        y.matches("kind: ServiceAccount\n    name: cloud\n    namespace: hanzo")
            .count(),
        3,
        "{y}"
    );
}

/// A local build is in the node's store or nowhere: the kubelet is told never to
/// pull it, so its absence is an error and not another image.
#[test]
fn a_local_build_is_never_pulled() {
    let y = workload_for(&Boot {
        cloud: "hanzo.local/hanzo-inc/cloud:0123456789abcdef".into(),
        ..boot()
    })
    .unwrap();
    assert!(
        y.contains("image: hanzo.local/hanzo-inc/cloud:0123456789abcdef"),
        "{y}"
    );
    assert!(y.contains("imagePullPolicy: Never"), "{y}");
    assert!(!y.contains("IfNotPresent"), "{y}");
}

/// A runtime too small for Cloud is refused when the workload is written, with the
/// arithmetic, rather than left to the scheduler to refuse a pod at a time.
#[test]
fn the_workload_is_refused_on_a_runtime_it_does_not_fit() {
    let e = workload_for(&Boot {
        memory_mb: 2048,
        ..boot()
    })
    .unwrap_err()
    .to_string();
    assert!(e.contains("2048 MiB of memory"), "{e}");
    // The ground is still laid: a runtime started for something else has a namespace.
    assert!(ground_for(&Boot {
        memory_mb: 2048,
        ..boot()
    })
    .contains("kind: Namespace"));
}

#[test]
fn the_cluster_keeps_its_state_under_our_own_name() {
    for path in [DATA, KUBECONFIG, GROUND, WORKLOAD] {
        assert!(path.contains("hanzo"), "{path} is not ours");
    }
    assert!(GROUND.starts_with(DATA) && WORKLOAD.starts_with(DATA));
    let argv = server_argv();
    for flag in ["--data-dir", "--write-kubeconfig"] {
        assert!(argv.contains(&flag), "{flag} is not passed: {argv:?}");
    }
    assert!(
        argv.contains(&DATA) && argv.contains(&KUBECONFIG),
        "{argv:?}"
    );
}

#[test]
fn no_manifest_ever_contains_the_key() {
    let k = key();
    let g = ground(8192);
    let w = workload("ghcr.io/hanzoai/cloud@sha256:aa");
    assert!(!g.contains(&k), "the ground leaked the key");
    assert!(!w.contains(&k), "the workload leaked the key");
    assert!(w.contains("secretKeyRef"), "{w}");
    assert!(KEYFILE.starts_with("/dev/shm/"), "{KEYFILE}");
    assert!(KEYFILE_FALLBACK.starts_with("/run/"), "{KEYFILE_FALLBACK}");
    assert!(PULLFILE.starts_with("/dev/shm/"), "{PULLFILE}");
    assert!(
        PULLFILE_FALLBACK.starts_with("/run/"),
        "{PULLFILE_FALLBACK}"
    );
    assert_eq!(CLOUD, "ghcr.io/hanzoai/cloud-oss:main");
}

#[test]
fn the_workload_register_covers_the_image_and_the_manifest() {
    let one = deployed("ghcr.io/hanzoai/cloud@sha256:aa", "kind: Deployment");
    let same = deployed("ghcr.io/hanzoai/cloud@sha256:aa", "kind: Deployment");
    assert_eq!(one.register(), same.register());

    let other_image = deployed("ghcr.io/hanzoai/cloud@sha256:bb", "kind: Deployment");
    assert_ne!(one.register(), other_image.register());

    let other_manifest = deployed("ghcr.io/hanzoai/cloud@sha256:aa", "kind: DaemonSet");
    assert_ne!(one.register(), other_manifest.register());

    assert_eq!(one.events()[1].source, WORKLOAD);
}

#[test]
fn the_at_rest_key_is_outside_the_measurement() {
    let manifest = workload("ghcr.io/hanzoai/cloud@sha256:aa");
    let register = deployed("ghcr.io/hanzoai/cloud@sha256:aa", &manifest).register();
    for _ in 0..2 {
        let _ = (ground(8192), key());
        assert_eq!(
            deployed("ghcr.io/hanzoai/cloud@sha256:aa", &manifest).register(),
            register
        );
    }
}

#[test]
fn the_checkpoint_installs_k3s_for_this_architecture() {
    let cp = checkpoint();
    let args = vm::checkpoint_args(&cp, &install_cmd_for(false, None).unwrap());
    assert_eq!(
        &args[..5],
        ["checkpoint", "create", cp.as_str(), "--allow-net", "--"]
    );
    let cmd = args.last().unwrap();
    assert!(cmd.contains("k3s-io/k3s/releases/latest/download"), "{cmd}");
    assert!(cmd.contains(k3s_asset()), "{cmd}");
    assert!(cmd.contains("chmod +x /usr/local/bin/k3s"), "{cmd}");
    assert!(cmd.contains(k3s_airgap_asset()), "{cmd}");
    assert!(cmd.contains("/var/lib/hanzo/agent/images"), "{cmd}");
    if cfg!(target_arch = "aarch64") {
        assert_eq!(k3s_asset(), "k3s-arm64");
        assert_eq!(k3s_airgap_asset(), "k3s-airgap-images-arm64.tar.zst");
    } else {
        assert_eq!(k3s_asset(), "k3s");
        assert_eq!(k3s_airgap_asset(), "k3s-airgap-images-amd64.tar.zst");
    }
}

#[test]
fn install_cmd_for_enforces_strict_registry() {
    let err = install_cmd_for(true, None).unwrap_err().to_string();
    assert!(err.contains("refusing GitHub k3s download"), "{err}");

    let err = install_cmd_for(
        true,
        Some("https://github.com/k3s-io/k3s/releases/download/v1/k3s"),
    )
    .unwrap_err()
    .to_string();
    assert!(err.contains("refusing non-Hanzo k3s artifact URL"), "{err}");

    let ok = install_cmd_for(true, Some("https://pkg.hanzo.ai/k3s/v1.31/k3s-arm64")).unwrap();
    assert!(
        ok.contains("https://pkg.hanzo.ai/k3s/v1.31/k3s-arm64"),
        "{ok}"
    );
    assert!(ok.contains("chmod +x /usr/local/bin/k3s"), "{ok}");
}

#[test]
fn a_node_is_ready_when_its_status_says_so() {
    assert!(node_ready(
        "k3s-node   Ready    control-plane   30s   v1.30.0\n"
    ));
    assert!(node_ready(
        "n1   Ready,SchedulingDisabled   worker   1m   v1.30.0\n"
    ));
    assert!(!node_ready(
        "k3s-node   NotReady   control-plane   5s   v1.30.0\n"
    ));
    assert!(!node_ready(""));
}

#[test]
fn the_kubeconfig_is_pointed_at_the_forwarded_port() {
    let yaml = "apiVersion: v1\nclusters:\n- cluster:\n    server: https://10.0.2.15:6443\n    certificate-authority-data: AAA\n";
    let out = rewrite_server(yaml);
    assert!(
        out.contains("    server: https://127.0.0.1:6443\n"),
        "{out}"
    );
    assert!(!out.contains("10.0.2.15"), "{out}");
    assert!(out.contains("certificate-authority-data: AAA"), "{out}");
    assert_eq!(
        rewrite_server("# the server: line\n"),
        "# the server: line\n"
    );
}

#[test]
fn the_kubeconfig_is_filed_owner_only() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(".kube").join("hanzo.yaml");
    write_kubeconfig(&path, "apiVersion: v1\n").unwrap();
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "apiVersion: v1\n");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600, "mode {mode:o}");
    }
}

#[cfg(unix)]
fn group(dir: &Path) -> (i32, i32) {
    use std::os::unix::process::CommandExt;
    let child = dir.join("child");
    let mut leader = Command::new("sh")
        .args([
            "-c",
            &format!("sleep 60 & echo $! > {}; wait", child.display()),
        ])
        .process_group(0)
        .spawn()
        .expect("sh runs");
    let pid = leader.id() as i32;
    std::thread::spawn(move || leader.wait());

    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let read = std::fs::read_to_string(&child).ok();
        if let Some(c) = read
            .and_then(|s| s.trim().parse().ok())
            .filter(|c| alive(*c))
        {
            return (pid, c);
        }
        assert!(Instant::now() < deadline, "the group never started");
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[cfg(unix)]
#[test]
fn the_pidfile_follows_a_real_process() {
    let dir = tempfile::tempdir().unwrap();
    let (pid, _) = group(dir.path());
    write_pid(dir.path(), SUPERVISOR, pid as u32).unwrap();

    assert_eq!(read_pid(dir.path(), SUPERVISOR), Some(pid));
    assert_eq!(running(dir.path(), SUPERVISOR), Some(pid));
    assert_eq!(read_pid(dir.path(), VM), None);

    assert!(stop(pid, Duration::from_secs(5)));
    assert_eq!(
        running(dir.path(), SUPERVISOR),
        None,
        "stopped is not running"
    );

    clear_pid(dir.path(), SUPERVISOR);
    assert_eq!(read_pid(dir.path(), SUPERVISOR), None);
}

#[cfg(unix)]
#[test]
fn stopping_a_group_leader_takes_its_children() {
    let dir = tempfile::tempdir().unwrap();
    let (leader, child) = group(dir.path());
    assert!(alive(child));

    assert!(stop(leader, Duration::from_secs(5)));

    let deadline = Instant::now() + Duration::from_secs(5);
    while alive(child) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(!alive(child), "the hypervisor outlived the vm's group");
}

#[cfg(unix)]
#[test]
fn stopping_what_is_already_gone_succeeds() {
    let mut child = Command::new("true").spawn().unwrap();
    let pid = child.id() as i32;
    child.wait().unwrap();
    assert!(stop(pid, Duration::from_millis(200)));
}

#[test]
fn the_state_file_carries_the_phase() {
    let dir = tempfile::tempdir().unwrap();
    assert_eq!(read_state(dir.path()), None);
    write_state(dir.path(), "boot");
    assert_eq!(read_state(dir.path()).as_deref(), Some("boot"));

    write_state(dir.path(), "ready");
    wait_ready_state(dir.path()).unwrap();

    write_state(dir.path(), "error: no assets");
    let err = wait_ready_state(dir.path()).unwrap_err();
    assert!(err.to_string().contains("no assets"), "{err}");

    write_state(dir.path(), "down (exit status: 0)");
    assert!(wait_ready_state(dir.path()).is_err());
}

#[test]
fn the_measurement_is_filed_and_reads_back() {
    let dir = tempfile::tempdir().unwrap();
    let measurement = json!({"jsonrpc": "2.0", "method": "measurement", "params": launched()});
    let script = format!(
        "{}{}{}{}",
        line(&measurement.to_string()),
        line(r#"{"jsonrpc":"2.0","method":"ready"}"#),
        "read line; ",
        line(r#"{"jsonrpc":"2.0","id":1,"result":{"platform":"none"}}"#),
    );
    let mut rpc = peer(dir.path(), &script);
    let launch = rpc.wait_ready().unwrap();

    let manifest = workload(&boot().cloud);
    record(dir.path(), &mut rpc, launch, &boot().cloud, &manifest).unwrap();
    let _ = rpc.child.wait();

    let m = measured(dir.path()).unwrap();
    assert_eq!(m.launch, launched());
    assert_eq!(
        m.workload.register(),
        deployed(&boot().cloud, &manifest).register()
    );
    assert_eq!(m.hardware.platform, "none");
    assert!(m.hardware.report.is_none());

    let path = measure_path(dir.path());
    let edited = std::fs::read_to_string(&path)
        .unwrap()
        .replace(&m.launch.register().hex(), &"ab".repeat(48));
    std::fs::write(&path, edited).unwrap();
    assert!(measured(dir.path()).is_err());

    std::fs::remove_file(&path).unwrap();
    let e = measured(dir.path()).unwrap_err();
    assert!(e.to_string().contains("is a cluster up?"), "{e}");
}

#[test]
fn the_old_up_spelling_splits_into_service_and_tail() {
    let v = |s: &[&str]| s.iter().map(|x| x.to_string()).collect::<Vec<_>>();
    assert_eq!(service_argv(v(&["iam"])), ("iam".into(), vec![]));
    assert_eq!(
        service_argv(v(&["cloud", "--", "--port", "1"])),
        ("cloud".into(), v(&["--port", "1"]))
    );
    assert_eq!(service_argv(vec![]), ("cloud".into(), vec![]));
}

#[test]
fn endpoints_surface_direct_cloud_url_when_hanzod_is_unavailable() {
    let kc = Path::new("/home/dev/.kube/hanzo.yaml");
    let with_hanzod = format_endpoints(kc, true);
    assert!(with_hanzod.contains("export KUBECONFIG=/home/dev/.kube/hanzo.yaml"));
    assert!(with_hanzod.contains("http://127.0.0.1:3690  via hanzod → 127.0.0.1:36930"));

    let without_hanzod = format_endpoints(kc, false);
    assert!(without_hanzod.contains("export KUBECONFIG=/home/dev/.kube/hanzo.yaml"));
    assert!(without_hanzod.contains("http://127.0.0.1:36930"));
    assert!(without_hanzod.contains("http://127.0.0.1:3690 requires hanzod"));
    assert!(without_hanzod.contains("hanzo upgrade hanzod"));
}

#[test]
fn kubeconfig_parses_mtls_credentials_and_server() {
    use base64::Engine as _;
    let b64 = base64::engine::general_purpose::STANDARD;
    let ca = b64.encode(b"CA-PEM");
    let cert = b64.encode(b"CERT-PEM");
    let key = b64.encode(b"KEY-PEM");
    let yaml = format!(
        "apiVersion: v1\nclusters:\n- cluster:\n    server: https://127.0.0.1:6443/\n    certificate-authority-data: {ca}\n  name: default\nusers:\n- name: default\n  user:\n    client-certificate-data: {cert}\n    client-key-data: {key}\n"
    );
    let auth = KubeClient::parse_kubeconfig(&yaml).unwrap();
    assert_eq!(auth.server, "https://127.0.0.1:6443");
    assert_eq!(auth.ca_pem.as_deref(), Some(b"CA-PEM".as_slice()));
    assert_eq!(
        auth.client_pem.as_deref(),
        Some(b"CERT-PEM\nKEY-PEM".as_slice())
    );
}

#[test]
fn extract_deployment_yaml_isolates_cloud_deployment() {
    // OSS-pure image ref in unit test
    let manifest = workload("ghcr.io/hanzoai/cloud-oss@sha256:abc");
    let deploy = extract_deployment_yaml(&manifest).unwrap();
    assert!(deploy.contains("kind: Deployment"), "{deploy}");
    assert!(!deploy.contains("kind: Service"), "{deploy}");
    let svc = extract_service_yaml(&manifest).unwrap();
    assert!(svc.contains("kind: Service"), "{svc}");
    assert!(svc.contains("nodePort: 30080"), "{svc}");
    assert!(!svc.contains("kind: Deployment"), "{svc}");
}

#[test]
fn has_workload_overrides_detects_changed_flags() {
    let base = Boot {
        cpus: 4,
        memory_mb: 4096,
        disk_mb: 32768,
        cloud: CLOUD.to_string(),
        staged: None,
        iam_issuer: None,
        ai_base_url: None,
        strict_registry: false,
        local: None,
    };
    assert!(!has_workload_overrides(&base));
    assert!(has_workload_overrides(&Boot {
        local: Some("/x/image".into()),
        ..base.clone()
    }));
    assert!(has_workload_overrides(&Boot {
        cloud: "ghcr.io/hanzoai/cloud-oss:v8.5.1453".into(),
        ..base.clone()
    }));
    assert!(has_workload_overrides(&Boot {
        staged: Some("fanclub".into()),
        ..base
    }));
}

/// `hanzo down` scales the Deployment to zero, and Kubernetes then leaves
/// `spec.replicas` out of the Scale document. That is a stopped Deployment, which
/// `hanzo up` scales back to one on the image it names. Read as "no Deployment" it
/// was redeployed with the default image, over a local build.
#[test]
fn a_deployment_scaled_to_zero_is_still_a_deployment() {
    assert_eq!(scale_replicas(&json!({"kind": "Scale", "spec": {}})), 0);
    assert_eq!(
        scale_replicas(&json!({"kind": "Scale", "spec": {"replicas": 0}})),
        0
    );
    assert_eq!(
        scale_replicas(&json!({"kind": "Scale", "spec": {"replicas": 1}})),
        1
    );
}

#[test]
fn every_kind_the_cli_writes_has_its_api_path() {
    let at = |api: &str, kind: &str, ns: &str, name: &str| {
        resource_url("https://127.0.0.1:6443", api, kind, ns, name).unwrap()
    };
    assert_eq!(
        at("v1", "ServiceAccount", "hanzo", "cloud"),
        "https://127.0.0.1:6443/api/v1/namespaces/hanzo/serviceaccounts/cloud"
    );
    assert_eq!(
        at(
            "rbac.authorization.k8s.io/v1",
            "ClusterRole",
            "",
            "hanzo-cloud"
        ),
        "https://127.0.0.1:6443/apis/rbac.authorization.k8s.io/v1/clusterroles/hanzo-cloud"
    );
    assert_eq!(
        at("rbac.authorization.k8s.io/v1", "RoleBinding", "hanzo-build", "cloud"),
        "https://127.0.0.1:6443/apis/rbac.authorization.k8s.io/v1/namespaces/hanzo-build/rolebindings/cloud"
    );
    // Every document of the ground is one of them.
    for doc in ground(8192).split("\n---\n") {
        let v: serde_json::Value = serde_norway::from_str(doc).unwrap();
        let s = |p: &str| v.pointer(p).and_then(|x| x.as_str()).unwrap_or("");
        if s("/kind") != "Namespace" {
            at(
                s("/apiVersion"),
                s("/kind"),
                s("/metadata/namespace"),
                s("/metadata/name"),
            );
        }
    }
    assert!(resource_url("https://x", "v1", "Secret", "hanzo", "cloud").is_err());
    assert!(resource_url("https://x", "v1", "ServiceAccount", "", "cloud").is_err());
}

#[test]
fn cloud_readiness_requires_current_rollout_and_nonzero_replicas() {
    let mut v = json!({
        "metadata": {"generation": 2},
        "spec": {"replicas": 1},
        "status": {
            "observedGeneration": 1,
            "replicas": 1,
            "updatedReplicas": 1,
            "readyReplicas": 1,
            "availableReplicas": 1
        }
    });
    assert!(!cloud_deployment_ready(&v));
    v["status"]["observedGeneration"] = json!(2);
    assert!(cloud_deployment_ready(&v));
    v["status"]["updatedReplicas"] = json!(0);
    assert!(!cloud_deployment_ready(&v));
    v["status"]["updatedReplicas"] = json!(1);
    v["status"]["readyReplicas"] = json!(0);
    assert!(!cloud_deployment_ready(&v));
    v["status"]["readyReplicas"] = json!(1);
    v["status"]["availableReplicas"] = json!(0);
    assert!(!cloud_deployment_ready(&v));
    v["status"]["availableReplicas"] = json!(1);
    v["status"]["replicas"] = json!(2);
    assert!(
        !cloud_deployment_ready(&v),
        "stale terminating replica must wait"
    );
    v["status"]["replicas"] = json!(1);
    v["spec"]["replicas"] = json!(0);
    assert!(!cloud_deployment_ready(&v));
}

#[test]
fn strict_registry_is_a_workload_override_on_warm_runtime() {
    let mut b = boot();
    b.cloud = CLOUD.into();
    assert!(!has_workload_overrides(&b));
    b.strict_registry = true;
    assert!(has_workload_overrides(&b));
}

/// Fresh boots start from the immutable `k3s-<ver>-airgap` checkpoint; once
/// `hanzo stop` has saved `k3s-<ver>-state.ext4`, `hanzo start` and `hanzo up`
/// boot from that state checkpoint and clamp `--disk-size` up if the saved
/// ext4 image was previously grown beyond the CLI flag's default.
#[test]
fn run_args_selects_saved_state_checkpoint_when_present_and_clamps_disk() {
    let vm_home = tempfile::tempdir().unwrap();
    let fresh = run_args_in(&boot(), vm_home.path());
    assert_eq!(
        fresh.last().map(String::as_str),
        Some(checkpoint().as_str())
    );
    assert!(fresh.windows(2).any(|w| w == ["--disk-size", "16384"]));

    let state_path = persist::checkpoint_file(vm_home.path(), &persist::state_checkpoint());
    std::fs::create_dir_all(state_path.parent().unwrap()).unwrap();
    let f = std::fs::File::create(&state_path).unwrap();
    // Simulate a 48 GiB saved runtime state image while `boot().disk_mb` is 16384.
    f.set_len(48 * 1024 * 1024 * 1024).unwrap();
    drop(f);

    assert!(persist::has_state_checkpoint_in(vm_home.path()));
    let resumed = run_args_in(&boot(), vm_home.path());
    assert_eq!(
        resumed.last().map(String::as_str),
        Some(persist::state_checkpoint().as_str())
    );
    assert!(
        resumed.windows(2).any(|w| w == ["--disk-size", "49152"]),
        "disk-size must never shrink below saved checkpoint image: {resumed:?}"
    );

    // `hanzo reset` deletes the saved runtime state checkpoint while leaving
    // the base airgap checkpoint intact.
    let airgap_path = persist::checkpoint_file(vm_home.path(), &checkpoint());
    std::fs::write(&airgap_path, b"base-airgap").unwrap();
    assert!(persist::delete_state_checkpoint_in(vm_home.path()));
    assert!(!persist::has_state_checkpoint_in(vm_home.path()));
    assert!(airgap_path.is_file(), "base airgap checkpoint stays cached");
    assert_eq!(
        run_args_in(&boot(), vm_home.path())
            .last()
            .map(String::as_str),
        Some(checkpoint().as_str())
    );
}

/// Resuming after `hanzo stop` or `hanzo down` preserves a `hanzo.local/*`
/// build (even when flags like `--staged` change) and never falls back to a
/// published image unless the caller explicitly names `--cloud` or `--strict-registry`.
#[test]
fn resuming_preserves_local_image_and_bound_pvc_size() {
    let mut b = boot();
    b.cloud = CLOUD.into();
    let local_ref = "hanzo.local/hanzo-inc/cloud:0123456789abcdef";

    // Default resume (`hanzo start` / `hanzo up`) keeps the local image.
    assert_eq!(
        persist::preserved_cluster_image(&b, Some(local_ref)),
        Some(local_ref)
    );
    // Changing non-image workload env (`--staged`) still keeps the local image!
    let with_staged = Boot {
        staged: Some("fanclub".into()),
        ..b.clone()
    };
    assert_eq!(
        persist::preserved_cluster_image(&with_staged, Some(local_ref)),
        Some(local_ref)
    );
    // Explicit `--cloud` replaces the image.
    let explicit_cloud = Boot {
        cloud: "ghcr.io/hanzoai/cloud-oss:v8.5.1473".into(),
        ..b.clone()
    };
    assert_eq!(
        persist::preserved_cluster_image(&explicit_cloud, Some(local_ref)),
        None
    );
    // `--strict-registry` refuses non-Hanzo refs while keeping `oci.hanzo.ai`.
    let strict = Boot {
        strict_registry: true,
        ..b
    };
    assert_eq!(
        persist::preserved_cluster_image(&strict, Some(local_ref)),
        None
    );
    assert_eq!(
        persist::preserved_cluster_image(&strict, Some("oci.hanzo.ai/hanzoai/cloud-oss@sha256:aa")),
        Some("oci.hanzo.ai/hanzoai/cloud-oss@sha256:aa")
    );

    // Resumed `GROUND` preserves the bound PVC's immutable storage request.
    let saved_ground = ground(8192);
    assert_eq!(persist::existing_claim_mb(&saved_ground), Some(8192));
}
