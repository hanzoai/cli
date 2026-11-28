use super::wire::*;
use super::*;

#[test]
fn platform_strings_match_the_release_assets() {
    assert_eq!(platform("macos", "aarch64"), Some("darwin-aarch64"));
    assert_eq!(platform("linux", "x86_64"), Some("linux-x86_64"));
    assert_eq!(platform("linux", "aarch64"), Some("linux-aarch64"));
    assert_eq!(platform("macos", "x86_64"), None);
    assert_eq!(platform("windows", "x86_64"), None);
}

#[test]
fn version_ordering_drives_the_update() {
    // A vm older than the pin is replaced — 2.0.0 has no measurement to
    // report, and `hanzo up` refuses a vm that reports none.
    assert!(older("2.0.0", VM_VERSION));
    // 2.0.1's `run --stdio` could outlive its closed stdin, holding the
    // forwarded ports; closing stdin is how every driver here stops a vm.
    assert!(older("2.0.1", VM_VERSION));
    assert!(older("0.1.3", "2.0.0"));
    assert!(older("1.9.9", "2.0.0"));
    assert!(!older("2.0.0", "2.0.0"));
    assert!(!older("2.0.1", "2.0.0"));
    assert!(!older("10.0.0", "2.0.0"));
    // Unreadable is older: reinstall rather than trust it.
    assert!(older("garbage", "2.0.0"));
    assert!(older("", "2.0.0"));
    // A suffixed patch still orders by its numeric prefix.
    assert!(older("2.0.0-rc1", "2.0.1"));
}

#[test]
fn sha256_sidecar_accepts_the_true_digest() {
    let body = b"the vm tarball";
    let sidecar = format!("{}  asset.tar.gz\n", hex(&Sha256::digest(body)));
    verify_sha256(body, &sidecar, "asset.tar.gz").unwrap();
}

#[test]
fn sha256_sidecar_refuses_a_tampered_download() {
    let sidecar = format!(
        "{}  asset.tar.gz\n",
        hex(&Sha256::digest(b"the vm tarball"))
    );
    let err = verify_sha256(b"tampered bytes", &sidecar, "asset.tar.gz").unwrap_err();
    assert!(err.to_string().contains("sha256 mismatch"), "{err}");
}

/// A crafted tar.gz (no network): verify → extract → executable file lands.
#[test]
fn extract_installs_the_hanzo_vm_entry() {
    let mut tar = tar::Builder::new(Vec::new());
    let body = b"#!/bin/sh\necho hanzo-vm test\n";
    let mut hdr = tar::Header::new_gnu();
    hdr.set_size(body.len() as u64);
    hdr.set_mode(0o755);
    hdr.set_cksum();
    tar.append_data(&mut hdr, "hanzo-vm", &body[..]).unwrap();
    let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    std::io::Write::write_all(&mut gz, &tar.into_inner().unwrap()).unwrap();
    let tarball = gz.finish().unwrap();

    let sidecar = format!("{}  x.tar.gz", hex(&Sha256::digest(&tarball)));
    verify_sha256(&tarball, &sidecar, "x.tar.gz").unwrap();

    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join("hanzo-vm");
    extract(&tarball, "hanzo-vm", &dest).unwrap();
    assert_eq!(std::fs::read(&dest).unwrap(), body);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&dest).unwrap().permissions().mode() & 0o777,
            0o755
        );
    }
}

#[test]
fn extract_refuses_a_tarball_without_the_binary() {
    let mut tar = tar::Builder::new(Vec::new());
    let mut hdr = tar::Header::new_gnu();
    hdr.set_size(2);
    hdr.set_mode(0o644);
    hdr.set_cksum();
    tar.append_data(&mut hdr, "README", &b"hi"[..]).unwrap();
    let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    std::io::Write::write_all(&mut gz, &tar.into_inner().unwrap()).unwrap();
    let tarball = gz.finish().unwrap();

    let dir = tempfile::tempdir().unwrap();
    let err = extract(&tarball, "hanzo-vm", &dir.path().join("hanzo-vm")).unwrap_err();
    assert!(err.to_string().contains("no hanzo-vm binary"), "{err}");
}

/// `checkpoint list` is parsed by its first column; the header is not a
/// checkpoint and prose ("No checkpoints found.") is not one either.
#[test]
fn the_checkpoint_listing_is_read_by_name() {
    let listing = "NAME                       SIZE CREATED\nk3s                      512 MB 2h ago\nbuild                    128 MB 1d ago\n";
    assert!(has_checkpoint(listing, "k3s"));
    assert!(has_checkpoint(listing, "build"));
    assert!(!has_checkpoint(listing, "k3"));
    assert!(!has_checkpoint("", "k3s"));
    assert!(!has_checkpoint("No checkpoints found.\n", "k3s"));
}

/// The vm does not outlive its conversation, by any path. Dropping the Rpc
/// closes the leash and collects the child — and takes the group with it,
/// so a hypervisor started underneath goes too.
#[cfg(unix)]
#[test]
fn dropping_the_conversation_stops_the_vm() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("grandchild");
    // A peer that ignores EOF and holds a child of its own: SIGTERM to the
    // group is the only thing that ends it.
    let script = format!(
        "trap '' HUP; sleep 60 & echo $! > {}; sleep 60",
        file.display()
    );
    let rpc = peer(dir.path(), &script);
    let pid = rpc.child.id() as i32;

    let deadline = Instant::now() + Duration::from_secs(10);
    let grandchild = loop {
        if let Some(p) = std::fs::read_to_string(&file)
            .ok()
            .and_then(|s| s.trim().parse::<i32>().ok())
            .filter(|p| alive(*p))
        {
            break p;
        }
        assert!(Instant::now() < deadline, "the grandchild never started");
        std::thread::sleep(Duration::from_millis(50));
    };

    drop(rpc);
    assert!(!alive(pid), "the vm survived the drop");
    let deadline = Instant::now() + Duration::from_secs(5);
    while alive(grandchild) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(!alive(grandchild), "the grandchild survived the drop");
}

/// The RPC client against a fake peer: the measurement before `ready`,
/// notifications skipped, results matched by id, errors surfaced.
#[test]
fn the_rpc_client_speaks_the_stdio_protocol() {
    let dir = tempfile::tempdir().unwrap();
    let measurement = json!({"jsonrpc": "2.0", "method": "measurement", "params": launched()});
    let script = format!(
        "{}{}{}{}{}{}{}",
        line(&measurement.to_string()),
        line(r#"{"jsonrpc":"2.0","method":"ready"}"#),
        "read line; ",
        line(
            r#"{"jsonrpc":"2.0","method":"output","params":{"pid":"p1","stream":"stdout","data":""}}"#
        ),
        line(r#"{"jsonrpc":"2.0","id":1,"result":{"stdout":"ok","stderr":"","exit_code":0}}"#),
        "read line; ",
        line(r#"{"jsonrpc":"2.0","id":2,"error":{"code":-32000,"message":"exec failed"}}"#),
    );
    let mut rpc = peer(dir.path(), &script);
    assert_eq!(rpc.wait_ready().unwrap(), launched());

    let (out, err, code) = rpc.exec(&["true"]).unwrap();
    assert_eq!((out.as_str(), err.as_str(), code), ("ok", "", 0));

    let e = rpc.exec(&["false"]).unwrap_err();
    assert!(e.to_string().contains("exec failed"), "{e}");

    // The peer is done; the next read is an honest EOF error, not a hang.
    assert!(rpc.read_line().is_err());
    let _ = rpc.child.wait();
}

/// `Rpc::checkpoint` sends `{"method":"checkpoint","params":{"name":...}}` over
/// the stdio wire and reaps the exiting vm process cleanly.
#[test]
fn the_rpc_client_requests_checkpoint_over_stdio_wire() {
    let dir = tempfile::tempdir().unwrap();
    let seen = dir.path().join("seen.json");
    let measurement = json!({"jsonrpc": "2.0", "method": "measurement", "params": launched()});
    let script = format!(
        "{}{}read req; printf '%s' \"$req\" > {}; {}",
        line(&measurement.to_string()),
        line(r#"{"jsonrpc":"2.0","method":"ready"}"#),
        seen.display(),
        line(r#"{"jsonrpc":"2.0","id":1,"result":{}}"#),
    );
    let mut rpc = peer(dir.path(), &script);
    rpc.wait_ready().unwrap();
    rpc.checkpoint("k3s-2.0.2-state-staging").unwrap();
    assert!(rpc.wait_stopped(Duration::from_secs(5)).is_some());

    let req: Value = serde_json::from_str(&std::fs::read_to_string(&seen).unwrap()).unwrap();
    assert_eq!(req["method"], "checkpoint");
    assert_eq!(req["params"]["name"], "k3s-2.0.2-state-staging");
}

/// Instance working directories are swept once the vm process has exited, and
/// never removed while a process of that pid is still alive.
#[cfg(unix)]
#[test]
fn remove_instance_dir_sweeps_exited_vm_and_spares_live_process() {
    let home = tempfile::tempdir().unwrap();
    let live_pid = std::process::id();
    let live_dir = home.path().join("instances").join(live_pid.to_string());
    std::fs::create_dir_all(&live_dir).unwrap();
    std::fs::write(live_dir.join("rootfs.ext4"), b"live").unwrap();
    remove_instance_dir_in(home.path(), live_pid);
    assert!(live_dir.exists(), "must never delete a live vm's disk");

    let mut exited = Command::new("true").spawn().unwrap();
    let dead_pid = exited.id();
    exited.wait().unwrap();
    let dead_dir = home.path().join("instances").join(dead_pid.to_string());
    std::fs::create_dir_all(&dead_dir).unwrap();
    std::fs::write(dead_dir.join("rootfs.ext4"), b"stale").unwrap();
    remove_instance_dir_in(home.path(), dead_pid);
    assert!(
        !dead_dir.exists(),
        "exited vm instance disk must be removed"
    );
}

/// A vm that says `ready` without saying what it booted is refused. An
/// unmeasured cluster is not one this can file a document about, and
/// filing nothing quietly would be worse than not starting.
#[test]
fn a_vm_that_reports_no_measurement_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let mut rpc = peer(dir.path(), &line(r#"{"jsonrpc":"2.0","method":"ready"}"#));
    let e = rpc.wait_ready().unwrap_err();
    assert!(
        e.to_string().contains("without reporting a measurement"),
        "{e}"
    );
    let _ = rpc.child.wait();
}

#[test]
fn linux_x86_64_preflight_distinguishes_kvm_and_cloud_hypervisor() {
    // Missing /dev/kvm reports KVM module / firmware guidance.
    let err = check_linux_x86_64_host(KvmState::Missing, Some(Path::new("/usr/bin/ch")))
        .unwrap_err()
        .to_string();
    assert!(err.contains("/dev/kvm does not exist"), "{err}");
    assert!(err.contains("modprobe"), "{err}");

    // Unreadable /dev/kvm reports kvm group guidance.
    let err = check_linux_x86_64_host(KvmState::Unreadable, Some(Path::new("/usr/bin/ch")))
        .unwrap_err()
        .to_string();
    assert!(err.contains("usermod -aG kvm $USER"), "{err}");

    // Available /dev/kvm + missing cloud-hypervisor reports exact install guidance,
    // never "Virtualization is not enabled".
    let err = check_linux_x86_64_host(KvmState::Available, None)
        .unwrap_err()
        .to_string();
    assert!(
        err.contains(
            "cloud-hypervisor is required on Linux x86-64 (not found in PATH or ~/.local/bin)"
        ),
        "{err}"
    );
    assert!(err.contains("cloud-hypervisor-static"), "{err}");
    assert!(!err.contains("Virtualization is not enabled"), "{err}");

    // Both present passes cleanly.
    check_linux_x86_64_host(
        KvmState::Available,
        Some(Path::new("/home/u/.local/bin/cloud-hypervisor")),
    )
    .unwrap();
}

#[test]
fn vm_virtualization_preflight_gating_skips_info_subcommands() {
    let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
    assert!(!args_need_virtualization(&s(&[])));
    assert!(!args_need_virtualization(&s(&["--version"])));
    assert!(!args_need_virtualization(&s(&["--help"])));
    assert!(!args_need_virtualization(&s(&["checkpoint", "list"])));
    assert!(args_need_virtualization(&s(&[
        "checkpoint",
        "create",
        "k3s"
    ])));
    assert!(args_need_virtualization(&s(&["run", "--stdio"])));
}

#[test]
fn linux_aarch64_and_macos_virtualization_preflight_return_actionable_errors() {
    let missing_arm = check_linux_kvm(KvmState::Missing, "aarch64")
        .unwrap_err()
        .to_string();
    assert!(
        missing_arm.contains("/dev/kvm does not exist"),
        "{missing_arm}"
    );
    assert!(missing_arm.contains("modprobe kvm"), "{missing_arm}");

    let perm_arm = check_linux_kvm(KvmState::Unreadable, "aarch64")
        .unwrap_err()
        .to_string();
    assert!(perm_arm.contains("usermod -aG kvm $USER"), "{perm_arm}");
    assert!(perm_arm.contains("newgrp kvm"), "{perm_arm}");
    check_linux_kvm(KvmState::Available, "aarch64").unwrap();

    let intel_mac = check_macos_virtualization_host("x86_64", Some(1), Some(true))
        .unwrap_err()
        .to_string();
    assert!(intel_mac.contains("Apple Silicon"), "{intel_mac}");

    let no_hv = check_macos_virtualization_host("aarch64", Some(0), Some(true))
        .unwrap_err()
        .to_string();
    assert!(no_hv.contains("kern.hv_support=0"), "{no_hv}");

    let unsigned = check_macos_virtualization_host("aarch64", Some(1), Some(false))
        .unwrap_err()
        .to_string();
    assert!(
        unsigned.contains("com.apple.security.virtualization"),
        "{unsigned}"
    );
    assert!(unsigned.contains("hanzo reset --all"), "{unsigned}");

    check_macos_virtualization_host("aarch64", Some(1), Some(true)).unwrap();
    check_macos_virtualization_host("aarch64", Some(1), None).unwrap();
}
