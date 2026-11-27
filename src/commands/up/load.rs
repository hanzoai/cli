//! An image built on this machine, put into the runtime's own containerd.
//!
//! The image arrives as an OCI image layout directory. What crosses into the guest
//! is an OCI archive holding the image's documents and only the layers the node
//! does not already have: containerd keeps layers by digest, so a build that
//! changed one app sends one layer, and `ctr images import` resolves the rest
//! against its store. No registry is involved at either end, and nothing is pulled.
//!
//! The archive crosses on the vm's stdio wire, the one channel the supervisor has,
//! in parts of [`PART`] bytes written to the guest's disk and read back in order
//! into the import. The wire carries a file whole, so the parts are what bound a
//! message; the guest's disk holds them only until the import returns.

use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashSet;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Instant;

use super::manifest::{is_local, LOCAL_REGISTRY};
use super::Boot;
use crate::commands::vm::Rpc;

/// k3s's containerd, and the namespace the kubelet reads images from.
const CTR: [&str; 5] = [
    "k3s",
    "ctr",
    "--address",
    "/run/k3s/containerd/containerd.sock",
    "--namespace=k8s.io",
];

/// Where the parts of an archive wait in the guest.
const STAGING: &str = "/var/lib/hanzo/load";

/// The bytes of one message on the wire. hanzo-vm 2.0.2 breaks the pipe on a
/// message of a megabyte (measured: 512 KiB of file passes, 1 MiB does not), and
/// base64 makes four bytes of three.
const PART: usize = 512 << 10;

/// What the node must have free beyond the bytes sent: the parts, the same bytes
/// again in containerd's store, and the layers unpacked, which for compressed Go
/// binaries is under three times their size.
const ROOM_FACTOR: u64 = 5;
const ROOM_SLACK: u64 = 512 << 20;

#[derive(Debug, Deserialize)]
struct Descriptor {
    digest: String,
    size: u64,
    #[serde(default)]
    annotations: std::collections::BTreeMap<String, String>,
    platform: Option<Platform>,
}

#[derive(Debug, Deserialize)]
struct Platform {
    architecture: String,
    os: String,
}

#[derive(Debug, Deserialize)]
struct Index {
    manifests: Vec<Descriptor>,
}

#[derive(Debug, Deserialize)]
struct Manifest {
    config: Descriptor,
    layers: Vec<Descriptor>,
}

/// One image, read out of a layout: its name, and every blob it is made of.
#[derive(Debug)]
pub(crate) struct Image {
    pub reference: String,
    pub architecture: String,
    pub revision: String,
    pub version: String,
    manifest: Descriptor,
    config: Descriptor,
    layers: Vec<Descriptor>,
    dir: PathBuf,
}

impl Image {
    fn blob(&self, digest: &str) -> PathBuf {
        blob_path(&self.dir, digest)
    }

    /// The bytes of every layer, compressed as they are stored.
    pub(crate) fn size(&self) -> u64 {
        self.layers.iter().map(|l| l.size).sum()
    }

    #[cfg(test)]
    pub(crate) fn layer_count(&self) -> usize {
        self.layers.len()
    }
}

fn blob_path(dir: &Path, digest: &str) -> PathBuf {
    dir.join("blobs")
        .join("sha256")
        .join(digest.trim_start_matches("sha256:"))
}

/// Read the one image an OCI layout names. A layout that names none, or several,
/// or an image with no name, is refused: what gets deployed is said by the build,
/// not chosen here.
pub(crate) fn read_layout(dir: &Path) -> Result<Image> {
    let index_path = dir.join("index.json");
    let raw = std::fs::read(&index_path).with_context(|| {
        format!(
            "{} is not an OCI image layout (no index.json) — has the local build run?",
            dir.display()
        )
    })?;
    let index: Index = serde_json::from_slice(&raw)
        .with_context(|| format!("parsing {}", index_path.display()))?;
    let mut manifests = index.manifests;
    if manifests.len() != 1 {
        bail!(
            "{} names {} images; a local build names exactly one",
            dir.display(),
            manifests.len()
        );
    }
    let manifest = manifests.remove(0);
    let reference = manifest
        .annotations
        .get("io.containerd.image.name")
        .cloned()
        .ok_or_else(|| {
            anyhow!(
                "the image in {} has no name (io.containerd.image.name)",
                dir.display()
            )
        })?;
    if !is_local(&reference) {
        bail!(
            "the image in {} is named {reference}; a local build is named under {LOCAL_REGISTRY}/, \
             which no registry answers to",
            dir.display()
        );
    }
    let platform = manifest
        .platform
        .as_ref()
        .ok_or_else(|| anyhow!("the image {reference} does not say what platform it is for"))?;
    if platform.os != "linux" {
        bail!(
            "the image {reference} is for {}, and the runtime is linux",
            platform.os
        );
    }
    let architecture = platform.architecture.clone();
    let body: Manifest = read_json(&blob_path(dir, &manifest.digest))?;
    let config: Value = read_json(&blob_path(dir, &body.config.digest))?;
    let label = |k: &str| {
        config
            .pointer("/config/Labels")
            .and_then(|l| l.get(k))
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string()
    };
    for l in &body.layers {
        let p = blob_path(dir, &l.digest);
        let have = std::fs::metadata(&p).map(|m| m.len()).unwrap_or(0);
        if have != l.size {
            bail!(
                "the layout is incomplete: layer {} is {have} bytes on disk and {} in the manifest",
                l.digest,
                l.size
            );
        }
    }
    Ok(Image {
        reference,
        architecture,
        revision: label("org.opencontainers.image.revision"),
        version: label("org.opencontainers.image.version"),
        manifest,
        config: body.config,
        layers: body.layers,
        dir: dir.to_path_buf(),
    })
}

fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T> {
    let raw = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    serde_json::from_slice(&raw).with_context(|| format!("parsing {}", path.display()))
}

/// `name:tag` → `name`.
pub(crate) fn repo_of(reference: &str) -> &str {
    match reference.rfind(':') {
        Some(i) if !reference[i..].contains('/') => &reference[..i],
        _ => reference,
    }
}

/// What a load did.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct Loaded {
    #[serde(rename = "ref")]
    pub reference: String,
    pub layers: usize,
    pub layers_sent: usize,
    pub bytes: u64,
    pub bytes_sent: u64,
    pub seconds: f64,
}

fn ctr(rpc: &mut Rpc, args: &[&str]) -> Result<(String, String, i64)> {
    let argv: Vec<&str> = CTR.iter().copied().chain(args.iter().copied()).collect();
    rpc.exec(&argv)
}

/// Every blob containerd already holds, by digest.
fn held(rpc: &mut Rpc) -> Result<HashSet<String>> {
    let (out, err, code) = ctr(rpc, &["content", "ls", "-q"])?;
    if code != 0 {
        bail!("asking containerd what it holds: {}", err.trim());
    }
    Ok(out
        .lines()
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty())
        .collect())
}

/// Whether the node's store names this image.
fn named(rpc: &mut Rpc, reference: &str) -> Result<bool> {
    let (out, err, code) = ctr(rpc, &["images", "ls", "-q", &format!("name=={reference}")])?;
    if code != 0 {
        bail!("asking containerd for {reference}: {}", err.trim());
    }
    Ok(out.lines().any(|l| l.trim() == reference))
}

/// Bytes free where k3s keeps everything.
fn free_bytes(rpc: &mut Rpc) -> Result<u64> {
    let (out, err, code) = rpc.exec(&["df", "-B1", "--output=avail", "/var/lib/hanzo"])?;
    if code != 0 {
        bail!("reading the node's free disk: {}", err.trim());
    }
    out.lines()
        .filter_map(|l| l.trim().parse::<u64>().ok())
        .next()
        .ok_or_else(|| anyhow!("df answered without a number: {}", out.trim()))
}

/// The wire's far end for a tar stream: parts of [`PART`] bytes, numbered in order.
struct Parts<'a> {
    rpc: &'a mut Rpc,
    dir: String,
    buf: Vec<u8>,
    next: u32,
    sent: u64,
}

impl Parts<'_> {
    fn send(&mut self, upto: usize) -> std::io::Result<()> {
        let path = format!("{}/part-{:08}", self.dir, self.next);
        self.rpc
            .write_bytes(&path, &self.buf[..upto])
            .map_err(|e| std::io::Error::other(format!("{e:#}")))?;
        self.sent += upto as u64;
        self.next += 1;
        self.buf.drain(..upto);
        Ok(())
    }
}

impl Write for Parts<'_> {
    fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
        self.buf.extend_from_slice(data);
        while self.buf.len() >= PART {
            self.send(PART)?;
        }
        Ok(data.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn header(size: u64) -> tar::Header {
    let mut h = tar::Header::new_gnu();
    h.set_size(size);
    h.set_mode(0o644);
    h.set_mtime(0);
    h.set_entry_type(tar::EntryType::Regular);
    h
}

/// Write the archive of `image` that a node holding `have` needs: the layout's two
/// fixed files, the image's documents, and the layers the node lacks.
fn archive<W: Write>(
    out: W,
    image: &Image,
    have: &HashSet<String>,
    mut each: impl FnMut(usize, usize),
) -> Result<(W, usize)> {
    let mut tar = tar::Builder::new(out);
    let put = |tar: &mut tar::Builder<W>, name: &str, path: &Path| -> Result<()> {
        let f = std::fs::File::open(path).with_context(|| format!("opening {}", path.display()))?;
        let size = f.metadata()?.len();
        tar.append_data(&mut header(size), name, f)
            .with_context(|| format!("sending {name}"))
    };
    put(&mut tar, "oci-layout", &image.dir.join("oci-layout"))?;
    put(&mut tar, "index.json", &image.dir.join("index.json"))?;
    for d in [&image.manifest, &image.config] {
        let name = format!("blobs/sha256/{}", d.digest.trim_start_matches("sha256:"));
        put(&mut tar, &name, &image.blob(&d.digest))?;
    }
    let missing: Vec<&Descriptor> = image
        .layers
        .iter()
        .filter(|l| !have.contains(&l.digest))
        .collect();
    for (i, l) in missing.iter().enumerate() {
        let name = format!("blobs/sha256/{}", l.digest.trim_start_matches("sha256:"));
        put(&mut tar, &name, &image.blob(&l.digest))?;
        each(i + 1, missing.len());
    }
    Ok((
        tar.into_inner().context("closing the archive")?,
        missing.len(),
    ))
}

/// Put the image of `layout` into the node's store and answer what was sent.
pub(crate) fn load(rpc: &mut Rpc, layout: &Path, progress: &mut dyn FnMut(&str)) -> Result<Loaded> {
    let started = Instant::now();
    let image = read_layout(layout)?;
    let have = held(rpc)?;
    let need: u64 = image
        .layers
        .iter()
        .filter(|l| !have.contains(&l.digest))
        .map(|l| l.size)
        .sum();
    let layers_sent = image
        .layers
        .iter()
        .filter(|l| !have.contains(&l.digest))
        .count();

    // Nothing to send and the name already there: the node has this build.
    if need == 0 && named(rpc, &image.reference)? {
        return Ok(Loaded {
            reference: image.reference.clone(),
            layers: image.layers.len(),
            layers_sent: 0,
            bytes: image.size(),
            bytes_sent: 0,
            seconds: started.elapsed().as_secs_f64(),
        });
    }

    let free = free_bytes(rpc)?;
    let room = need.saturating_mul(ROOM_FACTOR).saturating_add(ROOM_SLACK);
    if free < room {
        bail!(
            "the runtime has {} MiB of disk free and loading this image takes about {} \
             ({} MiB of new layers, stored and unpacked). Images no pod uses are removed by \
             `hanzo stop` and a fresh start; a larger disk is `hanzo start --disk-size`",
            free >> 20,
            room >> 20,
            need >> 20,
        );
    }

    let dir = format!("{STAGING}/{}", std::process::id());
    let _ = rpc.exec(&["rm", "-rf", STAGING]);
    rpc.mkdir(&dir)?;
    progress(&format!(
        "sending {} of {} layers ({} MiB of {})",
        layers_sent,
        image.layers.len(),
        need >> 20,
        image.size() >> 20,
    ));
    let sent = (|| -> Result<u64> {
        let parts = Parts {
            rpc,
            dir: dir.clone(),
            buf: Vec::with_capacity(PART * 2),
            next: 0,
            sent: 0,
        };
        let mut last = Instant::now();
        let (mut parts, _) = archive(parts, &image, &have, |i, n| {
            if last.elapsed().as_secs() >= 2 || i == n {
                last = Instant::now();
                progress(&format!("{i} of {n} layers sent"));
            }
        })?;
        if !parts.buf.is_empty() {
            let rest = parts.buf.len();
            parts.send(rest).map_err(|e| anyhow!("{e}"))?;
        }
        Ok(parts.sent)
    })();
    let sent = match sent {
        Ok(n) => n,
        Err(e) => {
            let _ = rpc.exec(&["rm", "-rf", STAGING]);
            return Err(e.context("sending the image to the runtime"));
        }
    };

    progress("importing into containerd");
    let import = format!("cat {dir}/part-* | {} images import -", CTR.join(" "));
    let out = rpc.exec(&["sh", "-c", &import]);
    let _ = rpc.exec(&["rm", "-rf", STAGING]);
    let (stdout, stderr, code) = out?;
    if code != 0 {
        bail!(
            "containerd refused the image: {}",
            if stderr.trim().is_empty() {
                stdout.trim()
            } else {
                stderr.trim()
            }
        );
    }
    if !named(rpc, &image.reference)? {
        bail!(
            "containerd imported the archive and holds no {} — its store is not what the kubelet reads",
            image.reference
        );
    }
    Ok(Loaded {
        reference: image.reference.clone(),
        layers: image.layers.len(),
        layers_sent,
        bytes: image.size(),
        bytes_sent: sent,
        seconds: started.elapsed().as_secs_f64(),
    })
}

/// The locally built images the node holds, by name.
fn local_images(rpc: &mut Rpc) -> Result<Vec<String>> {
    let (out, err, code) = ctr(rpc, &["images", "ls", "-q"])?;
    if code != 0 {
        bail!("listing the node's images: {}", err.trim());
    }
    Ok(out
        .lines()
        .map(str::trim)
        .filter(|l| is_local(l))
        .map(str::to_string)
        .collect())
}

/// Remove every image of `repo` but `keep`, and answer what went. The layers an
/// older build shares with the kept one stay: containerd counts references.
pub(crate) fn prune(rpc: &mut Rpc, repo: &str, keep: &str) -> Result<Vec<String>> {
    if !is_local(repo) {
        bail!("{repo} is not a local build's name; only those are pruned here");
    }
    let mut removed = Vec::new();
    for name in local_images(rpc)? {
        if name != keep && repo_of(&name) == repo {
            let (_, err, code) = ctr(rpc, &["images", "rm", &name])?;
            if code != 0 {
                bail!("removing {name}: {}", err.trim());
            }
            removed.push(name);
        }
    }
    Ok(removed)
}

/// What the runtime is and has left.
pub(crate) fn facts(rpc: &mut Rpc, boot: &Boot) -> Result<Value> {
    let (df, _, _) = rpc.exec(&["df", "-B1", "--output=size,avail", "/var/lib/hanzo"])?;
    let mut nums = df
        .lines()
        .filter_map(|l| {
            let mut it = l.split_whitespace().map(|t| t.parse::<u64>());
            match (it.next(), it.next()) {
                (Some(Ok(a)), Some(Ok(b))) => Some((a, b)),
                _ => None,
            }
        })
        .next()
        .unwrap_or((0, 0));
    if nums.0 == 0 {
        nums = (0, free_bytes(rpc).unwrap_or(0));
    }
    let meminfo = rpc
        .read_file("/proc/meminfo")
        .map(|b| String::from_utf8_lossy(&b).into_owned())
        .unwrap_or_default();
    let kb = |key: &str| {
        meminfo
            .lines()
            .find(|l| l.starts_with(key))
            .and_then(|l| l.split_whitespace().nth(1))
            .and_then(|n| n.parse::<u64>().ok())
            .unwrap_or(0)
    };
    Ok(json!({
        "cpus": boot.cpus,
        "memory_mb": boot.memory_mb,
        "disk_mb": boot.disk_mb,
        "disk_bytes": nums.0,
        "disk_free_bytes": nums.1,
        "memory_total_kb": kb("MemTotal:"),
        "memory_available_kb": kb("MemAvailable:"),
        "local_images": local_images(rpc).unwrap_or_default(),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::{Digest, Sha256};

    fn digest(b: &[u8]) -> String {
        format!(
            "sha256:{}",
            Sha256::digest(b)
                .iter()
                .map(|x| format!("{x:02x}"))
                .collect::<String>()
        )
    }

    /// A layout as cmd/pack writes one: an index naming one manifest for one
    /// platform, a config with labels, and `layers` layers.
    pub(crate) fn layout(dir: &Path, name: &str, layers: &[&[u8]]) -> Vec<String> {
        let blobs = dir.join("blobs").join("sha256");
        std::fs::create_dir_all(&blobs).unwrap();
        let put = |b: &[u8]| -> String {
            let d = digest(b);
            std::fs::write(blobs.join(d.trim_start_matches("sha256:")), b).unwrap();
            d
        };
        let config = serde_json::to_vec(&json!({
            "architecture": "amd64", "os": "linux",
            "config": {"Labels": {
                "org.opencontainers.image.revision": "abc123-dirty",
                "org.opencontainers.image.version": "v0.0.0-local.gabc123.dirty"}}
        }))
        .unwrap();
        let cd = put(&config);
        let digests: Vec<String> = layers.iter().map(|l| put(l)).collect();
        let manifest = serde_json::to_vec(&json!({
            "schemaVersion": 2,
            "config": {"digest": cd, "size": config.len()},
            "layers": layers.iter().zip(&digests).map(|(l, d)| json!({"digest": d, "size": l.len()})).collect::<Vec<_>>()
        }))
        .unwrap();
        let md = put(&manifest);
        std::fs::write(dir.join("oci-layout"), r#"{"imageLayoutVersion":"1.0.0"}"#).unwrap();
        std::fs::write(
            dir.join("index.json"),
            serde_json::to_vec(&json!({"schemaVersion": 2, "manifests": [{
                "digest": md, "size": manifest.len(),
                "annotations": {"io.containerd.image.name": name},
                "platform": {"architecture": "amd64", "os": "linux"}}]}))
            .unwrap(),
        )
        .unwrap();
        digests
    }

    #[test]
    fn a_layout_is_read_as_the_one_image_it_names() {
        let dir = tempfile::tempdir().unwrap();
        layout(
            dir.path(),
            "hanzo.local/hanzo-inc/cloud:0123456789abcdef",
            &[b"runtime", b"apps", b"host"],
        );
        let image = read_layout(dir.path()).unwrap();
        assert_eq!(
            image.reference,
            "hanzo.local/hanzo-inc/cloud:0123456789abcdef"
        );
        assert_eq!(image.architecture, "amd64");
        assert_eq!(image.revision, "abc123-dirty");
        assert_eq!(image.version, "v0.0.0-local.gabc123.dirty");
        assert_eq!(image.layer_count(), 3);
        assert_eq!(image.size(), 7 + 4 + 4);
    }

    /// A local build is never a registry's name: a layout that names one is not
    /// loaded as if it were local, and a directory that is no layout says so.
    #[test]
    fn what_is_not_a_local_build_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        layout(dir.path(), "ghcr.io/hanzoai/cloud-oss:main", &[b"x"]);
        let e = read_layout(dir.path()).unwrap_err().to_string();
        assert!(e.contains("named under hanzo.local/"), "{e}");

        let empty = tempfile::tempdir().unwrap();
        let e = format!("{:#}", read_layout(empty.path()).unwrap_err());
        assert!(e.contains("is not an OCI image layout"), "{e}");
        assert!(e.contains("has the local build run?"), "{e}");
    }

    #[test]
    fn a_layout_missing_a_layer_is_refused_before_anything_is_sent() {
        let dir = tempfile::tempdir().unwrap();
        let digests = layout(dir.path(), "hanzo.local/x:1", &[b"runtime", b"host"]);
        std::fs::remove_file(blob_path(dir.path(), &digests[1])).unwrap();
        let e = read_layout(dir.path()).unwrap_err().to_string();
        assert!(e.contains("the layout is incomplete"), "{e}");
    }

    #[test]
    fn local_names_and_their_repositories() {
        assert!(is_local("hanzo.local/hanzo-inc/cloud:abc"));
        assert!(!is_local("hanzo.localhost/x:1"));
        assert!(!is_local("ghcr.io/hanzo.local/x:1"));
        assert_eq!(
            repo_of("hanzo.local/hanzo-inc/cloud:abc"),
            "hanzo.local/hanzo-inc/cloud"
        );
        assert_eq!(repo_of("localhost:5000/x"), "localhost:5000/x");
    }

    fn names(archive: &[u8]) -> Vec<String> {
        tar::Archive::new(archive)
            .entries()
            .unwrap()
            .map(|e| e.unwrap().path().unwrap().to_string_lossy().into_owned())
            .collect()
    }

    /// What makes the second load small: the node says what it holds, and only the
    /// layers it lacks are in the archive. The documents always are, because they
    /// are what names the image.
    #[test]
    fn only_the_layers_the_node_lacks_are_sent() {
        let dir = tempfile::tempdir().unwrap();
        let digests = layout(
            dir.path(),
            "hanzo.local/x:1",
            &[b"runtime", b"apps", b"host"],
        );
        let image = read_layout(dir.path()).unwrap();
        let blob = |d: &str| format!("blobs/sha256/{}", d.trim_start_matches("sha256:"));

        let (all, sent) = archive(Vec::new(), &image, &HashSet::new(), |_, _| {}).unwrap();
        assert_eq!(sent, 3);
        let all = names(&all);
        assert_eq!(&all[..2], ["oci-layout", "index.json"]);
        assert_eq!(all.len(), 2 + 2 + 3);

        let have: HashSet<String> = digests[..2].iter().cloned().collect();
        let (some, sent) = archive(Vec::new(), &image, &have, |_, _| {}).unwrap();
        assert_eq!(sent, 1);
        let some = names(&some);
        assert_eq!(some.len(), 2 + 2 + 1);
        assert!(some.contains(&blob(&digests[2])));
        assert!(!some.contains(&blob(&digests[0])));
        assert!(some.contains(&blob(&image.manifest.digest)));
        assert!(some.contains(&blob(&image.config.digest)));
    }
}
