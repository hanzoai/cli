//! Supervisor, attestation, and guest k3s bootstrap for `hanzo up` / `hanzo start`.

use anyhow::{bail, Context, Result};
use std::path::Path;
use std::time::{Duration, Instant};
use vm_measure::{Log, Measurement};

use super::manifest::{ground, workload_for, ACCOUNT};
use super::{
    budget, clear_pid, kubeconfig_path, measure_path, persist, up_dir, write_pid, write_state,
    Boot, CLOUD_NODE_PORT, DATA, GROUND, K3S_PORT, KEYFILE, KEYFILE_FALLBACK, KUBECONFIG,
    LOCAL_PORT, PULLFILE, PULLFILE_FALLBACK, PULL_SECRET, READY_TIMEOUT, SUPERVISOR, VM, WORKLOAD,
};
use crate::commands::host::key;
use crate::commands::vm::{self, Rpc};
use crate::config::Config;
use crate::image;

#[cfg(test)]
pub(crate) fn run_args(boot: &Boot) -> Vec<String> {
    vm::run_args(
        boot.cpus,
        boot.memory_mb,
        boot.disk_mb,
        &[(K3S_PORT, K3S_PORT), (LOCAL_PORT, CLOUD_NODE_PORT)],
        &super::checkpoint(),
    )
}

pub(crate) fn run_args_in(boot: &Boot, vm_home: &Path) -> Vec<String> {
    let (disk_mb, from) = persist::boot_target_in(boot, vm_home);
    vm::run_args(
        boot.cpus,
        boot.memory_mb,
        disk_mb,
        &[(K3S_PORT, K3S_PORT), (LOCAL_PORT, CLOUD_NODE_PORT)],
        &from,
    )
}

/// Lay the ground now, by our own hand. k3s would apply the same file from its
/// manifests directory in its own time; the steps after this one need the namespace
/// and the account to exist when they run, not eventually.
fn ensure_ground(rpc: &mut Rpc) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        match rpc.exec(&["k3s", "kubectl", "apply", "-f", GROUND])? {
            (_, _, 0) => return Ok(()),
            // The API server answers before every group it serves is ready (rbac
            // among the last); the same apply succeeds a moment later.
            (_, _, _) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_secs(1));
            }
            (_, err, _) => bail!("laying the ground: {}", err.trim()),
        }
    }
}

/// Give the cluster the workload: the Deployment and its Service, after everything
/// they need is in place. Applied here rather than left in k3s's manifests
/// directory, which would create the pod at boot, before its pull credential and
/// its key exist — the pod that failed its first pull and had to be deleted, and
/// whose replacement was scheduled beside it.
fn apply_workload(rpc: &mut Rpc, manifest: &str) -> Result<()> {
    rpc.write_file(WORKLOAD, manifest)?;
    match rpc.exec(&["k3s", "kubectl", "apply", "-f", WORKLOAD])? {
        (_, _, 0) => Ok(()),
        (_, err, _) => bail!("deploying the workload: {}", err.trim()),
    }
}

fn plant_key(rpc: &mut Rpc) -> Result<()> {
    let k = key();
    let path = match rpc.write_file(KEYFILE, &k) {
        Ok(()) => KEYFILE,
        Err(_) => {
            rpc.write_file(KEYFILE_FALLBACK, &k)?;
            KEYFILE_FALLBACK
        }
    };
    let _ = rpc.exec(&["chmod", "600", path]);
    let made = rpc.exec(&[
        "k3s",
        "kubectl",
        "create",
        "secret",
        "generic",
        "cloud",
        "-n",
        "hanzo",
        &format!("--from-file=master={path}"),
    ]);
    let _ = rpc.exec(&["rm", "-f", path]);
    match made {
        Ok((_, err, code)) if code != 0 && !err.contains("already exists") => {
            bail!("creating the at-rest key: {}", err.trim())
        }
        Ok(_) => Ok(()),
        Err(e) => Err(e),
    }
}

fn plant_pull_secret(rpc: &mut Rpc, dockerconfigjson: &str) -> Result<()> {
    let path = match rpc.write_file(PULLFILE, dockerconfigjson) {
        Ok(()) => PULLFILE,
        Err(_) => {
            rpc.write_file(PULLFILE_FALLBACK, dockerconfigjson)?;
            PULLFILE_FALLBACK
        }
    };
    let _ = rpc.exec(&["chmod", "600", path]);
    let _ = rpc.exec(&[
        "k3s",
        "kubectl",
        "delete",
        "secret",
        PULL_SECRET,
        "-n",
        "hanzo",
        "--ignore-not-found",
    ]);
    let made = rpc.exec(&[
        "k3s",
        "kubectl",
        "create",
        "secret",
        "generic",
        PULL_SECRET,
        "-n",
        "hanzo",
        "--type=kubernetes.io/dockerconfigjson",
        &format!("--from-file=.dockerconfigjson={path}"),
    ]);
    let _ = rpc.exec(&["rm", "-f", path]);
    match made {
        Ok((_, err, code)) if code != 0 => {
            bail!("creating the registry pull secret: {}", err.trim())
        }
        Ok(_) => {}
        Err(e) => return Err(e),
    }

    // On the account the pod runs as, which the ground has already made. The pod
    // does not exist yet, so there is none to restart.
    let patch = format!(r#"{{"imagePullSecrets":[{{"name":"{PULL_SECRET}"}}]}}"#);
    match rpc.exec(&[
        "k3s",
        "kubectl",
        "patch",
        "serviceaccount",
        ACCOUNT,
        "-n",
        "hanzo",
        "-p",
        &patch,
    ])? {
        (_, _, 0) => Ok(()),
        (_, err, _) => bail!("attaching registry pull secret: {}", err.trim()),
    }
}

pub(crate) fn deployed(image: &str, manifest: &str) -> Log {
    let mut log = Log::default();
    log.text("image", image);
    log.bytes("manifest", WORKLOAD, manifest.as_bytes());
    log
}

pub(crate) fn record(
    dir: &Path,
    rpc: &mut Rpc,
    launch: Log,
    image: &str,
    manifest: &str,
) -> Result<()> {
    let mut m = Measurement {
        launch,
        workload: deployed(image, manifest),
        ..Measurement::default()
    };
    m.hardware = rpc.attest(&m.bind_hex())?;
    let path = measure_path(dir);
    std::fs::write(&path, m.to_json_pretty()).with_context(|| format!("writing {}", path.display()))
}

pub fn attest() -> Result<()> {
    println!("{}", measured(&up_dir()?)?.to_json_pretty());
    Ok(())
}

pub(crate) fn measured(dir: &Path) -> Result<Measurement> {
    let path = measure_path(dir);
    let text = std::fs::read_to_string(&path)
        .with_context(|| format!("reading {} — is a cluster up?", path.display()))?;
    Measurement::from_json(&text).with_context(|| format!("in {}", path.display()))
}

pub(crate) fn server_argv() -> [&'static str; 10] {
    [
        "k3s",
        "server",
        "--data-dir",
        DATA,
        "--write-kubeconfig",
        KUBECONFIG,
        "--disable",
        "traefik",
        "--disable",
        "metrics-server",
    ]
}

pub(crate) fn node_ready(out: &str) -> bool {
    out.lines().any(|l| {
        l.split_whitespace()
            .nth(1)
            .is_some_and(|status| status.split(',').any(|c| c == "Ready"))
    })
}

pub(crate) fn rewrite_server(yaml: &str) -> String {
    let mut out: String = yaml
        .lines()
        .map(|l| match l.find("server:") {
            Some(i) if l[..i].chars().all(|c| c == ' ') => {
                format!("{}server: https://127.0.0.1:{K3S_PORT}", &l[..i])
            }
            _ => l.to_string(),
        })
        .collect::<Vec<_>>()
        .join("\n");
    out.push('\n');
    out
}

pub(crate) fn write_kubeconfig(path: &Path, yaml: &str) -> Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    }
    std::fs::write(path, yaml).with_context(|| format!("writing {}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
            .with_context(|| format!("chmod 600 {}", path.display()))?;
    }
    Ok(())
}

pub async fn supervise(cfg: &mut Config, boot: Boot, runtime_only: bool) -> Result<()> {
    let dir = up_dir()?;
    write_pid(&dir, SUPERVISOR, std::process::id())?;
    let bin = vm::resolve_or_install().await?;
    let pull_secret = if runtime_only {
        None
    } else {
        image::pull_secret_for(&reqwest::Client::new(), &boot.cloud, cfg).await
    };
    let out = drive(&dir, &boot, &bin, pull_secret.as_deref(), runtime_only);
    if let Err(e) = &out {
        write_state(&dir, &format!("error: {e:#}"));
    }
    clear_pid(&dir, VM);
    clear_pid(&dir, SUPERVISOR);
    out
}

fn drive(
    dir: &Path,
    boot: &Boot,
    bin: &Path,
    pull_secret: Option<&str>,
    runtime_only: bool,
) -> Result<()> {
    write_state(dir, "boot");
    let vm_home = vm::vm_home_dir()?;
    let log = dir.join("supervisor.log");
    let mut rpc = Rpc::start(bin, &run_args_in(boot, &vm_home), &log)?;
    let vm_pid = rpc.child.id();
    write_pid(dir, VM, vm_pid)?;
    let out = drive_started(dir, boot, &vm_home, &mut rpc, pull_secret, runtime_only);
    drop(rpc);
    vm::remove_instance_dir_in(&vm_home, vm_pid);
    out
}

fn drive_started(
    dir: &Path,
    boot: &Boot,
    vm_home: &Path,
    rpc: &mut Rpc,
    pull_secret: Option<&str>,
    runtime_only: bool,
) -> Result<()> {
    #[cfg(not(unix))]
    let _ = vm_home;
    let launch = rpc.wait_ready()?;

    // Preserve any bound PVC storage size from a resumed state checkpoint so
    // `kubectl apply -f GROUND` never mutates an immutable claim size on resize.
    let claim_mb = rpc
        .read_file(GROUND)
        .ok()
        .and_then(|b| String::from_utf8(b).ok())
        .and_then(|s| persist::existing_claim_mb(&s))
        .unwrap_or_else(|| budget::data_mb(boot.disk_mb));
    rpc.write_file(GROUND, &ground(claim_mb))?;
    let manifest = if runtime_only {
        None
    } else {
        let manifest = workload_for(boot)?;
        record(dir, rpc, launch, &boot.cloud, &manifest)?;
        Some(manifest)
    };

    write_state(dir, "k3s");
    rpc.spawn(&server_argv())?;
    let deadline = Instant::now() + READY_TIMEOUT;
    loop {
        match rpc.exec(&["k3s", "kubectl", "get", "nodes", "--no-headers"]) {
            Ok((out, _, 0)) if node_ready(&out) => break,
            Ok(_) => {}
            Err(e) if rpc.exited().is_some() => return Err(e),
            Err(_) => {}
        }
        if Instant::now() >= deadline {
            bail!(
                "k3s reported no Ready node within {}s",
                READY_TIMEOUT.as_secs()
            );
        }
        std::thread::sleep(Duration::from_secs(3));
    }

    // A checkpoint saved on `hanzo stop` cordoned the node so pods could drain
    // without mutating `Deployment.spec.replicas`; uncordon now so resumed
    // workloads schedule onto it immediately.
    let _ = rpc.exec(&["k3s", "kubectl", "uncordon", "--all"]);

    ensure_ground(rpc)?;
    if let Some(secret) = pull_secret {
        plant_pull_secret(rpc, secret)?;
    }
    plant_key(rpc)?;
    if let Some(manifest) = &manifest {
        apply_workload(rpc, manifest)?;
    }

    let yaml = String::from_utf8(rpc.read_file(KUBECONFIG)?)
        .context("the guest kubeconfig is not utf-8")?;
    write_kubeconfig(&kubeconfig_path()?, &rewrite_server(&yaml))?;
    // Proves k3s runtime reachability; callers deploying or resuming Cloud wait
    // for the Deployment's observed generation and ready replicas before `ready`.
    write_state(dir, "runtime-ready");

    // Hold the vm, and between looks at it answer whoever asks through the
    // control socket: this process is the only one that can reach the guest.
    #[cfg(unix)]
    let listener = super::control::listen(dir)?;
    let status = loop {
        if let Some(status) = rpc.exited() {
            break Some(status);
        }
        #[cfg(unix)]
        if super::control::serve_pending_in(&listener, rpc, boot, vm_home) {
            break rpc.wait_stopped(Duration::from_secs(5));
        }
        std::thread::sleep(Duration::from_millis(150));
    };
    #[cfg(unix)]
    let _ = std::fs::remove_file(super::control::socket_path(dir));
    match status {
        Some(status) => write_state(dir, &format!("down ({status})")),
        None => write_state(dir, "down"),
    }
    Ok(())
}
