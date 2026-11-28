//! The supervisor's control socket: how a second `hanzo` reaches the runtime's guest.
//!
//! One process holds the vm — the supervisor, over the vm's stdio — and it is the
//! only thing that can run a command in the guest. Everything else a `hanzo`
//! invocation does to a warm runtime goes through the Kubernetes API, which is
//! enough for a Deployment and not for the node under it: an image that was built
//! on this machine has to be put in the node's own store, and no API call does that.
//!
//! So the supervisor listens on a unix socket beside its pid file, and answers a
//! fixed, short list of requests. It is not a shell: a caller names an operation,
//! never a command.
//!
//! The wire is JSON lines. One request, then any number of `{"progress": "…"}`
//! lines, then exactly one line with an `ok` member, and the connection closes.
//! The socket is the owner's alone (0600), like the kubeconfig that grants the same
//! cluster.

use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::time::Duration;

use super::{load, persist, Boot};
use crate::commands::vm::Rpc;

const SOCKET: &str = "control.sock";

pub(crate) fn socket_path(dir: &Path) -> PathBuf {
    dir.join(SOCKET)
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "op", rename_all = "snake_case")]
pub(crate) enum Request {
    /// Put the image of an OCI layout directory into the node's store.
    Load { layout: String },
    /// Remove every image of `repo` from the node's store except `keep`.
    Prune { repo: String, keep: String },
    /// What the runtime is and has left: its shape, free disk and memory, and the
    /// locally built images it holds.
    Facts,
    /// Quiesce k3s and pods, save the disk to the runtime state checkpoint
    /// (unless `save` is `false`, as on `hanzo reset`), and stop the vm.
    Stop {
        #[serde(default = "default_true")]
        save: bool,
    },
}

/// Bind the socket, replacing one a dead supervisor left behind.
pub(crate) fn listen(dir: &Path) -> Result<UnixListener> {
    let path = socket_path(dir);
    let _ = std::fs::remove_file(&path);
    let listener =
        UnixListener::bind(&path).with_context(|| format!("binding {}", path.display()))?;
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
            .with_context(|| format!("chmod 600 {}", path.display()))?;
    }
    listener
        .set_nonblocking(true)
        .context("making the control socket non-blocking")?;
    Ok(listener)
}

/// Answer the callers that are waiting, one at a time, and return `true` when a
/// caller asked the runtime to stop. The supervisor calls this between looks at its
/// vm; a request holds the vm's wire until it is done, which is the point — two
/// loads at once would interleave on it.
pub(crate) fn serve_pending_in(
    listener: &UnixListener,
    rpc: &mut Rpc,
    boot: &Boot,
    vm_home: &Path,
) -> bool {
    while let Ok((stream, _)) = listener.accept() {
        match serve(stream, rpc, boot, vm_home) {
            Ok(true) => return true,
            Ok(false) => {}
            Err(e) => eprintln!("control: {e:#}"),
        }
    }
    false
}

fn serve(stream: UnixStream, rpc: &mut Rpc, boot: &Boot, vm_home: &Path) -> Result<bool> {
    stream.set_nonblocking(false)?;
    stream.set_read_timeout(Some(Duration::from_secs(10)))?;
    let mut line = String::new();
    BufReader::new(stream.try_clone()?)
        .read_line(&mut line)
        .context("reading a request")?;
    let mut out = stream;
    let mut stopped = false;
    let reply = match serde_json::from_str::<Request>(line.trim()) {
        Err(e) => Err(anyhow!("not a request this runtime knows: {e}")),
        Ok(req) => {
            let is_stop = matches!(req, Request::Stop { .. });
            let mut progress = |msg: &str| {
                let _ = writeln!(out, "{}", json!({ "progress": msg }));
                let _ = out.flush();
            };
            let res = answer(req, rpc, boot, vm_home, &mut progress);
            if is_stop && res.is_ok() {
                stopped = true;
            }
            res
        }
    };
    let last = match reply {
        Ok(mut v) => {
            v["ok"] = json!(true);
            v
        }
        Err(e) => json!({ "ok": false, "error": format!("{e:#}") }),
    };
    writeln!(out, "{last}").context("writing a reply")?;
    let _ = out.flush();
    Ok(stopped)
}

fn answer(
    req: Request,
    rpc: &mut Rpc,
    boot: &Boot,
    vm_home: &Path,
    progress: &mut dyn FnMut(&str),
) -> Result<Value> {
    match req {
        Request::Load { layout } => {
            let done = load::load(rpc, Path::new(&layout), progress)?;
            Ok(serde_json::to_value(done)?)
        }
        Request::Prune { repo, keep } => Ok(json!({ "removed": load::prune(rpc, &repo, &keep)? })),
        Request::Facts => load::facts(rpc, boot),
        Request::Stop { save } => {
            let saved = persist::stop_and_save_in(rpc, vm_home, save, progress)?;
            Ok(json!({ "saved": saved }))
        }
    }
}

/// Why a runtime could not be asked.
#[derive(Debug)]
pub(crate) enum Unreachable {
    /// No socket: the supervisor is an older `hanzo`, from before it had one.
    NoSocket,
    Other(anyhow::Error),
}

/// Send one request to the running supervisor and return its final line.
pub(crate) fn call(
    dir: &Path,
    req: &Request,
    mut progress: impl FnMut(&str),
) -> std::result::Result<Value, Unreachable> {
    let path = socket_path(dir);
    let mut stream = match UnixStream::connect(&path) {
        Ok(s) => s,
        Err(e)
            if matches!(
                e.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused
            ) =>
        {
            return Err(Unreachable::NoSocket)
        }
        Err(e) => return Err(Unreachable::Other(anyhow!("{}: {e}", path.display()))),
    };
    let run = || -> Result<Value> {
        writeln!(stream, "{}", serde_json::to_string(req)?).context("sending the request")?;
        stream.flush()?;
        for line in BufReader::new(stream.try_clone()?).lines() {
            let line = line.context("reading the runtime's answer")?;
            let v: Value = match serde_json::from_str(line.trim()) {
                Ok(v) => v,
                Err(_) => continue,
            };
            if let Some(msg) = v.get("progress").and_then(Value::as_str) {
                progress(msg);
                continue;
            }
            return match v.get("ok").and_then(Value::as_bool) {
                Some(true) => Ok(v),
                Some(false) => bail!(
                    "{}",
                    v.get("error")
                        .and_then(Value::as_str)
                        .unwrap_or("the runtime refused")
                ),
                None => continue,
            };
        }
        bail!(
            "the runtime closed the connection without answering — see ~/.hanzo/up/supervisor.log"
        )
    };
    let mut run = run;
    run().map_err(Unreachable::Other)
}

/// What to tell someone whose runtime has no control socket.
pub(crate) const OLDER_RUNTIME: &str =
    "the running runtime was started by a hanzo from before it could be given a local image. \
     Restart it once: `hanzo stop`, then run this again";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requests_are_named_operations_on_the_wire() {
        let load: Request = serde_json::from_str(r#"{"op":"load","layout":"/x/image"}"#).unwrap();
        assert_eq!(
            load,
            Request::Load {
                layout: "/x/image".into()
            }
        );
        assert_eq!(
            serde_json::to_string(&Request::Facts).unwrap(),
            r#"{"op":"facts"}"#
        );
        let stop_default: Request = serde_json::from_str(r#"{"op":"stop"}"#).unwrap();
        assert_eq!(stop_default, Request::Stop { save: true });
        assert_eq!(
            serde_json::to_string(&Request::Stop { save: false }).unwrap(),
            r#"{"op":"stop","save":false}"#
        );
        // Nothing on this wire runs a command a caller wrote.
        assert!(serde_json::from_str::<Request>(r#"{"op":"exec","argv":["sh"]}"#).is_err());
    }

    /// `Request::Stop { save: true }` drains k3s in the guest, asks `hanzo-vm` to
    /// checkpoint to the staging file, atomically promotes it to `<state>.ext4`,
    /// warms `digests.json`, and sweeps any leftover `instances/<vm_pid>` disk.
    #[test]
    fn stop_request_quiesces_checkpoints_and_cleans_instance_dir() {
        use crate::commands::vm::wire::{launched, line, peer};

        let dir = tempfile::tempdir().unwrap();
        let vm_home = tempfile::tempdir().unwrap();
        let listener = listen(dir.path()).unwrap();
        let staging_ext4 =
            persist::checkpoint_file(vm_home.path(), &persist::state_staging_checkpoint());
        let final_ext4 = persist::checkpoint_file(vm_home.path(), &persist::state_checkpoint());
        let seen_exec = dir.path().join("quiesce.txt");
        let seen_cp = dir.path().join("cp.txt");

        let measurement = json!({"jsonrpc": "2.0", "method": "measurement", "params": launched()});
        let script = format!(
            "{}{}read q; printf '%s' \"$q\" > {}; {}read cp; printf '%s' \"$cp\" > {}; mkdir -p {}; printf 'ext4-state-bytes' > {}; {}",
            line(&measurement.to_string()),
            line(r#"{"jsonrpc":"2.0","method":"ready"}"#),
            seen_exec.display(),
            line(r#"{"jsonrpc":"2.0","id":1,"result":{"stdout":"","stderr":"","exit_code":0}}"#),
            seen_cp.display(),
            staging_ext4.parent().unwrap().display(),
            staging_ext4.display(),
            line(r#"{"jsonrpc":"2.0","id":2,"result":{}}"#),
        );
        let mut rpc = peer(dir.path(), &script);
        let vm_pid = rpc.child.id();
        rpc.wait_ready().unwrap();

        // Simulate a leftover 26 GB instance directory that must not leak.
        let inst_dir = vm_home.path().join("instances").join(vm_pid.to_string());
        std::fs::create_dir_all(&inst_dir).unwrap();
        std::fs::write(inst_dir.join("rootfs.ext4"), b"transient").unwrap();

        let dir_path = dir.path().to_path_buf();
        let client = std::thread::spawn(move || {
            let mut notes = Vec::new();
            let ans = call(&dir_path, &Request::Stop { save: true }, |m| {
                notes.push(m.to_string())
            })
            .unwrap();
            (ans, notes)
        });

        let boot = Boot {
            cpus: 4,
            memory_mb: 4096,
            disk_mb: 32768,
            cloud: super::super::CLOUD.into(),
            staged: None,
            iam_issuer: None,
            ai_base_url: None,
            strict_registry: false,
            local: None,
        };
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        let mut stopped = false;
        while std::time::Instant::now() < deadline {
            if serve_pending_in(&listener, &mut rpc, &boot, vm_home.path()) {
                stopped = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(stopped, "supervisor must observe Stop request");

        let (ans, notes) = client.join().unwrap();
        assert_eq!(ans["saved"], persist::state_checkpoint());
        assert!(
            notes.iter().any(|m| m.contains("stopping k3s")),
            "{notes:?}"
        );
        assert!(notes.iter().any(|m| m.contains("checkpoint")), "{notes:?}");

        let exec_wire = std::fs::read_to_string(&seen_exec).unwrap();
        assert!(exec_wire.contains("cordon"), "{exec_wire}");
        assert!(exec_wire.contains("pkill -TERM -x k3s"), "{exec_wire}");

        let cp_wire: Value =
            serde_json::from_str(&std::fs::read_to_string(&seen_cp).unwrap()).unwrap();
        assert_eq!(cp_wire["method"], "checkpoint");
        assert_eq!(
            cp_wire["params"]["name"],
            persist::state_staging_checkpoint()
        );

        assert!(
            !staging_ext4.exists(),
            "staging checkpoint must be promoted"
        );
        assert_eq!(std::fs::read(&final_ext4).unwrap(), b"ext4-state-bytes");
        assert!(
            vm_home.path().join("digests.json").is_file(),
            "digest cache warmed"
        );
        assert!(!inst_dir.exists(), "instance dir must be swept after stop");
    }

    #[test]
    fn a_runtime_with_no_socket_is_told_apart_from_one_that_failed() {
        let dir = tempfile::tempdir().unwrap();
        match call(dir.path(), &Request::Facts, |_| {}) {
            Err(Unreachable::NoSocket) => {}
            other => panic!("want NoSocket, got {other:?}"),
        }
    }

    #[test]
    fn the_socket_is_the_owners_alone() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let _l = listen(dir.path()).unwrap();
        let mode = std::fs::metadata(socket_path(dir.path()))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    /// The client's half of the wire: progress lines reach the caller as they come,
    /// and the one line with `ok` ends the call, as an answer or as the error it names.
    #[test]
    fn progress_is_relayed_and_the_last_line_decides() {
        let dir = tempfile::tempdir().unwrap();
        let listener = UnixListener::bind(socket_path(dir.path())).unwrap();
        let peer = std::thread::spawn(move || {
            for last in [
                r#"{"ok":true,"ref":"hanzo.local/x:1"}"#,
                r#"{"ok":false,"error":"no room"}"#,
            ] {
                let (mut s, _) = listener.accept().unwrap();
                let mut line = String::new();
                BufReader::new(s.try_clone().unwrap())
                    .read_line(&mut line)
                    .unwrap();
                assert!(line.contains(r#""op":"load""#), "{line}");
                writeln!(s, r#"{{"progress":"3 of 61 layers"}}"#).unwrap();
                writeln!(s, "{last}").unwrap();
            }
        });
        let req = Request::Load {
            layout: "/x".into(),
        };
        let mut seen = Vec::new();
        let v = call(dir.path(), &req, |m| seen.push(m.to_string())).unwrap();
        assert_eq!(v["ref"], "hanzo.local/x:1");
        assert_eq!(seen, ["3 of 61 layers"]);
        match call(dir.path(), &req, |_| {}) {
            Err(Unreachable::Other(e)) => assert_eq!(e.to_string(), "no room"),
            other => panic!("want the runtime's error, got {other:?}"),
        }
        peer.join().unwrap();
    }
}
