//! `hanzo up` — a local Kubernetes running the cloud, in a Hanzo microVM.
//!
//! Bare `hanzo up` boots k3s inside a `hanzo-vm` microVM, deploys the Hanzo
//! cloud into it as the cluster's first workload, and hands back a kubeconfig.
//! The VM lives exactly as long as its supervisor — a daemonized re-exec of
//! this binary (`up supervise`, hidden) that holds `hanzo-vm run --stdio` as a
//! child and speaks its JSON-RPC over that stdio: write the workload where k3s
//! will find it, exec k3s, poll the node Ready, read the kubeconfig out of the
//! guest. The vm's stdin is the supervisor's leash — the supervisor dying
//! closes it, the guest sees EOF and stops — so `up down` is one SIGTERM.
//!
//! First boot creates a `k3s` disk checkpoint (downloads the binary once, in
//! the foreground so the download is visible); every later boot starts from it.
//!
//! Every boot is measured. The vm reports what it launched — kernel, command
//! line, root image, shape — as an extend-only SHA-384 register; this
//! supervisor extends a second register with what it then deploys, asks the
//! guest what its platform will sign for the pair, and files all three in
//! `~/.hanzo/up/measure.json`. `hanzo up --attest` prints that. The two halves
//! are separate because they are decided by different programs at different
//! times, and because on confidential hardware they map onto separate runtime
//! registers.
//!
//! What ran here before — the local cloud API — is `hanzo host serve` now;
//! `hanzo up <service>` forwards there for one release.

use anyhow::{anyhow, bail, Context, Result};
use base64::Engine;
use colored::*;
use rand::RngCore;
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::time::{Duration, Instant};
use vm_measure::{attest, Log, Measurement};

use crate::commands::{host, net, vm};
use crate::config::Config;
use crate::image;

/// The VM's shape and what it runs — one value through every layer, so the
/// supervisor boots exactly what the caller asked for.
pub struct Boot {
    pub cpus: u32,
    pub memory_mb: u64,
    pub disk_mb: u64,
    /// The cloud image as `registry/repository@sha256:…`. Resolved in the
    /// foreground and handed to the supervisor already pinned, so the digest
    /// the manifest names, the bytes containerd verifies and the register the
    /// measurement extends are one thing decided once.
    pub cloud: String,
}

/// The k3s API port, forwarded host→guest one-to-one.
const K3S_PORT: u16 = 6443;
/// The cloud's own port, in the pod and on the Service; the node port the
/// Service is published at inside the guest; and the host port the forward
/// lands on. The last is 3690 because that is already what `local` means —
/// `hanzo network`'s built-in local network names `http://localhost:3690` as
/// its API, so a cluster published there is reachable by every other command
/// with no second number to remember. It also stays out of the way of 8080,
/// which on a developer's machine is usually somebody else's.
const CLOUD_PORT: u16 = 8080;
const CLOUD_NODE_PORT: u16 = 30080;
const LOCAL_PORT: u16 = 3690;
/// The image deployed when the caller names none.
///
/// `main` and not `latest` because it is the tag that publishes an index with
/// both architectures; `latest` is a lone linux/amd64 manifest, which no arm64
/// guest can run. Either way the tag is resolved to a digest before it reaches
/// the cluster, so what floats here is only which bytes a fresh `up` picks.
pub const CLOUD: &str = "ghcr.io/hanzoai/cloud:main";
/// The two files put where k3s applies anything it finds at startup: the
/// cluster's own ground, and the workload. Separate files because the workload
/// is measured and a per-boot key has no business in a measurement, and named
/// so the ground sorts first — k3s applies them in order, and the namespace
/// and secret have to exist before what needs them.
const GROUND: &str = "/var/lib/rancher/k3s/server/manifests/cloud-key.yaml";
const WORKLOAD: &str = "/var/lib/rancher/k3s/server/manifests/cloud.yaml";
/// The disk checkpoint every boot starts from.
const CHECKPOINT: &str = "k3s";
/// How long the guest gets to report a Ready node.
const READY_TIMEOUT: Duration = Duration::from_secs(180);
/// How long the foreground waits on the supervisor to reach `ready`.
const UP_TIMEOUT: Duration = Duration::from_secs(300);

// ---- state on disk -----------------------------------------------------------

/// `~/.hanzo/up` — supervisor pid, state and log.
fn up_dir() -> Result<PathBuf> {
    let d = dirs::home_dir()
        .ok_or_else(|| anyhow!("no home directory"))?
        .join(".hanzo")
        .join("up");
    std::fs::create_dir_all(&d).with_context(|| format!("creating {}", d.display()))?;
    Ok(d)
}

fn kubeconfig_path() -> Result<PathBuf> {
    Ok(dirs::home_dir()
        .ok_or_else(|| anyhow!("no home directory"))?
        .join(".kube")
        .join("hanzo.yaml"))
}

/// Where the running cluster's measurement is filed, for `--attest` and for
/// anything else that wants to know what this machine is.
fn measure_path(dir: &Path) -> PathBuf {
    dir.join("measure.json")
}

/// The two processes a running cluster is: the supervisor, and the vm it
/// holds. Each records its pid under its own name, and the pair is the whole
/// reason `down` can finish a job a crash left half-done.
const SUPERVISOR: &str = "supervisor";
const VM: &str = "vm";

fn write_pid(dir: &Path, who: &str, pid: u32) -> Result<()> {
    let f = dir.join(format!("{who}.pid"));
    std::fs::write(&f, pid.to_string()).with_context(|| format!("writing {}", f.display()))
}

fn read_pid(dir: &Path, who: &str) -> Option<i32> {
    std::fs::read_to_string(dir.join(format!("{who}.pid")))
        .ok()?
        .trim()
        .parse()
        .ok()
}

fn clear_pid(dir: &Path, who: &str) {
    let _ = std::fs::remove_file(dir.join(format!("{who}.pid")));
}

/// A recorded pid, if it is still that process. Returns `None` for both "never
/// recorded" and "recorded and gone", which are the same thing to every caller.
fn running(dir: &Path, who: &str) -> Option<i32> {
    read_pid(dir, who).filter(|p| alive(*p))
}

/// Signal 0 — the standard liveness test, and the only way to tell a live
/// supervisor from a stale pidfile.
#[cfg(unix)]
fn alive(pid: i32) -> bool {
    unsafe { libc::kill(pid, 0) == 0 }
}

#[cfg(not(unix))]
fn alive(_pid: i32) -> bool {
    false
}

/// Signal a recorded process and everything it started. Both processes we
/// record are process-group leaders, so `-pid` reaches the group; the direct
/// signal follows in case it is not one. `ESRCH` from either is the answer
/// "already gone", which is the outcome we wanted.
///
/// The group is what makes this complete on x86-64, where the vm runs
/// cloud-hypervisor as a child of its own: signalling only `hanzo-vm` would
/// leave the hypervisor holding the guest's memory and its ports.
#[cfg(unix)]
fn signal(pid: i32, sig: i32) {
    unsafe {
        libc::kill(-pid, sig);
        libc::kill(pid, sig);
    }
}

/// Ask `pid` to stop, and make sure it did: SIGTERM, then SIGKILL if it is
/// still there after `grace`. Answers whether the process is gone.
///
/// Signalling only pids WE recorded is the whole discipline here. Scanning the
/// process table for anything whose command line looks like a vm would also
/// find a colleague's on a shared machine, and killing that is not a repair.
#[cfg(unix)]
fn stop(pid: i32, grace: Duration) -> bool {
    for (sig, patience) in [(libc::SIGTERM, grace), (libc::SIGKILL, Duration::from_secs(2))] {
        signal(pid, sig);
        let deadline = Instant::now() + patience;
        while alive(pid) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(100));
        }
        if !alive(pid) {
            return true;
        }
    }
    false
}

#[cfg(not(unix))]
fn stop(_pid: i32, _grace: Duration) -> bool {
    false
}

/// The supervisor's phase, written where the foreground (and `status`) can read
/// it: `boot` → `k3s` → `ready` → `down (…)`, or `error: …`. Best-effort — a
/// phase we cannot record is not a reason to stop booting.
fn write_state(dir: &Path, s: &str) {
    let _ = std::fs::write(dir.join("state"), s);
}

fn read_state(dir: &Path) -> Option<String> {
    std::fs::read_to_string(dir.join("state")).ok().map(|s| s.trim().to_string())
}

// ---- what the vm is asked to do ----------------------------------------------

/// The argv `hanzo-vm` boots the k3s VM with. `--stdio` is the supervisor's
/// wire; the port forward is what makes 127.0.0.1:6443 the API on the host.
fn run_args(boot: &Boot) -> Vec<String> {
    [
        "run",
        "--stdio",
        "--allow-net",
        "--cpus",
        &boot.cpus.to_string(),
        "--memory",
        &boot.memory_mb.to_string(),
        "--disk-size",
        &boot.disk_mb.to_string(),
        "-p",
        &format!("{K3S_PORT}:{K3S_PORT}"),
        "-p",
        &format!("{LOCAL_PORT}:{CLOUD_NODE_PORT}"),
        "--from",
        CHECKPOINT,
    ]
    .iter()
    .map(|s| s.to_string())
    .collect()
}

/// The cluster's ground: the namespace the workload lands in, and the at-rest
/// key the cloud opens its stores with.
///
/// Deliberately NOT measured, and deliberately its own file. The key is 32
/// fresh bytes per cluster — per-instance state, not software identity — and
/// folding it into the workload register would make every boot of the same
/// image measure differently, which is the opposite of what a measurement is
/// for. It sorts before the workload file, so k3s creates the namespace and
/// the secret before it applies what needs them.
fn ground(key: &str) -> String {
    format!(
        "apiVersion: v1
kind: Namespace
metadata:
  name: hanzo
---
apiVersion: v1
kind: Secret
metadata:
  name: cloud
  namespace: hanzo
type: Opaque
stringData:
  master: {key}
"
    )
}

/// The workload: one cloud, its API published on a node port the host forwards.
///
/// The image is pinned to a digest before this is written, so the manifest
/// says exactly which bytes containerd must verify. The environment is the
/// production Deployment's, minus everything that names a cluster this is not:
///
/// `ZIP_RUNTIME_DIR` puts the plugin sockets inside the writable data volume —
/// a distroless image has no writable `/run`, and the host exits when it
/// cannot bind them. `CLOUD_MEMORY_REQUEST_MIB` is projected from the pod's
/// own request through the downward API, exactly as production does it,
/// because the host sizes how many plugin children it holds from that number
/// and a value it cannot read means it assumes the 6 GiB reservation it was
/// tuned against. `CLOUD_HEALTH_LISTEN` opens the ops port on the pod address:
/// its default binds loopback, where no probe can reach it.
///
/// The security context is production's, and `fsGroup` is the load-bearing
/// line: the image runs as 65532, an `emptyDir` arrives owned by root, and the
/// data root is the first thing the process opens. Without it the cloud starts,
/// cannot write, and sits there — no port, no second log line, 18 microcores.
/// `readOnlyRootFilesystem` then costs a `/tmp` volume, which is why one is
/// mounted.
fn workload(image: &str) -> String {
    format!(
        "apiVersion: apps/v1
kind: Deployment
metadata:
  name: cloud
  namespace: hanzo
spec:
  replicas: 1
  strategy:
    type: Recreate
  selector:
    matchLabels:
      app: cloud
  template:
    metadata:
      labels:
        app: cloud
    spec:
      securityContext:
        runAsNonRoot: true
        runAsUser: 65532
        runAsGroup: 65532
        fsGroup: 65532
      containers:
        - name: cloud
          image: {image}
          securityContext:
            allowPrivilegeEscalation: false
            readOnlyRootFilesystem: true
            capabilities:
              drop:
                - ALL
          ports:
            - name: api
              containerPort: {CLOUD_PORT}
          env:
            - name: CLOUD_HEALTH_LISTEN
              value: \":9090\"
            - name: ZIP_RUNTIME_DIR
              value: /var/lib/cloud/run
            - name: CLOUD_KMS_MASTER_KEY_REF
              valueFrom:
                secretKeyRef:
                  name: cloud
                  key: master
            - name: CLOUD_MEMORY_REQUEST_MIB
              valueFrom:
                resourceFieldRef:
                  containerName: cloud
                  resource: requests.memory
                  divisor: 1Mi
          resources:
            requests:
              memory: 1Gi
          readinessProbe:
            httpGet:
              path: /readyz
              port: 9090
            periodSeconds: 5
            failureThreshold: 60
          livenessProbe:
            httpGet:
              path: /healthz
              port: 9090
            periodSeconds: 10
            failureThreshold: 30
          volumeMounts:
            - name: data
              mountPath: /var/lib/cloud
            - name: tmp
              mountPath: /tmp
      volumes:
        - name: data
          emptyDir: {{}}
        - name: tmp
          emptyDir: {{}}
---
apiVersion: v1
kind: Service
metadata:
  name: cloud
  namespace: hanzo
spec:
  type: NodePort
  selector:
    app: cloud
  ports:
    - name: api
      port: {CLOUD_PORT}
      targetPort: {CLOUD_PORT}
      nodePort: {CLOUD_NODE_PORT}
"
    )
}

/// 32 fresh bytes, base64 — the shape the cloud reads its at-rest key in. From
/// the OS generator, never a derivation: a key a second machine could guess is
/// not one.
fn key() -> String {
    let mut bytes = [0u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

// ---- the measurement ----------------------------------------------------------

/// The workload half of the measurement: what this cluster was asked to run.
///
/// Two events, and the order is the order they decide things in. `image` is
/// the digest containerd must verify before a byte of the cloud executes.
/// `manifest` is the exact text k3s applies — a second cluster with the same
/// image but a different Deployment is a different cluster, and the register
/// says so. The at-rest key is NOT here: it is 32 fresh bytes per cluster, and
/// per-instance state in a register would make identical software measure
/// differently every boot, which is the opposite of what a register is for.
fn deployed(image: &str, manifest: &str) -> Log {
    let mut log = Log::default();
    log.text("image", image);
    log.bytes("manifest", WORKLOAD, manifest.as_bytes());
    log
}

/// File what this cluster is: the launch the vm reported, the workload we
/// deployed, and whatever the guest's platform would sign for the pair.
///
/// The report is asked for LAST because it is taken over the bind, which
/// covers both registers — anything extended afterwards would be outside what
/// the platform signed.
fn record(dir: &Path, rpc: &mut Rpc, launch: Log, image: &str, manifest: &str) -> Result<()> {
    let mut m = Measurement {
        launch,
        workload: deployed(image, manifest),
        ..Measurement::default()
    };
    m.hardware = rpc.attest(&m.bind_hex())?;
    let path = measure_path(dir);
    std::fs::write(&path, m.to_json_pretty()).with_context(|| format!("writing {}", path.display()))
}

/// `hanzo up --attest` — what the running cluster is, as its own document.
pub fn attest() -> Result<()> {
    println!("{}", measured(&up_dir()?)?.to_json_pretty());
    Ok(())
}

/// The filed measurement, read back through [`Measurement::from_json`], which
/// folds both registers and the bind again: a file edited on disk is refused
/// rather than repeated.
fn measured(dir: &Path) -> Result<Measurement> {
    let path = measure_path(dir);
    let text = std::fs::read_to_string(&path)
        .with_context(|| format!("reading {} — is a cluster up?", path.display()))?;
    Measurement::from_json(&text).with_context(|| format!("in {}", path.display()))
}

/// Which k3s release asset this host's architecture boots. The guest runs the
/// host's architecture — the vm does not emulate.
fn k3s_asset() -> &'static str {
    if cfg!(target_arch = "aarch64") {
        "k3s-arm64"
    } else {
        "k3s"
    }
}

fn install_cmd() -> String {
    format!(
        "curl -Lo /usr/local/bin/k3s \
         https://github.com/k3s-io/k3s/releases/latest/download/{} \
         && chmod +x /usr/local/bin/k3s",
        k3s_asset()
    )
}

/// The one-time checkpoint: download k3s into the base image, save the disk.
fn checkpoint_args() -> Vec<String> {
    [
        "checkpoint",
        "create",
        CHECKPOINT,
        "--allow-net",
        "--",
        "sh",
        "-c",
        &install_cmd(),
    ]
    .iter()
    .map(|s| s.to_string())
    .collect()
}

/// Whether `hanzo-vm checkpoint list` names ours (the first column of a row).
fn has_checkpoint(listing: &str, name: &str) -> bool {
    listing
        .lines()
        .filter_map(|l| l.split_whitespace().next())
        .any(|first| first == name)
}

/// Whether `k3s kubectl get nodes --no-headers` reports a Ready node. The
/// status column is a comma-joined condition list, so `Ready` is matched as a
/// member, never as a substring — `NotReady` must not read as ready.
fn node_ready(out: &str) -> bool {
    out.lines().any(|l| {
        l.split_whitespace()
            .nth(1)
            .is_some_and(|status| status.split(',').any(|c| c == "Ready"))
    })
}

/// Point the guest's kubeconfig at the forwarded port. The guest writes its own
/// idea of an address; the forward is OURS, so every `server:` line is mapped
/// to it explicitly.
fn rewrite_server(yaml: &str) -> String {
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

/// Write the kubeconfig owner-only (0600): it carries the cluster's keys.
fn write_kubeconfig(path: &Path, yaml: &str) -> Result<()> {
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

// ---- the vm's stdio JSON-RPC, spoken from the supervisor ----------------------

/// The `hanzo-vm --stdio` peer: JSON-lines JSON-RPC 2.0 on the child's
/// stdin/stdout (`vm-cli/src/stdio.rs`). Spoken BLOCKING — the supervisor is a
/// dedicated process with nothing else to do.
struct Rpc {
    child: Child,
    /// Closing this is the designed stop: the guest sees EOF and shuts down.
    /// An `Option` so [`Drop`] can close it while the child is still held.
    stdin: Option<ChildStdin>,
    out: BufReader<ChildStdout>,
    next: u64,
}

impl Rpc {
    /// Spawn the vm with its stderr appended to `log`; stdout is the protocol.
    fn start(bin: &Path, args: &[String], log: &Path) -> Result<Rpc> {
        let stderr = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(log)
            .with_context(|| format!("opening {}", log.display()))?;
        let mut cmd = Command::new(bin);
        cmd.args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::from(stderr));
        // Its own process group, so a later `stop` reaches whatever the vm
        // started — on x86-64 that is a cloud-hypervisor process holding the
        // guest's memory and its ports.
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            cmd.process_group(0);
        }
        let mut child = cmd
            .spawn()
            .with_context(|| format!("starting {}", bin.display()))?;
        let stdin = child.stdin.take().expect("piped stdin");
        let out = BufReader::new(child.stdout.take().expect("piped stdout"));
        Ok(Rpc { child, stdin: Some(stdin), out, next: 0 })
    }

    /// One protocol line. EOF is the vm being gone — its own last words are in
    /// the log, so say where to look rather than guessing why.
    fn read_line(&mut self) -> Result<Value> {
        let mut line = String::new();
        loop {
            line.clear();
            if self.out.read_line(&mut line)? == 0 {
                bail!("hanzo-vm exited mid-conversation — see the supervisor log");
            }
            let t = line.trim();
            if t.is_empty() {
                continue;
            }
            if let Ok(v) = serde_json::from_str::<Value>(t) {
                return Ok(v);
            }
        }
    }

    /// Block until the guest is up, and come back with what was booted.
    ///
    /// The vm sends its launch measurement before it says `ready` — that
    /// ordering is the point: the register is over the images the hypervisor
    /// was handed, taken before the guest could touch anything. A vm that
    /// reports no measurement is refused rather than run unmeasured.
    fn wait_ready(&mut self) -> Result<Log> {
        let mut launch = None;
        loop {
            let line = self.read_line()?;
            match line.get("method").and_then(Value::as_str) {
                Some("measurement") => {
                    let params = line
                        .get("params")
                        .ok_or_else(|| anyhow!("the vm sent a measurement with no log"))?;
                    launch = Some(
                        serde_json::from_value(params.clone())
                            .context("reading the vm's launch measurement")?,
                    );
                }
                Some("ready") => {
                    return launch.ok_or_else(|| {
                        anyhow!("this hanzo-vm booted without reporting a measurement")
                    })
                }
                _ => {}
            }
        }
    }

    /// One call: request out, notifications skipped, this id's result back. An
    /// `error` member is our error, never a silent null.
    fn call(&mut self, method: &str, params: Value) -> Result<Value> {
        self.next += 1;
        let id = self.next;
        let req = json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params });
        let stdin = self
            .stdin
            .as_mut()
            .ok_or_else(|| anyhow!("the vm's stdin is closed"))?;
        writeln!(stdin, "{req}").context("writing to hanzo-vm")?;
        stdin.flush().context("flushing to hanzo-vm")?;
        loop {
            let v = self.read_line()?;
            if v.get("id").and_then(Value::as_u64) != Some(id) {
                continue; // a notification (spawned k3s narrating), or nothing of ours
            }
            if let Some(e) = v.get("error") {
                bail!("{method}: {e}");
            }
            return Ok(v.get("result").cloned().unwrap_or(Value::Null));
        }
    }

    /// Run to completion in the guest: (stdout, stderr, exit code).
    fn exec(&mut self, argv: &[&str]) -> Result<(String, String, i64)> {
        let r = self.call("exec", json!({ "argv": argv }))?;
        let s = |k: &str| r.get(k).and_then(Value::as_str).unwrap_or_default().to_string();
        let code = r.get("exit_code").and_then(Value::as_i64).unwrap_or(-1);
        Ok((s("stdout"), s("stderr"), code))
    }

    /// Start a long-lived guest process; its output arrives as notifications,
    /// which [`Rpc::call`] skips past.
    fn spawn(&mut self, argv: &[&str]) -> Result<String> {
        let r = self.call("spawn", json!({ "argv": argv }))?;
        r.get("pid")
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| anyhow!("spawn answered without a pid: {r}"))
    }

    /// Write a guest file (the wire carries it base64), creating its directory.
    fn write_file(&mut self, path: &str, content: &str) -> Result<()> {
        let dir = path.rsplit_once('/').map(|(d, _)| d).unwrap_or("/");
        self.call("mkdir", json!({ "path": dir, "recursive": true }))?;
        let content = base64::engine::general_purpose::STANDARD.encode(content);
        self.call("write_file", json!({ "path": path, "content": content }))?;
        Ok(())
    }

    /// Ask the guest's platform for a report over `bind`. On hardware with
    /// neither `/dev/sev-guest` nor `/dev/tdx_guest` the answer is `none`,
    /// which the document records as the fact it is.
    fn attest(&mut self, bind: &str) -> Result<attest::Status> {
        let r = self.call("attest", json!({ "bind": bind }))?;
        serde_json::from_value(r).context("reading the guest's attestation status")
    }

    /// Read a guest file (the wire carries it base64).
    fn read_file(&mut self, path: &str) -> Result<Vec<u8>> {
        let r = self.call("read_file", json!({ "path": path }))?;
        let content = r
            .get("content")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("read_file answered without content: {r}"))?;
        base64::engine::general_purpose::STANDARD
            .decode(content)
            .context("decode read_file content")
    }

    fn exited(&mut self) -> Option<std::process::ExitStatus> {
        self.child.try_wait().ok().flatten()
    }

    /// Whether the child is finished within `patience`, reaping it if so. A
    /// child we killed is a zombie until we collect it, and `kill(pid, 0)`
    /// calls a zombie alive — so liveness for our OWN child is `try_wait`,
    /// never a signal.
    fn gone(&mut self, patience: Duration) -> bool {
        let deadline = Instant::now() + patience;
        loop {
            if self.exited().is_some() {
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

/// The vm does not outlive the conversation. Closing its stdin is the designed
/// stop and drop does that by itself — but only `hanzo-vm` is listening, and on
/// x86-64 it holds a cloud-hypervisor of its own. So the group is signalled and
/// the child is reaped here, where no error path can skip it.
///
/// Reaping through [`Child::wait`] rather than by polling for liveness is the
/// distinction that matters for a process we own: a killed child is a zombie
/// until its parent collects it, and `kill(pid, 0)` says a zombie is alive. The
/// parent is us.
impl Drop for Rpc {
    fn drop(&mut self) {
        // The leash, dropped: the guest sees EOF and shuts its disk down
        // cleanly. Everything below is for a vm that does not take the hint.
        self.stdin.take();
        if self.gone(Duration::from_secs(2)) {
            return;
        }
        #[cfg(unix)]
        {
            let pid = self.child.id() as i32;
            signal(pid, libc::SIGTERM);
            if self.gone(Duration::from_secs(3)) {
                return;
            }
            signal(pid, libc::SIGKILL);
        }
        let _ = self.child.wait();
    }
}

// ---- the supervisor -----------------------------------------------------------

/// `hanzo up supervise` (hidden): the daemon `hanzo up` leaves behind. Owns the
/// vm for its whole life; this process dying — `up down`, a crash, a logout —
/// closes the vm's stdin, and EOF is how the guest stops.
pub async fn supervise(boot: Boot) -> Result<()> {
    let dir = up_dir()?;
    write_pid(&dir, SUPERVISOR, std::process::id())?;
    // The same one resolver `hanzo up` used — by now the binary exists, but a
    // supervisor started by hand on a bare box bootstraps identically.
    let bin = vm::resolve_or_install().await?;
    let out = drive(&dir, &boot, &bin);
    if let Err(e) = &out {
        write_state(&dir, &format!("error: {e:#}"));
    }
    // `drive` has returned, so its `Rpc` is dropped and the vm with it, by any
    // path including the failing ones. A supervisor that was KILLED never gets
    // here — that is the case `down` collects from the recorded pid.
    clear_pid(&dir, VM);
    clear_pid(&dir, SUPERVISOR);
    out
}

/// Boot → workload → k3s → Ready → kubeconfig → hold. Every phase lands in the
/// state file so the foreground (and `up status`) reads facts, not hope.
fn drive(dir: &Path, boot: &Boot, bin: &Path) -> Result<()> {
    write_state(dir, "boot");
    let log = dir.join("supervisor.log");
    let mut rpc = Rpc::start(bin, &run_args(boot), &log)?;
    // Recorded before the first word of protocol: a supervisor killed between
    // the spawn and `ready` still leaves a pid `down` can collect. Without it
    // the vm outlives everything that knows about it, holding 6443 against the
    // next boot.
    write_pid(dir, VM, rpc.child.id())?;
    let launch = rpc.wait_ready()?;

    // The workload goes in before k3s starts, so the cluster's first act is to
    // apply it — no second tool, no window in which the cluster is up and
    // running nothing. It is written and MEASURED in the same breath: what the
    // register covers is the file the cluster will read.
    let manifest = workload(&boot.cloud);
    rpc.write_file(GROUND, &ground(&key()))?;
    rpc.write_file(WORKLOAD, &manifest)?;
    record(dir, &mut rpc, launch, &boot.cloud, &manifest)?;

    write_state(dir, "k3s");
    rpc.spawn(&["k3s", "server", "--disable", "traefik", "--disable", "metrics-server"])?;
    let deadline = Instant::now() + READY_TIMEOUT;
    loop {
        // A failing poll is k3s not answering YET — unless the vm itself is
        // gone, which no amount of waiting repairs.
        match rpc.exec(&["k3s", "kubectl", "get", "nodes", "--no-headers"]) {
            Ok((out, _, 0)) if node_ready(&out) => break,
            Ok(_) => {}
            Err(e) if rpc.exited().is_some() => return Err(e),
            Err(_) => {}
        }
        if Instant::now() >= deadline {
            bail!("k3s reported no Ready node within {}s", READY_TIMEOUT.as_secs());
        }
        std::thread::sleep(Duration::from_secs(3));
    }

    let yaml = String::from_utf8(rpc.read_file("/etc/rancher/k3s/k3s.yaml")?)
        .context("the guest kubeconfig is not utf-8")?;
    write_kubeconfig(&kubeconfig_path()?, &rewrite_server(&yaml))?;
    write_state(dir, "ready");

    // Hold the vm for as long as we live; its exit ends the watch either way.
    let status = rpc.child.wait().context("waiting on hanzo-vm")?;
    write_state(dir, &format!("down ({status})"));
    Ok(())
}

// ---- `hanzo up` and friends ---------------------------------------------------

/// Bare `hanzo up`: pin the image, ensure the checkpoint, leave a supervisor
/// behind, wait for `ready`, print the lines to paste.
pub async fn up(cfg: &mut Config, boot: Boot, link: Option<String>) -> Result<()> {
    let dir = up_dir()?;
    if let Some(pid) = running(&dir, SUPERVISOR) {
        let state = read_state(&dir).unwrap_or_else(|| "unknown".into());
        println!("{} already running (supervisor pid {pid}, {state})", "●".green());
        return endpoints().and(finish_link(cfg, link).await);
    }
    // No supervisor, but a vm we started is still there: its supervisor died
    // without closing anything, and it is holding 6443 against this boot. It is
    // ours and nothing is watching it, so collect it rather than fail on a port
    // conflict whose cause is invisible.
    if let Some(pid) = running(&dir, VM) {
        crate::warn(&format!("collecting an orphaned vm (pid {pid}) from an earlier boot"));
        if !stop(pid, Duration::from_secs(10)) {
            bail!("an earlier vm (pid {pid}) is still running — `hanzo down` first");
        }
        clear_pid(&dir, VM);
    }

    // Pinned HERE, once, in the foreground: the digest reaches the supervisor
    // as an argument, so the cluster and the measurement cannot end up naming
    // different bytes, and a tag that moves mid-boot cannot change what runs.
    let boot = Boot {
        cloud: image::pin(&reqwest::Client::new(), &boot.cloud, image::architecture()).await?,
        ..boot
    };
    let bin = vm::resolve_or_install().await?;
    ensure_checkpoint(&bin)?;

    write_state(&dir, "starting");
    spawn_supervisor(&dir, &boot)?;
    wait_ready_state(&dir)?;
    println!("{} k3s is up — API at https://127.0.0.1:{K3S_PORT}", "✓".green());
    endpoints()?;
    finish_link(cfg, link).await
}

fn endpoints() -> Result<()> {
    println!("  export KUBECONFIG={}", kubeconfig_path()?.display());
    println!("  the cloud             http://127.0.0.1:{LOCAL_PORT}  (hanzo network use local)");
    println!("  what this cluster is  hanzo up --attest");
    Ok(())
}

/// Create the `k3s` checkpoint when the store lacks it — in the FOREGROUND,
/// with inherited stdio, so the one-time k3s download is visible rather than a
/// silent minute.
fn ensure_checkpoint(bin: &Path) -> Result<()> {
    let out = Command::new(bin)
        .args(["checkpoint", "list"])
        .output()
        .with_context(|| format!("running {} checkpoint list", bin.display()))?;
    if has_checkpoint(&String::from_utf8_lossy(&out.stdout), CHECKPOINT) {
        return Ok(());
    }
    println!("{} creating the {CHECKPOINT} checkpoint (downloads k3s once)…", "→".cyan());
    let status = Command::new(bin)
        .args(checkpoint_args())
        .status()
        .with_context(|| format!("running {} checkpoint create", bin.display()))?;
    if !status.success() {
        bail!("checkpoint create failed ({status})");
    }
    Ok(())
}

/// Leave the supervisor behind: our own binary, re-run as the hidden
/// `up supervise`, detached into its own process group with its stdio on the
/// log — so it survives this command and Ctrl-C never reaches it.
fn spawn_supervisor(dir: &Path, boot: &Boot) -> Result<u32> {
    let exe = std::env::current_exe().context("resolving our own binary")?;
    // Emptied here, then APPENDED to. The supervisor and the vm it holds both
    // write to this file from separate processes; an offset of their own would
    // have each overwrite the other from byte zero, and the half that survived
    // was the half that said the least — "exited mid-conversation" landing on
    // top of the bind error that explained it.
    let path = dir.join("supervisor.log");
    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .truncate(false)
        .open(&path)
        .and_then(|f| f.set_len(0).map(|()| f))
        .with_context(|| format!("creating {}", path.display()))?;
    let mut cmd = Command::new(exe);
    cmd.args([
        "up",
        "--cpus",
        &boot.cpus.to_string(),
        "--memory",
        &boot.memory_mb.to_string(),
        "--disk-size",
        &boot.disk_mb.to_string(),
        "--cloud",
        &boot.cloud,
        "supervise",
    ])
    .stdin(Stdio::null())
    .stdout(Stdio::from(log.try_clone().context("duplicating the log handle")?))
    .stderr(Stdio::from(log));
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    let child = cmd.spawn().context("starting the k3s supervisor")?;
    Ok(child.id())
}

/// Watch the state file until the supervisor says `ready` — or says why not.
fn wait_ready_state(dir: &Path) -> Result<()> {
    let log = dir.join("supervisor.log");
    let deadline = Instant::now() + UP_TIMEOUT;
    loop {
        match read_state(dir).as_deref() {
            Some("ready") => return Ok(()),
            Some(s) if s.starts_with("error") || s.starts_with("down") => {
                bail!("{s} — see {}", log.display())
            }
            _ => {}
        }
        if Instant::now() >= deadline {
            bail!("k3s did not come up within {}s — see {}", UP_TIMEOUT.as_secs(), log.display());
        }
        std::thread::sleep(Duration::from_millis(500));
    }
}

/// `--link <cluster>`: mint the cluster's place on the org network — an
/// identity for this host and a service for the API. The guest half (enrolling
/// INSIDE the vm) needs a `zt` binary today's guest image does not carry, so
/// the minting is real and the enrollment is handed over, out loud.
async fn finish_link(cfg: &mut Config, link: Option<String>) -> Result<()> {
    let Some(cluster) = link else { return Ok(()) };
    let host_name = format!("k8s-{cluster}-host");
    let jwt = net::join(cfg, Some(host_name.clone()), vec![host_name.clone()]).await?;
    let dns = net::publish(cfg, format!("k8s-{cluster}"), format!("127.0.0.1:{K3S_PORT}")).await?;
    println!();
    println!("{}", "identity and service are minted; enrollment is manual for now:".bold());
    println!("  enroll this machine   zt edge enroll --jwt {}", jwt.display());
    println!(
        "  host the API          bind {dns} → 127.0.0.1:{K3S_PORT} as {host_name} (zt tunnel host)"
    );
    bail!(
        "not implemented: guest enrollment — the identity and service above exist; \
         finish with the steps printed"
    )
}

/// `hanzo up status` — the supervisor and the node, honestly separated: the
/// pidfile answers for the first, the kubeconfig (via kubectl) for the second.
pub async fn status() -> Result<()> {
    let dir = up_dir()?;
    let Some(pid) = running(&dir, SUPERVISOR) else {
        // A vm with no supervisor is the one state worth naming: it is running
        // and nothing is driving it.
        if let Some(vm) = running(&dir, VM) {
            println!("{} a vm (pid {vm}) is running with no supervisor — `hanzo down`", "●".yellow());
        } else {
            println!("{} not running", "○".dimmed());
        }
        return Ok(());
    };
    let state = read_state(&dir).unwrap_or_else(|| "unknown".into());
    println!("{} supervisor running (pid {pid}, {state})", "●".green());
    println!("  logs {}", dir.join("supervisor.log").display().to_string().dimmed());
    let kc = kubeconfig_path()?;
    if !kc.exists() {
        println!("  no kubeconfig yet ({})", kc.display());
        return Ok(());
    }
    match which::which("kubectl") {
        Ok(kubectl) => {
            let _ = Command::new(kubectl)
                .arg("--kubeconfig")
                .arg(&kc)
                .args(["get", "nodes"])
                .status();
        }
        Err(_) => println!("  kubectl not on PATH — KUBECONFIG={}", kc.display()),
    }
    Ok(())
}

/// `hanzo down` (and `hanzo up down`) — stop the supervisor, then make sure the
/// vm it held is gone.
///
/// The supervisor first: closing the vm's stdin is the designed shutdown, and a
/// guest that stops on the EOF flushes its disk. The vm second, because that
/// path is not the only way this ends. A supervisor that was SIGKILLed, or died
/// with the machine, never closed anything — and the vm it left behind holds
/// 6443 against the next boot while nothing on the system explains why. Its
/// recorded pid is how we collect it, and a pid we recorded is the only thing
/// we will signal.
pub fn down() -> Result<()> {
    let dir = up_dir()?;
    let (supervisor, vm) = (running(&dir, SUPERVISOR), running(&dir, VM));
    if supervisor.is_none() && vm.is_none() {
        clear_pid(&dir, SUPERVISOR);
        clear_pid(&dir, VM);
        println!("{} not running", "○".dimmed());
        return Ok(());
    }

    if let Some(pid) = supervisor {
        if !stop(pid, Duration::from_secs(30)) {
            bail!("supervisor (pid {pid}) did not exit");
        }
        clear_pid(&dir, SUPERVISOR);
    }
    // Re-read: a supervisor that exited cleanly reaped the vm and cleared it.
    if let Some(pid) = running(&dir, VM) {
        if !stop(pid, Duration::from_secs(10)) {
            bail!("the vm (pid {pid}) did not exit");
        }
    }
    clear_pid(&dir, VM);

    write_state(&dir, "down");
    println!("{} down", "✓".green());
    Ok(())
}

/// The old `hanzo up [service]` spelling — split into the service and its tail.
fn service_argv(argv: Vec<String>) -> (String, Vec<String>) {
    let mut it = argv.into_iter();
    let service = it.next().unwrap_or_else(|| "cloud".into());
    let mut rest: Vec<String> = it.collect();
    if rest.first().map(String::as_str) == Some("--") {
        rest.remove(0);
    }
    (service, rest)
}

/// The old `hanzo up <service>` — forwards to `hanzo host serve` for one
/// release, saying so.
pub async fn deprecated_service(argv: Vec<String>) -> Result<()> {
    let (service, rest) = service_argv(argv);
    crate::warn(&format!(
        "`hanzo up {service}` is now `hanzo host serve {service}`; \
         this forwarding goes away next release"
    ));
    host::serve(service, rest).await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn boot() -> Boot {
        Boot {
            cpus: 4,
            memory_mb: 4096,
            disk_mb: 16384,
            cloud: "ghcr.io/hanzoai/cloud@sha256:c10d".into(),
        }
    }

    /// The exact argv the k3s VM boots with — the stdio wire, both forwards,
    /// the checkpoint.
    #[test]
    fn the_vm_is_booted_with_the_stdio_wire_and_the_forwards() {
        assert_eq!(
            run_args(&boot()),
            [
                "run", "--stdio", "--allow-net", "--cpus", "4", "--memory", "4096",
                "--disk-size", "16384", "-p", "6443:6443", "-p", "3690:30080",
                "--from", "k3s",
            ]
        );
    }

    /// The workload names the pinned image, publishes the API on the node port
    /// the host forwards, and carries the three environment values a cluster
    /// this small still has to state: the ops listener on the pod address, the
    /// plugin sockets inside the writable volume, and the memory reservation
    /// the host sizes itself from.
    #[test]
    fn the_workload_deploys_the_pinned_image_on_the_forwarded_port() {
        let y = workload("ghcr.io/hanzoai/cloud@sha256:c10d");
        assert!(y.contains("image: ghcr.io/hanzoai/cloud@sha256:c10d"), "{y}");
        assert!(y.contains("nodePort: 30080"), "{y}");
        assert!(y.contains("containerPort: 8080"), "{y}");
        assert!(y.contains("namespace: hanzo"), "{y}");
        for env in ["CLOUD_HEALTH_LISTEN", "ZIP_RUNTIME_DIR", "CLOUD_MEMORY_REQUEST_MIB"] {
            assert!(y.contains(env), "{env} missing from\n{y}");
        }
        // The at-rest key is referenced, never inlined.
        assert!(y.contains("secretKeyRef"), "{y}");
        assert!(!y.contains("stringData"), "{y}");
    }

    /// The ground carries the namespace and a key that is fresh every time —
    /// two clusters never share one, and no derivation makes it guessable.
    #[test]
    fn the_ground_carries_a_fresh_key() {
        let (a, b) = (key(), key());
        assert_ne!(a, b);
        assert_eq!(
            base64::engine::general_purpose::STANDARD.decode(&a).unwrap().len(),
            32
        );
        let y = ground(&a);
        assert!(y.contains("kind: Namespace"), "{y}");
        assert!(y.contains(&format!("master: {a}")), "{y}");
        // The ground is applied first, so its filename sorts before the workload's.
        assert!(GROUND < WORKLOAD, "{GROUND} must sort before {WORKLOAD}");
    }

    /// The workload register moves with the image AND with the manifest text:
    /// the same cloud deployed differently is a different cluster, and the
    /// number says so.
    #[test]
    fn the_workload_register_covers_the_image_and_the_manifest() {
        let one = deployed("ghcr.io/hanzoai/cloud@sha256:aa", "kind: Deployment");
        let same = deployed("ghcr.io/hanzoai/cloud@sha256:aa", "kind: Deployment");
        assert_eq!(one.register(), same.register(), "same inputs, same register");

        let other_image = deployed("ghcr.io/hanzoai/cloud@sha256:bb", "kind: Deployment");
        assert_ne!(one.register(), other_image.register());

        let other_manifest = deployed("ghcr.io/hanzoai/cloud@sha256:aa", "kind: DaemonSet");
        assert_ne!(one.register(), other_manifest.register());

        // The manifest is measured by content; the path it lands at is recorded
        // for the reader, not hashed.
        assert_eq!(one.events()[1].source, WORKLOAD);
    }

    /// A key never enters the measurement: two clusters differing only in
    /// their at-rest key are the same software, and measure the same.
    #[test]
    fn the_at_rest_key_is_outside_the_measurement() {
        let manifest = workload("ghcr.io/hanzoai/cloud@sha256:aa");
        let register = deployed("ghcr.io/hanzoai/cloud@sha256:aa", &manifest).register();
        for _ in 0..2 {
            let _ = ground(&key());
            assert_eq!(
                deployed("ghcr.io/hanzoai/cloud@sha256:aa", &manifest).register(),
                register
            );
        }
    }

    /// The checkpoint downloads THIS architecture's k3s and marks it runnable.
    #[test]
    fn the_checkpoint_installs_k3s_for_this_architecture() {
        let args = checkpoint_args();
        assert_eq!(&args[..5], ["checkpoint", "create", "k3s", "--allow-net", "--"]);
        let cmd = args.last().unwrap();
        assert!(cmd.contains("k3s-io/k3s/releases/latest/download"), "{cmd}");
        assert!(cmd.contains(k3s_asset()), "{cmd}");
        assert!(cmd.contains("chmod +x /usr/local/bin/k3s"), "{cmd}");
        if cfg!(target_arch = "aarch64") {
            assert_eq!(k3s_asset(), "k3s-arm64");
        } else {
            assert_eq!(k3s_asset(), "k3s");
        }
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

    /// Ready is a MEMBER of the status column, never a substring: `NotReady`
    /// must not count, `Ready,SchedulingDisabled` must.
    #[test]
    fn a_node_is_ready_when_its_status_says_so() {
        assert!(node_ready("k3s-node   Ready    control-plane   30s   v1.30.0\n"));
        assert!(node_ready("n1   Ready,SchedulingDisabled   worker   1m   v1.30.0\n"));
        assert!(!node_ready("k3s-node   NotReady   control-plane   5s   v1.30.0\n"));
        assert!(!node_ready(""));
    }

    /// Every `server:` line is pointed at the forward; nothing else moves.
    #[test]
    fn the_kubeconfig_is_pointed_at_the_forwarded_port() {
        let yaml = "apiVersion: v1\nclusters:\n- cluster:\n    server: https://10.0.2.15:6443\n    certificate-authority-data: AAA\n";
        let out = rewrite_server(yaml);
        assert!(out.contains("    server: https://127.0.0.1:6443\n"), "{out}");
        assert!(!out.contains("10.0.2.15"), "{out}");
        assert!(out.contains("certificate-authority-data: AAA"), "{out}");
        // A comment naming server: elsewhere in the line is not an address.
        assert_eq!(rewrite_server("# the server: line\n"), "# the server: line\n");
    }

    /// The kubeconfig carries the cluster's keys: filed owner-only.
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

    /// Start a process that is NOT our child, and answer with its pid and the
    /// pid of a child of ITS own. That is the shape `stop` meets in production:
    /// `down` signals a supervisor and a vm it did not spawn and cannot reap,
    /// and the vm has a hypervisor under it on x86-64. Modelling them as our
    /// own children would test something else — a killed child is a zombie
    /// until its parent collects it, and `kill(pid, 0)` calls a zombie alive.
    #[cfg(unix)]
    fn orphan(dir: &Path) -> (i32, i32) {
        let leader = dir.join("leader");
        let child = dir.join("child");
        // `set -m` is job control, which puts a background job in a process
        // group of its OWN — the shape `Rpc::start` gives the vm. The shell we
        // spawn exits as soon as it has backgrounded that job, so the leader is
        // reparented to init and is never ours to reap.
        let script = format!(
            "set -m; sh -c 'sleep 60 & echo $! > {c}; echo $$ > {l}; wait' &",
            c = child.display(),
            l = leader.display()
        );
        Command::new("sh")
            .args(["-c", &script])
            .status()
            .expect("sh runs");

        let read = |p: &Path| -> Option<i32> {
            std::fs::read_to_string(p).ok()?.trim().parse().ok().filter(|p| alive(*p))
        };
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let (Some(l), Some(c)) = (read(&leader), read(&child)) {
                return (l, c);
            }
            assert!(Instant::now() < deadline, "the orphan never started");
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    /// The pidfile lifecycle, proven on a real process: recorded under its own
    /// name, seen running, stopped, seen gone, cleared.
    #[cfg(unix)]
    #[test]
    fn the_pidfile_follows_a_real_process() {
        let dir = tempfile::tempdir().unwrap();
        let (pid, _) = orphan(dir.path());
        write_pid(dir.path(), SUPERVISOR, pid as u32).unwrap();

        assert_eq!(read_pid(dir.path(), SUPERVISOR), Some(pid));
        assert_eq!(running(dir.path(), SUPERVISOR), Some(pid));
        // The two names are separate files: a supervisor is not a vm.
        assert_eq!(read_pid(dir.path(), VM), None);

        assert!(stop(pid, Duration::from_secs(5)));
        assert_eq!(running(dir.path(), SUPERVISOR), None, "stopped is not running");

        clear_pid(dir.path(), SUPERVISOR);
        assert_eq!(read_pid(dir.path(), SUPERVISOR), None);
    }

    /// `stop` reaches what the process started, not just the process. On x86-64
    /// the vm runs cloud-hypervisor as a child of its own, and signalling only
    /// `hanzo-vm` would leave it holding the guest's memory and its ports.
    #[cfg(unix)]
    #[test]
    fn stopping_a_group_leader_takes_its_children() {
        let dir = tempfile::tempdir().unwrap();
        let (leader, child) = orphan(dir.path());
        assert!(alive(child));

        assert!(stop(leader, Duration::from_secs(5)));

        let deadline = Instant::now() + Duration::from_secs(5);
        while alive(child) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(50));
        }
        assert!(!alive(child), "the hypervisor outlived the vm's group");
    }

    /// Stopping something already gone is the outcome we wanted, not an error.
    #[cfg(unix)]
    #[test]
    fn stopping_what_is_already_gone_succeeds() {
        let mut child = Command::new("true").spawn().unwrap();
        let pid = child.id() as i32;
        child.wait().unwrap();
        assert!(stop(pid, Duration::from_millis(200)));
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
        let mut rpc = peer(dir.path(), &script);
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

    /// The state file phases round-trip; the ready-watcher believes `ready`,
    /// reports an error, and refuses to wait on a `down`.
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

    /// A fake peer speaking the real protocol, driven by a script.
    fn peer(dir: &Path, script: &str) -> Rpc {
        Rpc::start(Path::new("sh"), &["-c".into(), script.into()], &dir.join("log")).unwrap()
    }

    /// One `printf` of a protocol line, single-quoted for `sh`.
    fn line(json: &str) -> String {
        format!("printf '%s\\n' '{json}'; ")
    }

    /// A launch log as the vm would report one.
    fn launched() -> Log {
        let mut log = Log::default();
        log.text("kernel", "K");
        log.text("cmdline", "root=/dev/vda rw");
        log
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
            line(r#"{"jsonrpc":"2.0","method":"output","params":{"pid":"p1","stream":"stdout","data":""}}"#),
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

    /// A vm that says `ready` without saying what it booted is refused. An
    /// unmeasured cluster is not one this can file a document about, and
    /// filing nothing quietly would be worse than not starting.
    #[test]
    fn a_vm_that_reports_no_measurement_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let mut rpc = peer(dir.path(), &line(r#"{"jsonrpc":"2.0","method":"ready"}"#));
        let e = rpc.wait_ready().unwrap_err();
        assert!(e.to_string().contains("without reporting a measurement"), "{e}");
        let _ = rpc.child.wait();
    }

    /// The whole record: the launch the vm reported, the workload we deployed,
    /// and what the guest's platform said — filed, and read back through the
    /// fold. The peer answers `attest` with `none`, which is what every
    /// machine this runs on today answers.
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
        assert_eq!(m.workload.register(), deployed(&boot().cloud, &manifest).register());
        assert_eq!(m.hardware.platform, "none");
        assert!(m.hardware.report.is_none());

        // An edited document is refused, not repeated: the registers and the
        // bind are folded again on the way in.
        let path = measure_path(dir.path());
        let edited = std::fs::read_to_string(&path)
            .unwrap()
            .replace(&m.launch.register().hex(), &"ab".repeat(48));
        std::fs::write(&path, edited).unwrap();
        assert!(measured(dir.path()).is_err());

        // And a machine with nothing filed says so rather than inventing one.
        std::fs::remove_file(&path).unwrap();
        let e = measured(dir.path()).unwrap_err();
        assert!(e.to_string().contains("is a cluster up?"), "{e}");
    }

    /// The old spelling splits into service + tail, with clap's `--` shed.
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
}
