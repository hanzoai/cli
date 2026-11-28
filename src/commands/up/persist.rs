//! Persistent microVM disk state across `hanzo stop` and `hanzo start` / `hanzo up`.
//!
//! `hanzo-vm run --stdio --from <checkpoint>` clones `<checkpoint>.ext4` into a
//! live VirtioBlock working copy (`~/.hanzo/vm/instances/<pid>/rootfs.ext4`).
//! Keeping k3s, containerd's unpacked `hanzo.local/*` OCI layers, and all
//! local-path PVCs (SQLite WAL + NATS JetStream) on that ext4 block device
//! preserves full POSIX + shared `mmap` semantics on both Linux KVM and Apple
//! Silicon `Virtualization.framework`.
//!
//! On `hanzo stop`, the supervisor cordons the node (leaving `Deployment`
//! `spec.replicas` untouched), gracefully deletes pods so Cloud flushes SQLite
//! and JetStream and unmounts PVCs, stops `k3s` and `containerd`, and asks
//! `hanzo-vm` over `--stdio` (`method: "checkpoint"`) to clone the quiescent
//! rootfs to `<state_staging>.ext4` before atomically renaming it to
//! `<state_checkpoint>.ext4`. `hanzo reset` deletes `<state_checkpoint>.ext4`
//! when a clean slate is explicitly requested.

use anyhow::{Context, Result};
use colored::*;
use std::path::{Path, PathBuf};
use std::time::Duration;
use vm_measure::Cache;

#[cfg(unix)]
use super::control;
use super::manifest::{ground_for, has_workload_overrides, select_cloud_image, workload_for};
use super::{
    adopt_native_linux_kubeconfig, checkpoint, kubeconfig_path, read_state, running, write_state,
    Boot, KubeClient, SUPERVISOR, UP_TIMEOUT,
};
use crate::commands::vm::{self, Rpc};
use crate::config::Config;
use crate::image;

/// Guest shutdown script run before checkpointing the live ext4 rootfs:
/// 1. Cordon the node so DeploymentController does not schedule replacement pods
///    while we drain, without mutating `Deployment.spec.replicas` in k3s `state.db`.
/// 2. Delete non-`kube-system` pods first so Cloud and user apps receive SIGTERM,
///    checkpoint SQLite WAL, flush NATS JetStream, and unmount PVCs cleanly.
/// 3. Delete `kube-system` pods, stop `k3s` and `containerd`, and `sync`.
pub(crate) const QUIESCE_GUEST_SCRIPT: &str = "\
if k3s kubectl get nodes >/dev/null 2>&1; then
  k3s kubectl cordon --all >/dev/null 2>&1 || true
  k3s kubectl delete pods --all --field-selector=metadata.namespace!=kube-system -A --grace-period=15 --timeout=20s >/dev/null 2>&1 || true
  k3s kubectl delete pods --all -n kube-system --grace-period=5 --timeout=10s >/dev/null 2>&1 || true
fi
pkill -TERM -x k3s 2>/dev/null || true
i=0
while pgrep -x k3s >/dev/null 2>&1 && [ \"$i\" -lt 50 ]; do
  sleep 0.2
  i=$((i + 1))
done
pkill -KILL -x k3s 2>/dev/null || true
pkill -TERM -f containerd 2>/dev/null || true
i=0
while pgrep -f containerd >/dev/null 2>&1 && [ \"$i\" -lt 25 ]; do
  sleep 0.2
  i=$((i + 1))
done
pkill -KILL -f containerd 2>/dev/null || true
sync";

/// Name of the saved runtime state checkpoint under `~/.hanzo/vm/checkpoints/`.
pub(crate) fn state_checkpoint() -> String {
    format!("k3s-{}-state", vm::VM_VERSION)
}

/// Temporary checkpoint name written before atomic `rename` to [`state_checkpoint`],
/// ensuring macOS `clonefile` never sees `EEXIST` and interrupted saves never
/// clobber the previous state checkpoint.
pub(crate) fn state_staging_checkpoint() -> String {
    format!("k3s-{}-state-staging", vm::VM_VERSION)
}

pub(crate) fn checkpoint_file(vm_home: &Path, name: &str) -> PathBuf {
    vm_home.join("checkpoints").join(format!("{name}.ext4"))
}

pub(crate) fn has_state_checkpoint_in(vm_home: &Path) -> bool {
    std::fs::metadata(checkpoint_file(vm_home, &state_checkpoint()))
        .is_ok_and(|m| m.is_file() && m.len() > 0)
}

pub(crate) fn has_state_checkpoint() -> bool {
    vm::vm_home_dir()
        .ok()
        .is_some_and(|home| has_state_checkpoint_in(&home))
}

/// Choose `(disk_mb, from_checkpoint)` for a boot against `vm_home`.
///
/// When a saved runtime state checkpoint exists, boot from it and never pass a
/// `--disk-size` smaller than the saved ext4 image (which `hanzo-vm` refuses).
pub(crate) fn boot_target_in(boot: &Boot, vm_home: &Path) -> (u64, String) {
    let state = state_checkpoint();
    if let Ok(meta) = std::fs::metadata(checkpoint_file(vm_home, &state)) {
        if meta.is_file() && meta.len() > 0 {
            let saved_mb = meta.len().div_ceil(1024 * 1024);
            return (boot.disk_mb.max(saved_mb), state);
        }
    }
    (boot.disk_mb, checkpoint())
}

/// Extract an existing `storage: <N>Mi` request from a saved `GROUND` YAML so a
/// resumed cluster keeps its bound PVC's immutable size across disk resizes while
/// still applying any updated RBAC or Namespace documents.
pub(crate) fn existing_claim_mb(ground_yaml: &str) -> Option<u64> {
    ground_yaml.lines().find_map(|line| {
        line.trim()
            .strip_prefix("storage:")?
            .trim()
            .strip_suffix("Mi")?
            .trim()
            .parse::<u64>()
            .ok()
    })
}

/// Quiesce the guest, optionally save its ext4 disk to [`state_checkpoint`],
/// reap the exiting `hanzo-vm` child, and sweep any leftover per-instance dir.
pub(crate) fn stop_and_save_in(
    rpc: &mut Rpc,
    vm_home: &Path,
    save: bool,
    progress: &mut dyn FnMut(&str),
) -> Result<Option<String>> {
    let vm_pid = rpc.child.id();
    if !save {
        progress("stopping microVM…");
        rpc.close_stdin();
        let _ = rpc.wait_stopped(Duration::from_secs(10));
        vm::remove_instance_dir_in(vm_home, vm_pid);
        return Ok(None);
    }

    progress("draining workloads and stopping k3s…");
    let _ = rpc.exec(&["sh", "-c", QUIESCE_GUEST_SCRIPT]);

    progress("saving runtime checkpoint…");
    let checkpoints_dir = vm_home.join("checkpoints");
    std::fs::create_dir_all(&checkpoints_dir)
        .with_context(|| format!("creating {}", checkpoints_dir.display()))?;

    let staging_name = state_staging_checkpoint();
    let state_name = state_checkpoint();
    let staging_path = checkpoint_file(vm_home, &staging_name);
    let final_path = checkpoint_file(vm_home, &state_name);
    let _ = std::fs::remove_file(&staging_path);

    if let Err(e) = rpc.checkpoint(&staging_name) {
        let _ = std::fs::remove_file(&staging_path);
        return Err(e);
    }

    let _ = rpc.wait_stopped(Duration::from_secs(15));
    vm::remove_instance_dir_in(vm_home, vm_pid);

    std::fs::rename(&staging_path, &final_path).with_context(|| {
        format!(
            "promoting {} to {}",
            staging_path.display(),
            final_path.display()
        )
    })?;

    warm_digest(vm_home, &final_path);
    Ok(Some(state_name))
}

fn warm_digest(vm_home: &Path, checkpoint_ext4: &Path) {
    let mut cache = Cache::open(vm_home.join("digests.json"));
    if cache.digest(checkpoint_ext4).is_ok() {
        cache.save();
    }
}

/// Delete any saved runtime state checkpoint in `vm_home`, returning `true` when
/// one was present. Leaves the base `k3s-<ver>-airgap` checkpoint untouched.
pub(crate) fn delete_state_checkpoint_in(vm_home: &Path) -> bool {
    let mut removed = false;
    for name in [state_checkpoint(), state_staging_checkpoint()] {
        for ext in ["ext4", "idx"] {
            let p = vm_home.join("checkpoints").join(format!("{name}.{ext}"));
            if std::fs::remove_file(&p).is_ok() && ext == "ext4" && name == state_checkpoint() {
                removed = true;
            }
        }
    }
    removed
}

pub(crate) fn delete_state_checkpoint() -> bool {
    vm::vm_home_dir()
        .ok()
        .is_some_and(|home| delete_state_checkpoint_in(&home))
}

/// Decide whether a resumed or warm cluster keeps its live Cloud Deployment
/// image (`Some(live)`) or resolves a newly requested image (`None`).
///
/// Resuming after `hanzo stop` or `hanzo down` with default flags preserves
/// `hanzo.local/*` builds and pinned images alike; only an explicit `--cloud`
/// override or `--strict-registry` against a non-Hanzo image replaces it.
pub(crate) fn preserved_cluster_image<'a>(
    boot: &Boot,
    live_image: Option<&'a str>,
) -> Option<&'a str> {
    let live = live_image?;
    if boot.cloud != super::CLOUD {
        return None;
    }
    if boot.strict_registry && !super::manifest::is_hanzo_registry(live) {
        return None;
    }
    Some(live)
}

/// `boot` with the shape of the runtime that is actually running.
pub(crate) fn running_shape(dir: &Path, boot: Boot) -> Boot {
    #[cfg(unix)]
    if let Ok(facts) = control::call(dir, &control::Request::Facts, |_| {}) {
        let n = |k: &str| facts.get(k).and_then(|v| v.as_u64()).unwrap_or(0);
        if n("memory_mb") > 0 && n("disk_mb") > 0 {
            return Boot {
                cpus: n("cpus").max(1) as u32,
                memory_mb: n("memory_mb"),
                disk_mb: n("disk_mb"),
                ..boot
            };
        }
    }
    let _ = dir;
    boot
}

/// Reconcile a warm or state-checkpoint-resumed cluster: preserve `hanzo.local/*`
/// builds across `hanzo stop` / `hanzo down`, honor explicit overrides, and wait
/// for the Cloud Deployment's rollout before reporting ready.
pub(crate) async fn reconcile_warm_cluster(
    cfg: &mut Config,
    dir: &Path,
    boot: Boot,
    native_warm: bool,
    resumed_from_state: bool,
) -> Result<()> {
    let pid = running(dir, SUPERVISOR).unwrap_or(0);
    let state = read_state(dir).unwrap_or_else(|| {
        if native_warm {
            "native-linux".into()
        } else {
            "unknown".into()
        }
    });
    let kc = if native_warm {
        adopt_native_linux_kubeconfig()?
    } else {
        kubeconfig_path()?
    };
    let kube = KubeClient::from_kubeconfig(&kc)?;
    let replicas = kube.cloud_replicas().await?;
    let overrides = has_workload_overrides(&boot);
    if overrides || replicas.is_none() {
        let live_image = match replicas {
            Some(_) => kube.cloud_image().await?,
            None => None,
        };
        let cloud = match preserved_cluster_image(&boot, live_image.as_deref()) {
            Some(image) => image.to_string(),
            None => {
                let has_hanzo_oci = !image::credentials_for("oci.hanzo.ai", cfg).await.is_empty();
                let selected =
                    select_cloud_image(&boot.cloud, boot.strict_registry, has_hanzo_oci)?;
                let pinned_ref = image::pin_with_config(
                    &reqwest::Client::new(),
                    &selected,
                    image::architecture(),
                    cfg,
                )
                .await?;
                if let Some(secret) =
                    image::pull_secret_for(&reqwest::Client::new(), &pinned_ref, cfg).await
                {
                    kube.ensure_pull_secret(&secret).await?;
                }
                pinned_ref
            }
        };
        let pinned = Boot {
            cloud,
            ..running_shape(dir, boot)
        };
        let manifest = workload_for(&pinned)?;
        kube.apply_workload(&ground_for(&pinned), &manifest).await?;
        write_state(dir, "cloud-starting");
        kube.rollout("hanzo", "cloud", UP_TIMEOUT).await?;
        write_state(dir, "ready");
        let verb = if replicas.is_none() {
            "deployed"
        } else {
            "updated & started"
        };
        if native_warm {
            println!("{} hanzo {verb} on native Linux k3s", "✓".green());
        } else {
            println!("{} hanzo {verb} on k3s (supervisor pid {pid})", "✓".green());
        }
    } else if state == "stopped" || replicas == Some(0) {
        kube.scale_cloud(1).await?;
        write_state(dir, "cloud-starting");
        kube.rollout("hanzo", "cloud", UP_TIMEOUT).await?;
        write_state(dir, "ready");
        println!(
            "{} hanzo started on k3s (skipped image pulls, data preserved)",
            "✓".green()
        );
    } else {
        kube.rollout("hanzo", "cloud", UP_TIMEOUT).await?;
        write_state(dir, "ready");
        if resumed_from_state {
            println!(
                "{} hanzo resumed from saved checkpoint (supervisor pid {pid}, data & local images preserved)",
                "✓".green()
            );
        } else if native_warm {
            println!("{} already running (native Linux k3s, ready)", "●".green());
        } else {
            println!(
                "{} already running (supervisor pid {pid}, ready)",
                "●".green()
            );
        }
    }
    Ok(())
}
