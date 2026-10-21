import importlib.util
from pathlib import Path
import unittest


SCRIPT = Path(__file__).with_name("runtime-manifest.py")
SPEC = importlib.util.spec_from_file_location("runtime_manifest", SCRIPT)
MODULE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(MODULE)


class RuntimeManifestTests(unittest.TestCase):
    def test_manifest_binds_release_and_contains_only_relative_package_paths(self):
        package = Path("build-root/private-package")
        files = {
            package / "bin/codex": "c" * 64,
            package / "bin/codex-code-mode-host": "d" * 64,
        }
        original_files = dict(files)
        manifest = MODULE.build_manifest(
            version="1.0.0",
            commit="a" * 40,
            target="x86_64-unknown-linux-gnu",
            archive=Path("build-root/discodex-runtime-x86_64-unknown-linux-gnu.tar.gz"),
            archive_sha256="b" * 64,
            package=package,
            files=files,
        )
        self.assertEqual(manifest, {
            "formatVersion": 1,
            "workerVersion": "1.0.0",
            "codexCommit": "a" * 40,
            "target": "x86_64-unknown-linux-gnu",
            "archive": "discodex-runtime-x86_64-unknown-linux-gnu.tar.gz",
            "sha256": "b" * 64,
            "files": {
                "bin/codex": "c" * 64,
                "bin/codex-code-mode-host": "d" * 64,
            },
        })
        self.assertEqual(files, original_files)
