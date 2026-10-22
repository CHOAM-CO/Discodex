import importlib.util
import hashlib
import io
from pathlib import Path
import tarfile
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

    def make_runtime(self):
        contents = {
            "bin/codex": b"codex executable",
            "bin/codex-code-mode-host": b"host executable",
            "codex-path/rg": b"ripgrep executable",
            "codex-package.json": b'{"name":"fixture"}',
            "codex-resources/bwrap": b"sandbox executable",
        }
        archive_output = io.BytesIO()
        with tarfile.open(fileobj=archive_output, mode="w:gz") as output:
            for name in contents:
                member = tarfile.TarInfo(name)
                member.mode = 0o755
                member.size = len(contents[name])
                output.addfile(member, io.BytesIO(contents[name]))
        archive = archive_output.getvalue()
        files = {
            name: hashlib.sha256(contents[name]).hexdigest()
            for name in contents
        }
        return archive, contents, {
            "formatVersion": 1,
            "workerVersion": "1.1.0",
            "codexCommit": "a" * 40,
            "target": "x86_64-unknown-linux-gnu",
            "archive": "discodex-runtime-x86_64-unknown-linux-gnu.tar.gz",
            "sha256": hashlib.sha256(archive).hexdigest(),
            "files": files,
        }

    def archive_entries(self, archive):
        with tarfile.open(fileobj=io.BytesIO(archive), mode="r:gz") as source:
            return [
                (member.name, path.as_posix(), member.isfile(), member.isdir(), member.mode & 0o777)
                for member, path in MODULE.archive_entries(source)
            ]

    def rebind(self, source, archive, files=None, *, commit="a" * 40, target="x86_64-unknown-linux-gnu"):
        return MODULE.rebind_manifest_values(
            source=source,
            version="1.1.1",
            commit=commit,
            target=target,
            archive_name=f"discodex-runtime-{target}.tar.gz",
            actual_archive_sha256=hashlib.sha256(archive).hexdigest(),
            actual_files=source["files"] if files is None else files,
        )

    def test_reuse_validates_archive_and_package_then_changes_only_worker_version(self):
        archive, _, source = self.make_runtime()
        original = dict(source)
        output = self.rebind(source, archive)
        expected = dict(source, workerVersion="1.1.1")
        self.assertEqual(output, expected)
        self.assertEqual(source, original)

    def test_reuse_rejects_core_and_target_mismatches(self):
        archive, _, source = self.make_runtime()
        with self.assertRaises(ValueError) as caught:
            self.rebind(source, archive, commit="b" * 40)
        self.assertEqual(
            str(caught.exception),
            "runtime manifest Codex commit differs from this worker's pin",
        )
        with self.assertRaises(ValueError) as caught:
            self.rebind(source, archive, target="aarch64-unknown-linux-gnu")
        self.assertEqual(
            str(caught.exception),
            "runtime manifest target differs from this build target",
        )

    def test_reuse_rejects_archive_and_package_hash_mismatches(self):
        archive, contents, source = self.make_runtime()
        tampered_archive = archive + b"tampered"
        with self.assertRaises(ValueError) as caught:
            self.rebind(source, tampered_archive)
        self.assertEqual(
            str(caught.exception),
            "runtime archive checksum does not match its manifest",
        )
        tampered_files = dict(source["files"])
        tampered_files["bin/codex"] = hashlib.sha256(b"tampered package file").hexdigest()
        self.assertNotEqual(tampered_files["bin/codex"], hashlib.sha256(contents["bin/codex"]).hexdigest())
        with self.assertRaises(ValueError) as caught:
            self.rebind(source, archive, files=tampered_files)
        self.assertEqual(
            str(caught.exception),
            "runtime package files do not match their manifest checksums",
        )

    def test_archive_member_values_match_manifest_package_checksums(self):
        archive, contents, source = self.make_runtime()
        entries = self.archive_entries(archive)
        self.assertEqual([entry[1] for entry in entries], list(contents))
        self.assertTrue(all(entry[2] and not entry[3] for entry in entries))
        extracted_files = {
            entry[1]: hashlib.sha256(contents[entry[1]]).hexdigest()
            for entry in entries
        }
        self.assertEqual(extracted_files, source["files"])
        output = self.rebind(source, archive, files=extracted_files)
        self.assertEqual(output["workerVersion"], "1.1.1")

    def test_archive_entries_reject_path_escape_values(self):
        for name in ("../outside", "/outside", "C:/outside"):
            archive_output = io.BytesIO()
            with tarfile.open(fileobj=archive_output, mode="w:gz") as output:
                data = b"escape"
                member = tarfile.TarInfo(name)
                member.size = len(data)
                output.addfile(member, io.BytesIO(data))
            with self.assertRaises(ValueError) as caught:
                self.archive_entries(archive_output.getvalue())
            self.assertEqual(
                str(caught.exception),
                f"invalid runtime archive entry: {name!r}",
            )
