"""Exercise the standalone installer with fake GitHub answers and real archives."""
import hashlib
import io
import json
import os
from pathlib import Path
import subprocess
import tarfile
import tempfile
import unittest

INSTALLER = Path(__file__).resolve().parents[1] / "install.sh"
ARCHIVE = "hanzo-mcp-linux-amd64.tar.gz"

# A stand-in for curl that answers from files under $FIXTURES:
#   refs                       git's ref advertisement for the repo
#   releases/<tag>/<asset>     a published release asset
#   api-blocked                present: api.github.com answers 403, as it does
#                              once an address has spent its 60 anonymous calls
# Anything missing answers 404, and -f turns 4xx into exit 22, as curl does.
FAKE_CURL = r'''#!/usr/bin/env python3
import json, os, pathlib, sys
root = pathlib.Path(os.environ["FIXTURES"])
args = sys.argv[1:]
url = next(a for a in args if a.startswith("https://"))
headers = [args[i + 1] for i, a in enumerate(args) if a == "-H"]
with (root / "requests").open("a") as out: out.write(json.dumps([url, headers]) + "\n")
releases = root / "releases"
published = sorted((t.name, a.name) for t in releases.glob("*") for a in t.glob("*"))
def body():
    if url.startswith("https://api.github.com/"):
        if (root / "api-blocked").exists(): return None
        path = url.split("/repos/hanzoai/mcp/", 1)[1]
        if path.startswith("releases/tags/"):
            tag = path.rsplit("/", 1)[1]
            names = [(100 + i, n) for i, (t, n) in enumerate(published) if t == tag]
            if not names: return None
            return json.dumps({"tag_name": tag, "assets": [
                {"id": i, "name": n} for i, n in names]}, indent=2).encode()
        if path.startswith("releases/assets/"):
            tag, name = published[int(path.rsplit("/", 1)[1]) - 100]
            return (releases / tag / name).read_bytes()
        return None
    if url.startswith("https://github.com/hanzoai/mcp.git/info/refs"):
        refs = root / "refs"
        return refs.read_bytes() if refs.exists() else None
    prefix = "https://github.com/hanzoai/mcp/releases/download/"
    if url.startswith(prefix):
        path = root / "releases" / url[len(prefix):]
        return path.read_bytes() if path.is_file() else None
    return None
data = body()
if data is None: sys.exit(22)
if "-I" in args: sys.exit(0)
if "-o" in args: pathlib.Path(args[args.index("-o") + 1]).write_bytes(data)
else: sys.stdout.buffer.write(data)
'''


def pkt(line):
    return f"{len(line) + 4:04x}{line}"


class InstallerTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name)
        self.bin = self.root / "bin"
        self.bin.mkdir()
        self.prefix = self.root / "installed"
        self.env = {
            **os.environ, "PATH": f"{self.bin}:{os.environ['PATH']}",
            "FIXTURES": str(self.root), "HANZO_INSTALL_REPO": "hanzoai/mcp",
            "HANZO_INSTALL_BIN": "hanzo-mcp", "HANZO_INSTALL_ALIAS": "mcp",
            "HANZO_INSTALL_PREFIX": str(self.prefix), "HANZO_VERSION": "",
            "HANZO_INSTALL_TOKEN": "", "GH_TOKEN": "", "GITHUB_TOKEN": "",
        }
        self.executable("gh", "#!/bin/sh\nexit 1\n")
        self.executable("uname", '#!/bin/sh\ncase "$1" in -s) echo Linux;; *) echo x86_64;; esac\n')
        self.executable("curl", FAKE_CURL)
        buffer = io.BytesIO()
        with tarfile.open(fileobj=buffer, mode="w:gz") as archive:
            contents = b"#!/bin/sh\necho native-mcp\n"
            member = tarfile.TarInfo("hanzo-mcp")
            member.size = len(contents)
            member.mode = 0o755
            archive.addfile(member, io.BytesIO(contents))
        self.archive = buffer.getvalue()
        self.checksum = f"{hashlib.sha256(self.archive).hexdigest()}  {ARCHIVE}\n"

    def executable(self, name, text):
        path = self.bin / name
        path.write_text(text)
        path.chmod(0o755)

    def tags(self, *tags):
        """Advertise refs the way github.com's smart HTTP does, peeled tags included."""
        sha = "a" * 40
        lines = [pkt("# service=git-upload-pack\n"), "0000",
                 pkt(f"{sha} HEAD\0multi_ack thin-pack side-band ofs-delta\n"),
                 pkt(f"{sha} refs/heads/main\n")]
        for tag in tags:
            lines += [pkt(f"{sha} refs/tags/{tag}\n"), pkt(f"{sha} refs/tags/{tag}^{{}}\n")]
        (self.root / "refs").write_text("".join(lines) + "0000")

    def release(self, tag, *targets, checksum=True):
        directory = self.root / "releases" / tag
        directory.mkdir(parents=True)
        for target in targets:
            name = f"hanzo-mcp-{target}.tar.gz"
            (directory / name).write_bytes(self.archive)
            if checksum:
                (directory / f"{name}.sha256").write_text(self.checksum.replace(ARCHIVE, name))

    def requests(self):
        path = self.root / "requests"
        return [json.loads(line) for line in path.read_text().splitlines()] if path.exists() else []

    def install(self):
        return subprocess.run(["sh", str(INSTALLER)], env=self.env,
                              capture_output=True, text=True, timeout=15)

    def test_an_exhausted_api_limit_does_not_stop_an_install(self):
        # 60 anonymous REST calls an hour per address, shared by everyone behind
        # the same NAT: the anonymous install path makes none.
        (self.root / "api-blocked").touch()
        self.tags("v1.0.0")
        self.release("v1.0.0", "linux-amd64")
        result = self.install()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("v1.0.0 linux-amd64", result.stdout)
        self.assertEqual([u for u, _ in self.requests() if "api.github.com" in u], [])

    def test_semver_beats_tag_order_and_skips_other_products(self):
        # MCP publishes JavaScript versions under v* in the same repository; they
        # carry no native archive.
        self.tags("rust-v1.9.9", "v8.0.0", "rust-v1.10.2", "rust-v2.0.0", "v9.0.0-beta.1")
        for tag in ("rust-v1.9.9", "rust-v1.10.2"):
            self.release(tag, "linux-amd64")
        result = self.install()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("rust-v1.10.2 linux-amd64", result.stdout)
        self.assertEqual(subprocess.check_output([self.prefix / "mcp"], text=True), "native-mcp\n")

    def test_newest_release_without_this_platform_is_skipped(self):
        # hanzoai/cli v8.5.158 shipped darwin and linux-arm64 only, and hanzoai/mcp
        # rust-v1.1.23 linux only: the newest release is not the newest one a given
        # machine can install, and a half-uploaded release has a tarball and no sum.
        self.tags("v3.0.0", "v2.0.0", "v1.0.0")
        self.release("v3.0.0", "darwin-arm64", "linux-arm64")
        self.release("v2.0.0", "linux-amd64", checksum=False)
        self.release("v1.0.0", "linux-amd64")
        result = self.install()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("v1.0.0 linux-amd64", result.stdout)

    def test_a_token_reads_a_private_repo_through_the_api(self):
        # A private repo answers 404 to anonymous refs and downloads, so the
        # token rides both: Basic on git's refs, Bearer on the API.
        self.env["GH_TOKEN"] = "ghp_fixture"
        self.tags("v2.0.0", "v1.0.0")
        self.release("v2.0.0", "darwin-arm64")
        self.release("v1.0.0", "linux-amd64")
        result = self.install()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("v1.0.0 linux-amd64", result.stdout)
        refs = [h for u, h in self.requests() if "/info/refs" in u]
        self.assertEqual(refs, [["Authorization: Basic eC1hY2Nlc3MtdG9rZW46Z2hwX2ZpeHR1cmU="]])
        self.assertEqual([u for u, _ in self.requests() if u.startswith("https://github.com/hanzoai/mcp/releases")], [])

    def test_explicit_pin_skips_release_selection(self):
        self.env["HANZO_VERSION"] = "rust-v1.0.0"
        self.release("rust-v1.0.0", "linux-amd64")
        result = self.install()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual([u for u, _ in self.requests() if "/info/refs" in u], [])

    def test_refuses_checksum_mismatch_before_installing(self):
        self.tags("v1.0.0")
        self.release("v1.0.0", "linux-amd64")
        (self.root / "releases" / "v1.0.0" / f"{ARCHIVE}.sha256").write_text(f"{'0' * 64}  {ARCHIVE}\n")
        result = self.install()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("checksum MISMATCH", result.stderr)
        self.assertFalse(self.prefix.exists())

    def test_no_stable_release_with_this_build_fails(self):
        self.tags("v2.0.0", "v1.0.0-rc.1")
        self.release("v1.0.0-rc.1", "linux-amd64")
        result = self.install()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("no stable release of hanzoai/mcp has a build for linux-amd64", result.stderr)

    def test_unreachable_refs_fail(self):
        result = self.install()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("could not list the tags of hanzoai/mcp", result.stderr)


if __name__ == "__main__":
    unittest.main()
