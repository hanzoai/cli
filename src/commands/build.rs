//! `hanzo build` — a container image, built by BuildKit inside a hanzo-vm microVM.
//!
//! One build, one kernel. Every build boots its own vm from the
//! `buildkit-<version>` checkpoint, starts buildkitd in it, and drives it from
//! the host with buildctl, BuildKit's own client, over a loopback forward. The
//! context, secrets and registry credentials stay on the host; buildkitd asks
//! for them through the session. The vm stops when its stdin closes, so no exit
//! of this process leaves one running. There is no docker and no dockerd.

use anyhow::{anyhow, bail, Context, Result};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::commands::{launch, vm};
use crate::image;

/// The BuildKit release both halves run: buildctl on the host, buildkitd in the vm.
const BUILDKIT: &str = "0.33.0";

/// sha256 of each release tarball this CLI fetches, by BuildKit's platform name.
const TARBALLS: [(&str, &str); 3] = [
    (
        "darwin-arm64",
        "730ac4ffd6f4a88dc404fc675aeaf4cfee414915036042847f0a60861cc8790c",
    ),
    (
        "linux-amd64",
        "b6242896d343100808dcbe37565caf381e0a444a6a83d7255926bb1519248ead",
    ),
    (
        "linux-arm64",
        "e5acfb5929f967fde3b925ddb39f79fd481a0e96774c641fab3a0e83950d7bfa",
    ),
];

/// Where buildkitd listens and logs, inside the guest, and where the build's
/// certificate and key live: tmpfs, so neither reaches the vm's disk.
const GUEST_PORT: u16 = 1234;
const DAEMON_LOG: &str = "/var/log/buildkitd.log";
const GUEST_CERT: &str = "/tmp/buildkit.crt";
const GUEST_KEY: &str = "/tmp/buildkit.key";

/// How long buildkitd has to answer through the forward.
const READY_TIMEOUT: Duration = Duration::from_secs(120);

#[derive(clap::Args)]
pub struct Args {
    /// The build context
    #[arg(default_value = ".")]
    pub context: PathBuf,

    /// The Dockerfile [default: <CONTEXT>/Dockerfile]
    #[arg(short, long, value_name = "FILE")]
    pub file: Option<PathBuf>,

    /// Name the image, as registry/repository:tag (repeatable)
    #[arg(short = 't', long = "tag", value_name = "REF")]
    pub tags: Vec<String>,

    /// Push the image to its registry
    #[arg(long)]
    pub push: bool,

    /// Write the image to FILE as an OCI archive
    #[arg(short, long, value_name = "FILE")]
    pub output: Option<PathBuf>,

    /// Target platforms, e.g. linux/amd64,linux/arm64 [default: this machine's]
    #[arg(long, value_delimiter = ',')]
    pub platform: Vec<String>,

    /// A build argument (repeatable)
    #[arg(long = "build-arg", value_name = "KEY=VALUE")]
    pub build_args: Vec<String>,

    /// The Dockerfile stage to build
    #[arg(long)]
    pub target: Option<String>,

    /// A secret for `RUN --mount=type=secret`: id=NAME,env=VAR or id=NAME,src=FILE
    #[arg(long, value_name = "SPEC")]
    pub secret: Vec<String>,

    /// CPUs for the vm
    #[arg(long, default_value_t = cores())]
    pub cpus: u32,

    /// Memory for the vm, in MB
    #[arg(long, default_value_t = 8192)]
    pub memory: u64,

    /// Disk for the vm, in MB
    #[arg(long, default_value_t = 32768)]
    pub disk_size: u64,
}

fn cores() -> u32 {
    std::thread::available_parallelism().map_or(4, |n| n.get() as u32)
}

/// Boot the vm, start buildkitd, wait for it to answer, build.
pub async fn run(args: Args) -> Result<()> {
    if args.push && args.tags.is_empty() {
        bail!("--push needs a name: -t registry/repository:tag");
    }
    let build = argv(&args)?;
    let bin = vm::resolve_or_install().await?;
    let buildctl = client().await?;
    let checkpoint = format!("buildkit-{BUILDKIT}");
    vm::ensure_checkpoint(&bin, &checkpoint, &install()?)?;

    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .and_then(|l| l.local_addr())
        .context("choosing a local port for buildkitd")?
        .port();
    let log = home()?.join("vm.log");
    std::fs::File::create(&log).with_context(|| format!("creating {}", log.display()))?;
    let boot = vm::run_args(
        args.cpus,
        args.memory,
        args.disk_size,
        &[(port, GUEST_PORT)],
        &checkpoint,
    );
    let mut rpc = vm::Rpc::start(&bin, &boot, &log)?;
    rpc.wait_ready()?;

    // One build, one key. Both ends present this certificate and trust only it,
    // so whoever else shares the loopback cannot drive the daemon. It is made in
    // the guest and copied out over the wire.
    let make = format!(
        "openssl req -x509 -nodes -newkey ec -pkeyopt ec_paramgen_curve:prime256v1 \
         -subj /CN=hanzo-build -addext subjectAltName=IP:127.0.0.1 \
         -addext extendedKeyUsage=serverAuth,clientAuth \
         -not_before 20000101000000Z -not_after 21000101000000Z \
         -keyout {GUEST_KEY} -out {GUEST_CERT}"
    );
    let (_, err, code) = rpc.exec(&make.split_whitespace().collect::<Vec<_>>())?;
    if code != 0 {
        bail!("making the build's certificate: {}", err.trim());
    }
    let tls = tempfile::tempdir().context("creating a directory for the build's key")?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(tls.path(), std::fs::Permissions::from_mode(0o700))
            .context("making the key's directory owner-only")?;
    }
    let (host_cert, host_key) = (tls.path().join("cert.pem"), tls.path().join("key.pem"));
    std::fs::write(&host_cert, rpc.read_file(GUEST_CERT)?)
        .context("writing the build's certificate")?;
    std::fs::write(&host_key, rpc.read_file(GUEST_KEY)?).context("writing the build's key")?;
    rpc.spawn(&[
        "sh",
        "-c",
        &format!(
            "exec buildkitd --addr tcp://0.0.0.0:{GUEST_PORT} --tlscert {GUEST_CERT} \
             --tlskey {GUEST_KEY} --tlscacert {GUEST_CERT} >{DAEMON_LOG} 2>&1"
        ),
    ])?;

    let (cert, key) = (
        host_cert.display().to_string(),
        host_key.display().to_string(),
    );
    let addr = format!("tcp://127.0.0.1:{port}");
    let conn = [
        "--addr",
        &addr,
        "--tlscacert",
        &cert,
        "--tlscert",
        &cert,
        "--tlskey",
        &key,
    ]
    .map(String::from)
    .to_vec();
    ready(&buildctl, &conn, &mut rpc)?;
    launch::exec(&buildctl, &[conn, build].concat())
}

/// `buildctl debug workers` succeeds once buildkitd answers through the forward
/// with a worker to build on. A vm that exits, or a daemon that never answers,
/// is reported with what the daemon logged.
fn ready(buildctl: &Path, conn: &[String], rpc: &mut vm::Rpc) -> Result<()> {
    let deadline = Instant::now() + READY_TIMEOUT;
    loop {
        let answered = Command::new(buildctl)
            .args(conn)
            .args(["debug", "workers"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .with_context(|| format!("running {}", buildctl.display()))?
            .success();
        if answered {
            return Ok(());
        }
        if rpc.exited().is_some() || Instant::now() >= deadline {
            let said = rpc
                .read_file(DAEMON_LOG)
                .map(|b| String::from_utf8_lossy(&b).into_owned())
                .unwrap_or_default();
            bail!("buildkitd did not answer at {}\n{}", conn[1], said.trim());
        }
        std::thread::sleep(Duration::from_millis(250));
    }
}

/// buildctl's argv for this build, after `--addr`.
fn argv(a: &Args) -> Result<Vec<String>> {
    let file = a
        .file
        .clone()
        .unwrap_or_else(|| a.context.join("Dockerfile"));
    let name = file
        .file_name()
        .ok_or_else(|| anyhow!("{} names no file", file.display()))?
        .to_string_lossy();
    let dir = file
        .parent()
        .filter(|d| !d.as_os_str().is_empty())
        .unwrap_or(Path::new("."));

    let mut v: Vec<String> = ["build", "--frontend", "dockerfile.v0"]
        .map(String::from)
        .to_vec();
    let mut pair = |flag: &str, value: String| v.extend([flag.to_string(), value]);
    pair("--local", format!("context={}", a.context.display()));
    pair("--local", format!("dockerfile={}", dir.display()));
    pair("--opt", format!("filename={name}"));
    if !a.platform.is_empty() {
        pair("--opt", format!("platform={}", a.platform.join(",")));
    }
    if let Some(target) = &a.target {
        pair("--opt", format!("target={target}"));
    }
    for arg in &a.build_args {
        pair("--opt", format!("build-arg:{arg}"));
    }
    for secret in &a.secret {
        pair("--secret", secret.clone());
    }
    // Values are CSV fields to buildctl, so each is quoted: a name list carries
    // commas, and so can a path.
    let names = format!("\"name={}\"", a.tags.join(","));
    if a.push {
        pair("--output", format!("type=image,{names},push=true"));
    }
    if let Some(out) = &a.output {
        let named = if a.tags.is_empty() {
            String::new()
        } else {
            format!(",{names}")
        };
        pair(
            "--output",
            format!("type=oci,\"dest={}\"{named}", out.display()),
        );
    }
    Ok(v)
}

/// buildctl for this host, from the pinned release, verified before it is kept.
async fn client() -> Result<PathBuf> {
    let bin = home()?
        .join(format!("buildkit-{BUILDKIT}"))
        .join("buildctl");
    if bin.is_file() {
        return Ok(bin);
    }
    let os = match std::env::consts::OS {
        "macos" => "darwin",
        other => other,
    };
    let platform = format!("{os}-{}", image::architecture());
    let (asset, url, digest) = tarball(&platform)?;
    eprintln!(
        "installing buildctl v{BUILDKIT} ({platform}) → {} …",
        bin.display()
    );
    let bytes = vm::fetch(&reqwest::Client::new(), &url).await?;
    vm::verify_sha256(&bytes, digest, &asset)?;
    let dir = bin.parent().context("buildctl has no directory")?;
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    vm::extract(&bytes, "buildctl", &bin)?;
    Ok(bin)
}

/// What the checkpoint runs once in the base image: this architecture's
/// BuildKit into /usr/local, refused unless the tarball hashes to the pin.
fn install() -> Result<String> {
    let (asset, url, digest) = tarball(&format!("linux-{}", image::architecture()))?;
    Ok(format!(
        "set -e; cd /tmp; curl -fsSLo {asset} {url}; \
         echo '{digest}  {asset}' | sha256sum -c -; \
         tar -xzf {asset} -C /usr/local; rm {asset}"
    ))
}

/// A pinned release tarball: its file name, URL and sha256.
fn tarball(platform: &str) -> Result<(String, String, &'static str)> {
    let digest = TARBALLS
        .iter()
        .find(|(p, _)| *p == platform)
        .map(|(_, d)| *d)
        .ok_or_else(|| anyhow!("no BuildKit release is pinned for {platform}"))?;
    let asset = format!("buildkit-v{BUILDKIT}.{platform}.tar.gz");
    let url = format!("https://github.com/moby/buildkit/releases/download/v{BUILDKIT}/{asset}");
    Ok((asset, url, digest))
}

/// `~/.hanzo/build` — buildctl and the last vm's log.
fn home() -> Result<PathBuf> {
    let d = dirs::home_dir()
        .context("no home directory")?
        .join(".hanzo")
        .join("build");
    std::fs::create_dir_all(&d).with_context(|| format!("creating {}", d.display()))?;
    Ok(d)
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[derive(Parser)]
    struct Cli {
        #[command(flatten)]
        args: Args,
    }

    fn args(argv: &[&str]) -> Args {
        Cli::try_parse_from(std::iter::once("build").chain(argv.iter().copied()))
            .unwrap()
            .args
    }

    fn after<'a>(v: &'a [String], flag: &str) -> Vec<&'a str> {
        v.windows(2)
            .filter(|w| w[0] == flag)
            .map(|w| w[1].as_str())
            .collect()
    }

    #[test]
    fn a_bare_build_reads_the_dockerfile_in_its_context_and_exports_nothing() {
        let v = argv(&args(&[])).unwrap();
        assert_eq!(&v[..3], ["build", "--frontend", "dockerfile.v0"]);
        assert_eq!(after(&v, "--local"), ["context=.", "dockerfile=."]);
        assert_eq!(after(&v, "--opt"), ["filename=Dockerfile"]);
        assert!(after(&v, "--output").is_empty(), "{v:?}");
    }

    #[test]
    fn every_flag_reaches_buildctl() {
        let v = argv(&args(&[
            "app",
            "-f",
            "ops/Containerfile",
            "-t",
            "ghcr.io/hanzoai/app:1.2.3",
            "-t",
            "ghcr.io/hanzoai/app:main",
            "--push",
            "-o",
            "out dir/app.tar",
            "--platform",
            "linux/amd64,linux/arm64",
            "--target",
            "run",
            "--build-arg",
            "VERSION=1.2.3",
            "--secret",
            "id=GIT_AUTH_TOKEN,env=GIT_AUTH_TOKEN",
        ]))
        .unwrap();
        assert_eq!(after(&v, "--local"), ["context=app", "dockerfile=ops"]);
        assert_eq!(
            after(&v, "--opt"),
            [
                "filename=Containerfile",
                "platform=linux/amd64,linux/arm64",
                "target=run",
                "build-arg:VERSION=1.2.3"
            ]
        );
        assert_eq!(
            after(&v, "--secret"),
            ["id=GIT_AUTH_TOKEN,env=GIT_AUTH_TOKEN"]
        );
        assert_eq!(
            after(&v, "--output"),
            [
                "type=image,\"name=ghcr.io/hanzoai/app:1.2.3,ghcr.io/hanzoai/app:main\",push=true",
                "type=oci,\"dest=out dir/app.tar\",\"name=ghcr.io/hanzoai/app:1.2.3,ghcr.io/hanzoai/app:main\""
            ]
        );
    }

    #[tokio::test]
    async fn a_push_with_no_name_is_refused_before_anything_boots() {
        let err = run(args(&["--push"])).await.unwrap_err();
        assert!(err.to_string().contains("--push needs a name"), "{err}");
    }

    #[test]
    fn the_checkpoint_installs_this_architecture_and_checks_its_digest() {
        let cmd = install().unwrap();
        let platform = format!("linux-{}", image::architecture());
        let (_, _, digest) = tarball(&platform).unwrap();
        assert!(
            cmd.contains(&format!("buildkit-v{BUILDKIT}.{platform}.tar.gz")),
            "{cmd}"
        );
        assert!(
            cmd.contains(&format!("echo '{digest}  ")) && cmd.contains("sha256sum -c -"),
            "{cmd}"
        );
        assert!(
            cmd.find("sha256sum").unwrap() < cmd.find("tar -xzf").unwrap(),
            "verified before unpacked: {cmd}"
        );
    }

    #[test]
    fn every_pin_is_a_sha256() {
        for (platform, digest) in TARBALLS {
            assert_eq!(digest.len(), 64, "{platform}");
            assert!(digest.bytes().all(|b| b.is_ascii_hexdigit()), "{platform}");
        }
        assert!(tarball("windows-amd64").is_err());
    }
}
