import importlib.util
import io
import json
from pathlib import Path
import tarfile
import tempfile
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

    def make_runtime(self, root):
        package = root / "package"
        package.mkdir()
        contents = {
            "bin/codex": b"codex executable",
            "bin/codex-code-mode-host": b"host executable",
            "codex-path/rg": b"ripgrep executable",
            "codex-package.json": b'{"name":"fixture"}',
            "codex-resources/bwrap": b"sandbox executable",
        }
        for name, data in contents.items():
            path = package / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(data)
        archive = root / "discodex-runtime-x86_64-unknown-linux-gnu.tar.gz"
        with tarfile.open(archive, "w:gz") as output:
            for name in contents:
                output.add(package / name, arcname=name)
        files = MODULE.package_files(package)
        return package, archive, {
            "formatVersion": 1,
            "workerVersion": "1.1.0",
            "codexCommit": "a" * 40,
            "target": "x86_64-unknown-linux-gnu",
            "archive": archive.name,
            "sha256": MODULE.digest(archive),
            "files": files,
        }

    def test_reuse_validates_archive_and_package_then_changes_only_worker_version(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            package, archive, source = self.make_runtime(root)
            archive_bytes = archive.read_bytes()
            output = MODULE.rebind_manifest(
                source=source,
                version="1.1.1",
                commit="a" * 40,
                target="x86_64-unknown-linux-gnu",
                archive=archive,
                package=package,
            )
            expected = dict(source, workerVersion="1.1.1")
            self.assertEqual(output, expected)
            self.assertEqual(archive.read_bytes(), archive_bytes)

    def test_reuse_rejects_core_and_target_mismatches(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            package, archive, source = self.make_runtime(root)
            with self.assertRaisesRegex(ValueError, "Codex commit"):
                MODULE.rebind_manifest(
                    source=source,
                    version="1.1.1",
                    commit="b" * 40,
                    target="x86_64-unknown-linux-gnu",
                    archive=archive,
                    package=package,
                )
            with self.assertRaisesRegex(ValueError, "target"):
                MODULE.rebind_manifest(
                    source=source,
                    version="1.1.1",
                    commit="a" * 40,
                    target="aarch64-unknown-linux-gnu",
                    archive=archive,
                    package=package,
                )

    def test_reuse_rejects_archive_and_package_hash_mismatches(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            package, archive, source = self.make_runtime(root)
            archive.write_bytes(archive.read_bytes() + b"tampered")
            with self.assertRaisesRegex(ValueError, "archive checksum"):
                MODULE.rebind_manifest(
                    source=source,
                    version="1.1.1",
                    commit="a" * 40,
                    target="x86_64-unknown-linux-gnu",
                    archive=archive,
                    package=package,
                )
            archive.write_bytes(b"restored archive bytes")
            source["sha256"] = MODULE.digest(archive)
            (package / "bin/codex").write_bytes(b"tampered package file")
            with self.assertRaisesRegex(ValueError, "package files"):
                MODULE.rebind_manifest(
                    source=source,
                    version="1.1.1",
                    commit="a" * 40,
                    target="x86_64-unknown-linux-gnu",
                    archive=archive,
                    package=package,
                )

    def test_extract_archive_and_reuse_manifest_fixture(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            package, archive, source = self.make_runtime(root)
            extracted = root / "extracted"
            MODULE.extract_archive(archive, extracted)
            output = MODULE.rebind_manifest(
                source=json.loads(json.dumps(source)),
                version="1.1.1",
                commit="a" * 40,
                target="x86_64-unknown-linux-gnu",
                archive=archive,
                package=extracted,
            )
            self.assertEqual(output["workerVersion"], "1.1.1")
            self.assertEqual((extracted / "bin/codex").read_bytes(), b"codex executable")

    def test_extract_archive_rejects_path_escape(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            for index, name in enumerate(("../outside", "/outside", "C:/outside")):
                archive = root / f"bad-{index}.tar.gz"
                with tarfile.open(archive, "w:gz") as output:
                    data = b"escape"
                    member = tarfile.TarInfo(name)
                    member.size = len(data)
                    output.addfile(member, io.BytesIO(data))
                with self.assertRaises(ValueError):
                    MODULE.extract_archive(archive, root / f"package-{index}")
            self.assertFalse((root / "outside").exists())
