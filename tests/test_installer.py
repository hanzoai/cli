"""Exercise the standalone installer with release metadata and real archives."""
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


def release(tag, **fields):
    return {
        "tag_name": tag, "draft": False, "prerelease": False,
        "body": 'Release notes with {braces}, "tag_name": "v99.0.0" and a newline\n',
        "assets": [{"name": "hanzo-mcp-linux-amd64.tar.gz"}], **fields,
    }


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
        self.executable("curl", '''#!/usr/bin/env python3
import os, pathlib, sys
from urllib.parse import urlparse, parse_qs
root = pathlib.Path(os.environ["FIXTURES"])
args = sys.argv[1:]
url = next(a for a in args if a.startswith("https://"))
with (root / "requests").open("a") as out: out.write(url + "\\n")
parsed = urlparse(url)
if parsed.path.endswith("/releases"):
    name = "page-" + parse_qs(parsed.query)["page"][0] + ".json"
else:
    name = parsed.path.rsplit("/", 1)[1]
data = (root / name).read_bytes()
if "-o" in args: pathlib.Path(args[args.index("-o") + 1]).write_bytes(data)
else: sys.stdout.buffer.write(data)
''')
        self.archive = self.root / "hanzo-mcp-linux-amd64.tar.gz"
        with tarfile.open(self.archive, "w:gz") as archive:
            contents = b"#!/bin/sh\necho native-mcp\n"
            member = tarfile.TarInfo("hanzo-mcp")
            member.size = len(contents)
            member.mode = 0o755
            archive.addfile(member, io.BytesIO(contents))
        digest = hashlib.sha256(self.archive.read_bytes()).hexdigest()
        self.checksum = self.root / f"{self.archive.name}.sha256"
        self.checksum.write_text(f"{digest}  {self.archive.name}\n")

    def executable(self, name, text):
        path = self.bin / name
        path.write_text(text)
        path.chmod(0o755)

    def page(self, number, releases):
        (self.root / f"page-{number}.json").write_text(json.dumps(releases))

    def install(self):
        return subprocess.run(["sh", str(INSTALLER)], env=self.env,
                              capture_output=True, text=True, timeout=15)

    def test_semver_beats_publication_order_and_non_native_releases(self):
        self.page(1, [release("rust-v1.9.9"), release("v8.0.0", assets=[]),
                     release("rust-v1.10.2"), release("rust-v2.0.0", draft=True),
                     release("rust-v3.0.0", prerelease=True), release("v9.0.0-beta.1")])
        result = self.install()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("rust-v1.10.2 linux-amd64", result.stdout)
        self.assertEqual(subprocess.check_output([self.prefix / "mcp"], text=True), "native-mcp\n")

    def test_searches_all_pages(self):
        self.page(1, [release(f"v1.0.{n}") for n in range(100)])
        self.page(2, [release("v2.1.0")])
        result = self.install()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("v2.1.0 linux-amd64", result.stdout)

    def test_explicit_pin_skips_release_selection(self):
        self.env["HANZO_VERSION"] = "rust-v1.0.0"
        result = self.install()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertNotIn("per_page", (self.root / "requests").read_text())

    def test_refuses_checksum_mismatch_before_installing(self):
        self.page(1, [release("v1.0.0")])
        self.checksum.write_text(f"{'0' * 64}  {self.archive.name}\n")
        result = self.install()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("checksum MISMATCH", result.stderr)
        self.assertFalse(self.prefix.exists())

    def test_no_stable_native_release_fails(self):
        self.page(1, [release("v2.0.0", assets=[]), release("v1.0.0-rc.1")])
        result = self.install()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("stable native release", result.stderr)

    def test_truncated_metadata_fails(self):
        (self.root / "page-1.json").write_text('[{"tag_name":"v2.0.0"')
        result = self.install()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("invalid release metadata", result.stderr)


if __name__ == "__main__":
    unittest.main()
