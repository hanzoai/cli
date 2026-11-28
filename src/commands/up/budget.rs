//! What the local runtime's memory and disk are divided into, decided before a
//! workload is given to Kubernetes.
//!
//! The runtime is one node of a size the person chose, and everything Cloud is told
//! about its share follows from that size here, in one place: the memory its pod
//! requests, the file store its message bus may reserve, the volume its data is
//! kept on. A number picked somewhere else is a number that does not add up with
//! these, and the scheduler or the bus is where that is found out.

use anyhow::{bail, Result};

/// What the node keeps for itself: the kernel, k3s, containerd and kube-system.
/// Measured on the 4 GiB default with Cloud running: about 700 MiB.
pub(crate) const SYSTEM_MB: u64 = 768;

/// The least Cloud's pod may request. The host reads its own request and holds
/// that many apps resident ((request - 200) / 71, `manifest.Warm` in cloud); the
/// apps it starts with itself and never stops are 18 today, so a request under
/// 1478 MiB is a host that evicts an app it is about to be asked for. Idle, the pod
/// was measured at 480 MiB: this is a reservation, not what it uses.
pub(crate) const CLOUD_MIN_MB: u64 = 1536;

/// The most it requests however large the node is: a request is a reservation,
/// and what it does not use nothing else on the node may be scheduled into.
pub(crate) const CLOUD_MAX_MB: u64 = 4096;

/// The smallest runtime Cloud is deployed into: the node's own share and two
/// minimal pods. See [`plan`] for why two.
pub(crate) const MEMORY_MIN_MB: u64 = SYSTEM_MB + 2 * CLOUD_MIN_MB;

/// The smallest disk: the base system, one Cloud image unpacked beside its
/// compressed layers, and room for data.
pub(crate) const DISK_MIN_MB: u64 = 16 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Budget {
    /// The runtime's memory, in MiB.
    pub memory_mb: u64,
    /// The runtime's disk, in MiB.
    pub disk_mb: u64,
    /// What Cloud's pod requests, in MiB.
    pub cloud_mb: u64,
    /// The file store Cloud's message bus may hold, in bytes
    /// (`CLOUD_PUBSUB_MAX_STORE`). Its streams reserve shares of this, not their
    /// production sizes.
    pub store_bytes: u64,
    /// The size asked of the volume Cloud's data is kept on, in MiB.
    pub data_mb: u64,
}

/// The size asked of Cloud's data volume on a disk of `disk_mb`: a quarter. The
/// provisioner does not enforce it; it is what the claim says the data may grow to.
pub(crate) fn data_mb(disk_mb: u64) -> u64 {
    (disk_mb / 4).max(1024)
}

/// Divide a runtime of `memory_mb` and `disk_mb`.
///
/// **Memory.** Cloud's pod requests half of what the node does not keep for
/// itself, so that two of them fit. One replica runs, and the Deployment replaces
/// it rather than surging; but a pod that is deleted is still on the node, and
/// still counted, while its replacement is scheduled. A request sized for one is
/// what the scheduler answered with `Insufficient memory`.
///
/// **Disk.** The bus is given a sixteenth of the disk, between 256 MiB and 4 GiB:
/// its streams hold what consumers have not drained yet, which on one developer's
/// node is little. The data volume is asked for a quarter.
pub(crate) fn plan(memory_mb: u64, disk_mb: u64) -> Result<Budget> {
    if memory_mb < MEMORY_MIN_MB {
        bail!(
            "the runtime has {memory_mb} MiB of memory and Cloud needs {MEMORY_MIN_MB}: \
             {SYSTEM_MB} for the node itself and {CLOUD_MIN_MB} for its pod, twice, so a \
             restart can be scheduled — `hanzo stop`, then `hanzo up --memory 4GiB`"
        );
    }
    if disk_mb < DISK_MIN_MB {
        bail!(
            "the runtime has {disk_mb} MiB of disk and Cloud needs {DISK_MIN_MB} — \
             `hanzo stop`, then `hanzo up --disk-size 32GiB`"
        );
    }
    let cloud_mb = ((memory_mb - SYSTEM_MB) / 2).clamp(CLOUD_MIN_MB, CLOUD_MAX_MB);
    // Whole multiples of 64 MiB: a request nobody has to round to read.
    let cloud_mb = cloud_mb / 64 * 64;
    let store_mb = (disk_mb / 16).clamp(256, 4096);
    Ok(Budget {
        memory_mb,
        disk_mb,
        cloud_mb,
        store_bytes: store_mb * 1024 * 1024,
        data_mb: data_mb(disk_mb),
    })
}

impl Budget {
    /// One line a person can check the arithmetic of.
    pub(crate) fn describe(&self) -> String {
        format!(
            "runtime {} MiB memory / {} MiB disk — node {} MiB, Cloud requests {} MiB \
             (two fit: {} of {}), bus store {} MiB, data volume {} MiB",
            self.memory_mb,
            self.disk_mb,
            SYSTEM_MB,
            self.cloud_mb,
            SYSTEM_MB + 2 * self.cloud_mb,
            self.memory_mb,
            self.store_bytes / (1024 * 1024),
            self.data_mb,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn two_cloud_pods_fit_every_supported_runtime() {
        for memory in [MEMORY_MIN_MB, 4096, 6144, 8192, 16384, 65536] {
            let b = plan(memory, 32768).unwrap();
            assert!(
                SYSTEM_MB + 2 * b.cloud_mb <= memory,
                "{memory} MiB: two pods of {} do not fit",
                b.cloud_mb
            );
            assert!(b.cloud_mb >= CLOUD_MIN_MB && b.cloud_mb <= CLOUD_MAX_MB);
        }
    }

    #[test]
    fn the_default_runtime_is_divided_as_documented() {
        let b = plan(4096, 32768).unwrap();
        assert_eq!(b.cloud_mb, 1664);
        assert_eq!(b.store_bytes, 2048 * 1024 * 1024);
        assert_eq!(b.data_mb, 8192);
        // The request that was scheduled before, and did not fit twice.
        const { assert!(SYSTEM_MB + 2 * 2048 > 4096) };
    }

    #[test]
    fn a_runtime_too_small_is_refused_before_anything_is_scheduled() {
        let e = plan(3072, 32768).unwrap_err().to_string();
        assert!(e.contains("3072 MiB of memory"), "{e}");
        // The default is a supported runtime, and the smallest that is.
        assert_eq!(MEMORY_MIN_MB, 3840);
        assert!(plan(4096, 32768).is_ok());
        let e = plan(4096, 8192).unwrap_err().to_string();
        assert!(e.contains("8192 MiB of disk"), "{e}");
    }

    #[test]
    fn the_bus_store_follows_the_disk_within_its_bounds() {
        assert_eq!(plan(4096, 16384).unwrap().store_bytes, 1024 << 20);
        assert_eq!(plan(4096, 32768).unwrap().store_bytes, 2048 << 20);
        assert_eq!(plan(4096, 1024 * 1024).unwrap().store_bytes, 4096 << 20);
    }
}
