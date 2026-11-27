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
    assert!(
        y.contains("memory: 2048Mi"),
        "default 4GiB VM must request 2048Mi so Warm covers pinned subsystems:\n{y}"
    );
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
    let y = ground();
    assert!(y.contains("kind: Namespace"), "{y}");
    for leak in ["Secret", "stringData", "master:"] {
        assert!(
            !y.contains(leak),
            "the ground must not carry `{leak}`:\n{y}"
        );
    }
    assert!(GROUND < WORKLOAD, "{GROUND} must sort before {WORKLOAD}");
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
    let g = ground();
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
        let _ = (ground(), key());
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
    };
    assert!(!has_workload_overrides(&base));
    assert!(has_workload_overrides(&Boot {
        cloud: "ghcr.io/hanzoai/cloud-oss:v8.5.1453".into(),
        ..base.clone()
    }));
    assert!(has_workload_overrides(&Boot {
        staged: Some("fanclub".into()),
        ..base
    }));
}
