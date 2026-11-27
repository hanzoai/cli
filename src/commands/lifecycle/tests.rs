//! Unit & conformance tests for `src/commands/lifecycle/*`:
//! - Human-readable size parsing (`parse_size_mb`)
//! - Multi-project detection, isolated K8s manifest generation, prefix matching
//! - Desired-state persistence, `down` isolation, and `restart` restoration
//! - `hanzod` `/v1/lifecycle/*` JSON wire deserialization & 404 fallback

use hanzo_client::Reply;
use serde_json::json;

use super::client::{classify_reply, DoctorResponse, HanzodOutcome, PsResponse};
use super::project::{
    allocate_node_port, detect_project, humanize_slug, load_state_from, render_app_k8s_manifest,
    resolve_registered_app, save_state_to, slugify, upsert_detected_app, DesiredState,
};
use super::resources::{is_vm_lifecycle_verb, parse_vm_lifecycle, VmLifecycleCommand};
use super::units::{format_size_mb, parse_size_mb};

#[test]
fn parse_size_mb_conformance() {
    assert_eq!(parse_size_mb("32GiB").unwrap(), 32768);
    assert_eq!(parse_size_mb("32GB").unwrap(), 32768);
    assert_eq!(parse_size_mb("32G").unwrap(), 32768);
    assert_eq!(parse_size_mb("8GiB").unwrap(), 8192);
    assert_eq!(parse_size_mb("8192MiB").unwrap(), 8192);
    assert_eq!(parse_size_mb("8192MB").unwrap(), 8192);
    assert_eq!(parse_size_mb("8192M").unwrap(), 8192);
    assert_eq!(parse_size_mb("4096").unwrap(), 4096);
    assert_eq!(parse_size_mb("512MiB").unwrap(), 512);
    assert_eq!(parse_size_mb("1TiB").unwrap(), 1_048_576);
    assert_eq!(parse_size_mb("1.5GiB").unwrap(), 1536);

    assert!(parse_size_mb("").is_err());
    assert!(parse_size_mb("0").is_err());
    assert!(parse_size_mb("0GiB").is_err());
    assert!(parse_size_mb("64KiB").is_err());

    assert_eq!(format_size_mb(32768), "32 GiB");
    assert_eq!(format_size_mb(8192), "8 GiB");
    assert_eq!(format_size_mb(512), "512 MiB");
}

#[test]
fn vm_lifecycle_subcommands_parse_and_preserve_passthrough() {
    let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
    assert!(is_vm_lifecycle_verb(&s(&["start", "--cpus", "8"])));
    assert!(is_vm_lifecycle_verb(&s(&["stop"])));
    assert!(is_vm_lifecycle_verb(&s(&["resize", "--disk", "64GiB"])));
    assert!(is_vm_lifecycle_verb(&s(&["status"])));

    // Verbatim passthrough subcommands stay untouched
    assert!(!is_vm_lifecycle_verb(&s(&["run", "--cpus", "2", "echo", "hi"])));
    assert!(!is_vm_lifecycle_verb(&s(&["checkpoint", "list"])));
    assert!(!is_vm_lifecycle_verb(&s(&["--help"])));
    assert!(!is_vm_lifecycle_verb(&s(&["--version"])));

    let start = parse_vm_lifecycle(&s(&[
        "start",
        "--cpus",
        "8",
        "--memory",
        "8GiB",
        "--disk",
        "32GiB",
        "--native",
    ]))
    .expect("vm start parses");
    match start.command {
        VmLifecycleCommand::Start {
            cpus,
            memory,
            disk_mb,
            native,
            ..
        } => {
            assert_eq!(cpus, Some(8));
            assert_eq!(memory, Some(8192));
            assert_eq!(disk_mb, Some(32768));
            assert!(native);
        }
        other => panic!("expected Start, got {other:?}"),
    }

    let resize = parse_vm_lifecycle(&s(&[
        "resize",
        "--cpus",
        "12",
        "--memory",
        "16GiB",
        "--disk-size",
        "64GiB",
    ]))
    .expect("vm resize parses");
    match resize.command {
        VmLifecycleCommand::Resize {
            cpus,
            memory,
            disk_mb,
        } => {
            assert_eq!(cpus, Some(12));
            assert_eq!(memory, Some(16384));
            assert_eq!(disk_mb, Some(65536));
        }
        other => panic!("expected Resize, got {other:?}"),
    }
}

#[test]
fn multi_project_detection_and_desired_state_isolation() {
    let tmp = tempfile::tempdir().unwrap();
    let state_file = tmp.path().join("state.json");

    // Project 1: Solar CRM (`hanzo.toml` with container image)
    let solar_dir = tmp.path().join("solar-crm");
    std::fs::create_dir_all(&solar_dir).unwrap();
    std::fs::write(
        solar_dir.join("hanzo.toml"),
        r#"
[app]
name = "Solar CRM"
image = "ghcr.io/acme/solar-crm:1.0.0"
port = 3000
"#,
    )
    .unwrap();

    // Project 2: Billing Portal (`compose.yaml`)
    let billing_dir = tmp.path().join("billing-portal");
    std::fs::create_dir_all(&billing_dir).unwrap();
    std::fs::write(
        billing_dir.join("compose.yaml"),
        r#"
services:
  web:
    image: ghcr.io/acme/billing-portal:latest
    ports:
      - "4100:4100"
"#,
    )
    .unwrap();

    // System checkout (`hanzoai/cloud`) is NEVER treated as a user app workload
    let cloud_checkout = tmp.path().join("cloud");
    std::fs::create_dir_all(&cloud_checkout).unwrap();
    std::fs::write(
        cloud_checkout.join("go.mod"),
        "module github.com/hanzoai/cloud\n\ngo 1.24\n",
    )
    .unwrap();
    assert_eq!(detect_project(&cloud_checkout).unwrap(), None);

    let solar = detect_project(&solar_dir)
        .unwrap()
        .expect("solar-crm detected");
    assert_eq!(solar.slug, "solar-crm");
    assert_eq!(solar.name, "Solar CRM");
    assert_eq!(solar.image.as_deref(), Some("ghcr.io/acme/solar-crm:1.0.0"));
    assert_eq!(solar.container_port, 3000);

    let billing = detect_project(&billing_dir)
        .unwrap()
        .expect("billing-portal detected");
    assert_eq!(billing.slug, "billing-portal");
    assert_eq!(billing.name, "Billing Portal");
    assert_eq!(billing.container_port, 4100);

    // Register both apps + Cloud in shared desired state
    let mut state = DesiredState::default();
    state.runtime.desired_running = true;
    state.platform.cloud_enabled = true;
    let solar_reg = upsert_detected_app(&mut state, &solar);
    let billing_reg = upsert_detected_app(&mut state, &billing);

    assert_eq!(solar_reg.namespace, "hanzo-app-solar-crm");
    assert_eq!(billing_reg.namespace, "hanzo-app-billing-portal");
    assert_ne!(solar_reg.node_port, billing_reg.node_port);
    assert_eq!(allocate_node_port(&state, "solar-crm"), solar_reg.node_port);

    // Prefix matching: `hanzo app start/stop solar` resolves `solar-crm`
    let matched = resolve_registered_app(&state, Some("solar"), None).unwrap();
    assert_eq!(matched.slug, "solar-crm");
    // CWD matching when name omitted
    let by_cwd = resolve_registered_app(&state, None, Some(&billing_dir)).unwrap();
    assert_eq!(by_cwd.slug, "billing-portal");

    // Isolated K8s manifest uses dedicated namespace `hanzo-app-solar-crm`
    let k8s_yaml = render_app_k8s_manifest(&solar_reg, "ghcr.io/acme/solar-crm:1.0.0");
    assert!(k8s_yaml.contains("name: hanzo-app-solar-crm"), "{k8s_yaml}");
    assert!(k8s_yaml.contains("namespace: hanzo-app-solar-crm"), "{k8s_yaml}");
    assert!(k8s_yaml.contains("containerPort: 3000"), "{k8s_yaml}");

    // Simulate `hanzo down` inside `solar-crm`: stops ONLY `solar-crm`, preserving Cloud & `billing-portal`!
    state.apps.get_mut("solar-crm").unwrap().desired_running = false;
    save_state_to(&state_file, &state).unwrap();

    let reloaded = load_state_from(&state_file).unwrap();
    assert!(reloaded.platform.cloud_enabled, "Cloud remains enabled");
    assert!(
        !reloaded.apps["solar-crm"].desired_running,
        "solar-crm stopped"
    );
    assert!(
        reloaded.apps["billing-portal"].desired_running,
        "unrelated billing-portal preserved for restart restoration"
    );

    assert_eq!(slugify("My Cool App!"), "my-cool-app");
    assert_eq!(humanize_slug("solar-crm"), "Solar CRM");
}

#[test]
fn hanzod_wire_contract_deserializes_live_ps_and_doctor_and_404_falls_back() {
    // Live hanzod `/v1/lifecycle/ps` payload (`component` + `status` + optional `detail` + `endpoint`)
    let live_ps = json!({
        "ok": true,
        "rows": [
            {
                "component": "Hanzo Runtime",
                "layer": "infrastructure",
                "status": "Stopped",
                "desired": "stopped",
                "detail": "native-linux (4 vCPU, 4096 MiB)",
                "namespace": "kube-system"
            },
            {
                "component": "Hanzo Engine",
                "layer": "platform",
                "status": "Stopped",
                "desired": "stopped",
                "namespace": "hanzo"
            },
            {
                "component": "Hanzo Dev",
                "layer": "workload",
                "status": "Ready",
                "detail": "native harness (hanzo-gen)",
                "endpoint": "http://127.0.0.1:3690/v1/dev"
            }
        ]
    });
    let parsed_ps: PsResponse = serde_json::from_value(live_ps).expect("live ps deserializes");
    assert_eq!(parsed_ps.rows.len(), 3);
    assert_eq!(parsed_ps.rows[0].component, "Hanzo Runtime");
    assert_eq!(parsed_ps.rows[1].display_detail(), "ns/hanzo");
    assert!(parsed_ps.rows[2]
        .display_detail()
        .contains("http://127.0.0.1:3690/v1/dev"));

    // Also supports `name` + `state` aliases
    let alias_ps = json!({
        "rows": [{ "name": "k3s", "layer": "infrastructure", "state": "Running", "detail": ":6443" }]
    });
    let parsed_alias: PsResponse = serde_json::from_value(alias_ps).unwrap();
    assert_eq!(parsed_alias.rows[0].component, "k3s");
    assert_eq!(parsed_alias.rows[0].status, "Running");

    // Live hanzod `/v1/lifecycle/doctor` payload (`message` alias for `detail`)
    let live_doc = json!({
        "ok": true,
        "healthy": true,
        "summary": "all lifecycle layers synchronized and healthy",
        "checks": [
            { "layer": "daemon", "name": "state_store", "status": "ok", "message": "mirror ok" }
        ]
    });
    let parsed_doc: DoctorResponse = serde_json::from_value(live_doc).expect("doctor deserializes");
    assert!(parsed_doc.ok);
    assert_eq!(parsed_doc.checks[0].detail, "mirror ok");

    // 404 / 401 from older or unsigned hanzod cleanly triggers local fallback (`Unsupported`)
    assert!(matches!(
        classify_reply(
            Reply {
                status: 404,
                body: json!({"error": "not found"})
            },
            "/v1/lifecycle/ps"
        )
        .unwrap(),
        HanzodOutcome::Unsupported
    ));
    assert!(matches!(
        classify_reply(
            Reply {
                status: 401,
                body: json!({"error": "a bearer is required"})
            },
            "/v1/lifecycle/service"
        )
        .unwrap(),
        HanzodOutcome::Unsupported
    ));
    // Genuine 400 validation failure surfaces as Err
    assert!(classify_reply(
        Reply {
            status: 400,
            body: json!({"error": "invalid backend"})
        },
        "/v1/lifecycle/runtime"
    )
    .is_err());
}
