//! `hanzo doctor`, the local runtime's part: what the runtime was given, what Cloud
//! was told it has, and whether the two agree with what is running.
//!
//! Each check answers a question someone had to read a pod's log to ask: is this my
//! build or a pulled one, does the pod fit the node, may Cloud make the Kubernetes
//! calls it makes, did the message bus get a store it could use. They are read from
//! the cluster and the runtime as they are, never from what this CLI meant to set.

use serde_json::Value;
use std::time::Duration;

use super::manifest::ACCOUNT;
use super::{budget, kubeconfig_path, running, up_dir, KubeClient, LOCAL_PORT, SUPERVISOR};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Check {
    pub name: &'static str,
    /// `ok`, `warn`, `fail` or `info`.
    pub status: &'static str,
    pub detail: String,
}

fn check(name: &'static str, status: &'static str, detail: impl Into<String>) -> Check {
    Check {
        name,
        status,
        detail: detail.into(),
    }
}

/// The Kubernetes requests Cloud makes on a local runtime: verb, API group,
/// resource, namespace ("" is the cluster). The grants in `manifest::ground` are
/// this list; doctor asks the API server about each.
pub(crate) const GRANTS: [(&str, &str, &str, &str); 4] = [
    ("list", "", "pods", ""),
    ("watch", "", "pods", ""),
    ("list", "batch", "jobs", "hanzo"),
    ("list", "batch", "jobs", "hanzo-build"),
];

/// This machine's memory, in MiB, when it can be read.
fn host_memory_mb() -> Option<u64> {
    #[cfg(target_os = "linux")]
    {
        let s = std::fs::read_to_string("/proc/meminfo").ok()?;
        let kb: u64 = s
            .lines()
            .find(|l| l.starts_with("MemTotal:"))?
            .split_whitespace()
            .nth(1)?
            .parse()
            .ok()?;
        Some(kb / 1024)
    }
    #[cfg(target_os = "macos")]
    {
        let out = std::process::Command::new("sysctl")
            .args(["-n", "hw.memsize"])
            .output()
            .ok()?;
        let bytes: u64 = String::from_utf8_lossy(&out.stdout).trim().parse().ok()?;
        Some(bytes >> 20)
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        None
    }
}

/// What the runtime's shape leaves a host of `host_mb`: the verdict and the words.
pub(crate) fn host_share(host_mb: u64, vm_mb: u64) -> (&'static str, String) {
    let left = host_mb.saturating_sub(vm_mb);
    let said = format!(
        "this machine has {host_mb} MiB; the runtime holds {vm_mb}, leaving {left} for everything else \
         (local model inference runs on the host, outside the runtime)"
    );
    // Under 3 GiB for the host's own work is a machine that swaps.
    if left < 3072 {
        ("warn", said)
    } else {
        ("ok", said)
    }
}

/// Disk: the kubelet removes unused images above 85% and evicts pods above 95%.
pub(crate) fn disk_verdict(total: u64, free: u64) -> (&'static str, String) {
    if total == 0 {
        return ("info", "the node's disk could not be read".into());
    }
    let used = 100 - free * 100 / total;
    let said = format!("{} MiB free of {} ({used}% used)", free >> 20, total >> 20);
    match used {
        95.. => (
            "fail",
            format!("{said}: pods are evicted at this level — `hanzo stop` and start again"),
        ),
        85.. => (
            "warn",
            format!("{said}: the kubelet is removing images no pod uses"),
        ),
        _ => ("ok", said),
    }
}

/// What a Deployment says about the image it runs and how.
pub(crate) fn provenance(deploy: &Value) -> Check {
    let s = |ptr: &str| deploy.pointer(ptr).and_then(Value::as_str).unwrap_or("");
    let c = "/spec/template/spec/containers/0";
    let image = s(&format!("{c}/image"));
    let policy = s(&format!("{c}/imagePullPolicy"));
    let local = super::manifest::is_local(image);
    match (local, policy) {
        (true, "Never") => check(
            "image",
            "ok",
            format!("{image} — built on this machine, loaded into the runtime, pull policy Never"),
        ),
        (true, other) => check(
            "image",
            "fail",
            format!("{image} is a local build with pull policy {other}: it must be Never, or a missing image becomes a pull"),
        ),
        (false, _) => check(
            "image",
            "info",
            format!("{image} — a published image, pulled from its registry (`hanzo up --local` runs this machine's source)"),
        ),
    }
}

/// The local runtime's diagnostics. Empty when no runtime is up: there is nothing
/// to diagnose, and the lifecycle section already says how to start one.
pub async fn checks() -> Vec<Check> {
    let mut out = Vec::new();
    let Ok(dir) = up_dir() else { return out };
    if running(&dir, SUPERVISOR).is_none() {
        return out;
    }

    // The runtime, from the process that booted it.
    let mut shape = None;
    #[cfg(unix)]
    match super::control::call(&dir, &super::control::Request::Facts, |_| {}) {
        Ok(f) => {
            let n = |k: &str| f.get(k).and_then(Value::as_u64).unwrap_or(0);
            shape = Some((n("memory_mb"), n("disk_mb")));
            out.push(check(
                "runtime",
                "ok",
                format!(
                    "{} CPUs, {} MiB memory, {} MiB disk",
                    n("cpus"),
                    n("memory_mb"),
                    n("disk_mb")
                ),
            ));
            match budget::plan(n("memory_mb"), n("disk_mb")) {
                Ok(b) => out.push(check("budget", "ok", b.describe())),
                Err(e) => out.push(check("budget", "fail", format!("{e:#}"))),
            }
            if let Some(host) = host_memory_mb() {
                let (status, said) = host_share(host, n("memory_mb"));
                out.push(check("host memory", status, said));
            }
            let (status, said) = disk_verdict(n("disk_bytes"), n("disk_free_bytes"));
            out.push(check("node disk", status, said));
            out.push(check(
                "node memory",
                if n("memory_available_kb") < 256 * 1024 {
                    "warn"
                } else {
                    "ok"
                },
                format!(
                    "{} MiB available of {}",
                    n("memory_available_kb") / 1024,
                    n("memory_total_kb") / 1024
                ),
            ));
        }
        Err(super::control::Unreachable::NoSocket) => {
            out.push(check("runtime", "warn", super::control::OLDER_RUNTIME));
        }
        Err(super::control::Unreachable::Other(e)) => {
            out.push(check(
                "runtime",
                "warn",
                format!("the supervisor did not answer: {e:#}"),
            ));
        }
    }

    let Some(kube) = kubeconfig_path()
        .ok()
        .and_then(|p| KubeClient::from_kubeconfig(&p).ok())
    else {
        out.push(check(
            "cluster",
            "warn",
            "no kubeconfig for the local cluster",
        ));
        return out;
    };
    let deploy = match kube
        .get("/apis/apps/v1/namespaces/hanzo/deployments/cloud")
        .await
    {
        Ok(Some(d)) => d,
        Ok(None) => {
            out.push(check(
                "cloud",
                "info",
                "not deployed (`hanzo up`, or `hanzo up --local` for this machine's source)",
            ));
            return out;
        }
        Err(e) => {
            out.push(check(
                "cluster",
                "fail",
                format!("the local API server did not answer: {e:#}"),
            ));
            return out;
        }
    };
    let s = |ptr: &str| {
        deploy
            .pointer(ptr)
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string()
    };
    let n = |ptr: &str| deploy.pointer(ptr).and_then(Value::as_i64).unwrap_or(0);
    out.push(provenance(&deploy));

    // Does the pod fit the node, twice?
    let request = s("/spec/template/spec/containers/0/resources/requests/memory");
    let request_mb = request
        .strip_suffix("Mi")
        .and_then(|v| v.parse::<u64>().ok())
        .or_else(|| {
            request
                .strip_suffix("Gi")
                .and_then(|v| v.parse::<u64>().ok())
                .map(|g| g * 1024)
        });
    match (request_mb, shape) {
        (Some(req), Some((mem, _))) if budget::SYSTEM_MB + 2 * req > mem => out.push(check(
            "memory request",
            "fail",
            format!(
                "Cloud requests {req} MiB on a {mem} MiB runtime: two do not fit beside the node's {} \
                 ({} needed), so a replaced pod's successor is refused with `Insufficient memory` — \
                 run `hanzo up` again with this hanzo to apply the budget",
                budget::SYSTEM_MB,
                budget::SYSTEM_MB + 2 * req
            ),
        )),
        (Some(req), _) => out.push(check("memory request", "ok", format!("Cloud requests {req} MiB"))),
        (None, _) => out.push(check("memory request", "warn", format!("Cloud's request reads `{request}`"))),
    }

    // Does its data outlive it?
    let volumes = deploy
        .pointer("/spec/template/spec/volumes")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let data = volumes
        .iter()
        .find(|v| v.get("name").and_then(Value::as_str) == Some("data"));
    match data {
        Some(v) if v.get("persistentVolumeClaim").is_some() => out.push(check(
            "data",
            "ok",
            "on a volume claim: it survives `hanzo down` and a replaced pod (a stopped runtime's disk is not kept)",
        )),
        _ => out.push(check(
            "data",
            "warn",
            "in the pod's own scratch space: every `hanzo down` and every new pod starts empty — run `hanzo up` again with this hanzo",
        )),
    }

    // May it ask what it asks?
    let account = match s("/spec/template/spec/serviceAccountName") {
        a if a.is_empty() => "default".to_string(),
        a => a,
    };
    let user = format!("system:serviceaccount:hanzo:{account}");
    let mut refused = Vec::new();
    for (verb, group, resource, ns) in GRANTS {
        match kube.allowed(&user, verb, group, resource, ns).await {
            Ok(true) => {}
            Ok(false) => refused.push(format!(
                "{verb} {resource} {}",
                if ns.is_empty() {
                    "cluster-wide".to_string()
                } else {
                    format!("in {ns}")
                }
            )),
            Err(e) => refused.push(format!("{verb} {resource}: not answered ({e:#})")),
        }
    }
    if refused.is_empty() && account == ACCOUNT {
        out.push(check(
            "kubernetes access",
            "ok",
            format!("runs as {account}; every request Cloud makes is granted, and no more"),
        ));
    } else if refused.is_empty() {
        out.push(check(
            "kubernetes access",
            "warn",
            format!("runs as {account}, not its own account `{ACCOUNT}`"),
        ));
    } else {
        out.push(check(
            "kubernetes access",
            "fail",
            format!(
                "runs as {account}, which is refused: {} — run `hanzo up` again with this hanzo",
                refused.join("; ")
            ),
        ));
    }

    // Is it up, and did the scheduler or the bus say otherwise?
    let ready = n("/status/availableReplicas") >= 1 && n("/status/updatedReplicas") >= 1;
    if n("/spec/replicas") == 0 {
        out.push(check(
            "cloud",
            "info",
            "stopped (`hanzo up` starts it on the image it names)",
        ));
        return out;
    }
    if !ready {
        out.push(check(
            "cloud",
            "fail",
            kube.pod_trouble("hanzo", "app=cloud").await,
        ));
    } else {
        out.push(check("cloud", "ok", "the pod is ready"));
    }
    let store = deploy
        .pointer("/spec/template/spec/containers/0/env")
        .and_then(Value::as_array)
        .and_then(|env| {
            env.iter()
                .find(|e| e.get("name").and_then(Value::as_str) == Some("CLOUD_PUBSUB_MAX_STORE"))
        })
        .and_then(|e| e.get("value").and_then(Value::as_str))
        .and_then(|v| v.parse::<u64>().ok());
    let log = kube
        .pod_logs("hanzo", "app=cloud", 2000)
        .await
        .unwrap_or_default();
    let refused = log.matches("err_code=10047").count()
        + log.matches("insufficient storage resources").count() / 2;
    let denied = log.matches(" is forbidden: User ").count();
    match (refused, store) {
        (0, Some(b)) => out.push(check("message bus", "ok", format!("its file store is declared at {} MiB; no stream was refused", b >> 20))),
        (0, None) => out.push(check("message bus", "info", "no store budget is declared (it sizes itself to the node's free disk); no stream was refused")),
        (n, _) => out.push(check(
            "message bus",
            "fail",
            format!("{n} stream declarations refused in the last 2000 log lines (JetStream 10047, insufficient storage): the streams ask for more than the store holds — run `hanzo up` again with this hanzo, on a Cloud that sizes its streams to the store"),
        )),
    }
    if denied > 0 {
        out.push(check(
            "kubernetes requests",
            "fail",
            format!("{denied} requests refused by the API server in the last 2000 log lines"),
        ));
    }
    if let Ok(resp) = reqwest::Client::builder()
        .timeout(Duration::from_secs(3))
        .build()
        .unwrap_or_default()
        .get(format!("http://127.0.0.1:{LOCAL_PORT}/healthz"))
        .send()
        .await
    {
        let version = resp
            .headers()
            .get("x-api-version")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();
        let body: Value = resp.json().await.unwrap_or(Value::Null);
        let absent: Vec<String> = body
            .get("absent")
            .and_then(Value::as_object)
            .map(|m| m.keys().cloned().collect())
            .unwrap_or_default();
        out.push(check(
            "api",
            if absent.is_empty() { "ok" } else { "info" },
            format!(
                "http://127.0.0.1:{LOCAL_PORT} answers{}{}",
                if version.is_empty() {
                    String::new()
                } else {
                    format!(" as {version}")
                },
                if absent.is_empty() {
                    String::new()
                } else {
                    format!("; not started: {}", absent.join(", "))
                },
            ),
        ));
    } else {
        out.push(check(
            "api",
            "warn",
            format!("http://127.0.0.1:{LOCAL_PORT} did not answer"),
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn deployment(image: &str, policy: &str) -> Value {
        json!({"spec": {"template": {"spec": {"containers": [{"image": image, "imagePullPolicy": policy}]}}}})
    }

    #[test]
    fn provenance_tells_a_local_build_from_a_pulled_image() {
        let local = provenance(&deployment(
            "hanzo.local/hanzo-inc/cloud:0123456789abcdef",
            "Never",
        ));
        assert_eq!(local.status, "ok");
        assert!(
            local.detail.contains("built on this machine"),
            "{}",
            local.detail
        );

        // The one configuration that could turn a missing local image into a pull.
        let wrong = provenance(&deployment(
            "hanzo.local/hanzo-inc/cloud:0123456789abcdef",
            "IfNotPresent",
        ));
        assert_eq!(wrong.status, "fail");

        let pulled = provenance(&deployment(
            "oci.hanzo.ai/hanzoai/cloud-oss@sha256:aa",
            "IfNotPresent",
        ));
        assert_eq!(pulled.status, "info");
        assert!(
            pulled.detail.contains("pulled from its registry"),
            "{}",
            pulled.detail
        );
    }

    #[test]
    fn the_disk_is_judged_by_the_kubelets_thresholds() {
        let gib = 1u64 << 30;
        assert_eq!(disk_verdict(32 * gib, 20 * gib).0, "ok");
        assert_eq!(disk_verdict(32 * gib, 4 * gib).0, "warn");
        assert_eq!(disk_verdict(32 * gib, gib).0, "fail");
        assert_eq!(disk_verdict(0, 0).0, "info");
    }

    #[test]
    fn an_eight_gigabyte_machine_keeps_room_for_itself() {
        // The default runtime on an 8 GiB laptop leaves the host 4 GiB.
        assert_eq!(host_share(8192, 4096).0, "ok");
        // A runtime sized for a workstation does not.
        assert_eq!(host_share(8192, 6144).0, "warn");
    }

    /// The grants doctor checks are the grants the ground makes: one list read two
    /// ways would be two lists the day one of them changes.
    #[test]
    fn every_request_doctor_checks_is_one_the_ground_grants() {
        let ground = crate::commands::up::manifest::ground(8192);
        for (verb, _, resource, ns) in GRANTS {
            assert!(
                ground.contains(&format!("\\\"{verb}\\\"").replace('\\', "")),
                "{verb} is not granted"
            );
            assert!(
                ground.contains(&format!("resources: [\"{resource}\"]")),
                "{resource} is not granted"
            );
            if !ns.is_empty() {
                assert!(
                    ground.contains(&format!("namespace: {ns}")),
                    "nothing is granted in {ns}"
                );
            }
        }
    }
}
