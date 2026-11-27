//! `hanzo up --local` — Cloud from this machine's source, in the local runtime.
//!
//! Local source, built here by the checkout's own build; an OCI image layout on this
//! disk; that image loaded into the runtime's containerd; a Deployment that names
//! it exactly and is told never to pull. No docker, no registry.
//!
//! **It does not fall back.** Every step either succeeds or ends the command with
//! what failed: a build that fails, a directory that is no image, a runtime that
//! cannot be reached, a node with no room. Nothing here ever resolves a name
//! against a registry, so there is no published image this could quietly deploy in
//! place of the one that was asked for; and the Deployment is only written once the
//! node holds the image.

use anyhow::{bail, Context, Result};
use colored::*;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use super::control::{self, Request, Unreachable};
use super::manifest::{discover_app_manifest, ground_for, workload_for};
use super::{
    budget, kubeconfig_path, load, running, start, up_dir, wait_ready_state, Boot, KubeClient,
    LOCAL_PORT, SUPERVISOR,
};
use crate::commands::host;
use crate::config::Config;
use crate::image;

/// How long the new pod has to become ready.
const ROLLOUT: Duration = Duration::from_secs(300);

/// What `--local` named: a layout, and the command that makes it when the
/// directory declares one.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Source {
    pub dir: PathBuf,
    pub build: Option<String>,
    pub layout: PathBuf,
}

/// Resolve `--local` (`arg` empty) or `--local=<layout>` from `cwd`.
pub(crate) fn resolve(arg: &str, cwd: &Path) -> Result<Source> {
    if !arg.trim().is_empty() {
        return Ok(Source {
            dir: cwd.to_path_buf(),
            build: None,
            layout: cwd.join(arg.trim()),
        });
    }
    let declared = discover_app_manifest(cwd)?.and_then(|m| m.local);
    let Some(local) = declared else {
        bail!(
            "{} declares no local build. Name the OCI image layout to run \
             (`hanzo up --local=<dir>`), or declare the build in hanzo.config.json:\n  \
             {{ \"local\": {{ \"build\": \"make local-image\", \"image\": \"bin/local/image\" }} }}",
            cwd.display()
        );
    };
    if local.image.trim().is_empty() {
        bail!("hanzo.config.json declares a local build with no image directory");
    }
    Ok(Source {
        dir: cwd.to_path_buf(),
        build: local.build.filter(|b| !b.trim().is_empty()),
        layout: cwd.join(local.image.trim()),
    })
}

/// Run the checkout's own build and answer the layout it left. A build that fails
/// ends the command: there is no image to run, and no other one is substituted.
pub(crate) fn prepare(arg: &str, cwd: &Path) -> Result<(String, Duration)> {
    let src = resolve(arg, cwd)?;
    let started = Instant::now();
    if let Some(cmd) = &src.build {
        println!("{} building this checkout: {}", "→".cyan(), cmd.bold());
        let status = Command::new("sh")
            .arg("-c")
            .arg(cmd)
            .current_dir(&src.dir)
            .status()
            .with_context(|| format!("running `{cmd}`"))?;
        if !status.success() {
            bail!(
                "the local build failed ({status}): `{cmd}` in {}. Nothing was deployed, and \
                 what was running still is",
                src.dir.display()
            );
        }
    }
    let layout = std::fs::canonicalize(&src.layout).with_context(|| {
        format!(
            "{} does not exist — the local build writes the image there",
            src.layout.display()
        )
    })?;
    Ok((layout.to_string_lossy().into_owned(), started.elapsed()))
}

fn ask(dir: &Path, req: &Request, progress: impl FnMut(&str)) -> Result<serde_json::Value> {
    match control::call(dir, req, progress) {
        Ok(v) => Ok(v),
        Err(Unreachable::NoSocket) => bail!("{}", control::OLDER_RUNTIME),
        Err(Unreachable::Other(e)) => Err(e),
    }
}

/// Bring Cloud up on the image in `boot.local`.
pub(crate) async fn up(cfg: &mut Config, boot: Boot, built: Option<Duration>) -> Result<()> {
    let layout = PathBuf::from(
        boot.local
            .clone()
            .context("no local image layout was named")?,
    );
    let total = Instant::now();
    let image = load::read_layout(&layout)?;
    if image.architecture != image::architecture() {
        bail!(
            "{} is built for linux/{} and the runtime on this machine is linux/{} — \
             build it for this machine's architecture",
            image.reference,
            image.architecture,
            image::architecture()
        );
    }

    let dir = up_dir()?;
    if running(&dir, SUPERVISOR).is_none() {
        if super::host_native_k3s_active() {
            bail!(
                "this machine's k3s was not started by hanzo, so hanzo cannot put an image in \
                 its store. Import it yourself (`sudo k3s ctr -n k8s.io images import <archive>`), \
                 or stop that k3s and let `hanzo up --local` boot its own runtime"
            );
        }
        // The runtime alone: no published image is deployed on the way to a local one.
        start(cfg, boot.clone(), false).await?;
    } else {
        wait_ready_state(&dir)?;
    }

    // The runtime's real shape, from the process that booted it. The flags of this
    // invocation describe a vm that may not be the one running.
    let facts = ask(&dir, &Request::Facts, |_| {})?;
    let n = |k: &str| facts.get(k).and_then(|v| v.as_u64()).unwrap_or(0);
    let shape = Boot {
        cpus: n("cpus") as u32,
        memory_mb: n("memory_mb"),
        disk_mb: n("disk_mb"),
        cloud: image.reference.clone(),
        ..boot
    };
    let plan = budget::plan(shape.memory_mb, shape.disk_mb)?;

    let t = Instant::now();
    let req = Request::Load {
        layout: layout.to_string_lossy().into_owned(),
    };
    let loaded = ask(&dir, &req, |msg| println!("  {}", msg.dimmed()))?;
    let loaded: load::Loaded =
        serde_json::from_value(loaded).context("reading what the runtime loaded")?;
    let load_time = t.elapsed();

    let t = Instant::now();
    let kube = KubeClient::from_kubeconfig(&kubeconfig_path()?)?;
    kube.apply_workload(&ground_for(&shape), &workload_for(&shape)?)
        .await?;
    kube.rollout("hanzo", "cloud", ROLLOUT)
        .await
        .with_context(|| {
            format!(
                "{} was loaded and the pod did not come up on it",
                image.reference
            )
        })?;
    let rollout_time = t.elapsed();
    super::write_state(&dir, "ready");

    // The builds before this one, now that nothing runs them.
    let prune = Request::Prune {
        repo: load::repo_of(&image.reference).to_string(),
        keep: image.reference.clone(),
    };
    if let Err(e) = ask(&dir, &prune, |_| {}) {
        crate::warn(&format!("older local images were not removed: {e:#}"));
    }

    println!("{} Cloud is running this checkout", "✓".green());
    println!(
        "  image    {} (in the runtime's store; never pulled)",
        image.reference.cyan()
    );
    println!("  source   {}  version {}", image.revision, image.version);
    println!(
        "  loaded   {} of {} layers, {} MiB of {}",
        loaded.layers_sent,
        loaded.layers,
        loaded.bytes_sent >> 20,
        loaded.bytes >> 20,
    );
    println!("  budget   {}", plan.describe());
    let secs = |d: Duration| format!("{:.1}s", d.as_secs_f64());
    println!(
        "  timings  {}load {} · rollout {} · total {}",
        built
            .map(|b| format!("build {} · ", secs(b)))
            .unwrap_or_default(),
        secs(load_time),
        secs(rollout_time),
        secs(total.elapsed() + built.unwrap_or_default()),
    );
    match answering(&image.version).await {
        Some(true) => println!("  answers  http://127.0.0.1:{LOCAL_PORT} as {}", image.version),
        Some(false) => crate::warn(&format!(
            "http://127.0.0.1:{LOCAL_PORT} does not answer as {} — `hanzo k8s -n hanzo logs deploy/cloud`",
            image.version
        )),
        None => {}
    }
    let hanzod = host::register_upstream(Some(&format!("127.0.0.1:{LOCAL_PORT}")))
        .await
        .is_ok();
    print!("{}", super::format_endpoints(&kubeconfig_path()?, hanzod));
    Ok(())
}

/// Whether the forwarded port answers as the version just deployed. `None` when the
/// image states no version to compare.
async fn answering(version: &str) -> Option<bool> {
    if version.is_empty() {
        return None;
    }
    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(3))
        .build()
        .ok()?;
    let url = format!("http://127.0.0.1:{LOCAL_PORT}/v1/health");
    for _ in 0..10 {
        if let Ok(resp) = http.get(&url).send().await {
            if let Some(v) = resp
                .headers()
                .get("x-api-version")
                .and_then(|v| v.to_str().ok())
            {
                return Some(v == version);
            }
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    Some(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_named_layout_is_taken_as_it_is_and_builds_nothing() {
        let cwd = tempfile::tempdir().unwrap();
        let src = resolve("out/image", cwd.path()).unwrap();
        assert_eq!(src.layout, cwd.path().join("out/image"));
        assert_eq!(src.build, None);
    }

    #[test]
    fn a_checkout_declares_its_own_build() {
        let cwd = tempfile::tempdir().unwrap();
        std::fs::write(
            cwd.path().join("hanzo.config.json"),
            r#"{"name":"cloud","local":{"build":"make local-image","image":"bin/local/image"}}"#,
        )
        .unwrap();
        let src = resolve("", cwd.path()).unwrap();
        assert_eq!(src.build.as_deref(), Some("make local-image"));
        assert_eq!(src.layout, cwd.path().join("bin/local/image"));
    }

    /// The refusals that matter most: asked for a local build where there is none,
    /// or one that fails, the command stops. It names no image and deploys nothing.
    #[test]
    fn nothing_to_build_and_a_build_that_fails_both_end_the_command() {
        let cwd = tempfile::tempdir().unwrap();
        let e = resolve("", cwd.path()).unwrap_err().to_string();
        assert!(e.contains("declares no local build"), "{e}");

        std::fs::write(
            cwd.path().join("hanzo.config.json"),
            r#"{"local":{"build":"exit 3","image":"image"}}"#,
        )
        .unwrap();
        let e = prepare("", cwd.path()).unwrap_err().to_string();
        assert!(e.contains("the local build failed"), "{e}");
        assert!(e.contains("Nothing was deployed"), "{e}");

        // A build that succeeds and leaves no image is not a success either.
        std::fs::write(
            cwd.path().join("hanzo.config.json"),
            r#"{"local":{"build":"true","image":"image"}}"#,
        )
        .unwrap();
        let e = format!("{:#}", prepare("", cwd.path()).unwrap_err());
        assert!(e.contains("does not exist"), "{e}");
    }
}
