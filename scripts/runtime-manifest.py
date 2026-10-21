#!/usr/bin/env python3
"""Bind the official package output to one Discodex worker release."""

import argparse
import hashlib
import json
from pathlib import Path
import subprocess
import tomllib


def digest(path: Path) -> str:
    with path.open("rb") as source:
        return hashlib.file_digest(source, "sha256").hexdigest()


def build_manifest(*, version, commit, target, archive, archive_sha256, package, files):
    return {
        "formatVersion": 1,
        "workerVersion": version,
        "codexCommit": commit,
        "target": target,
        "archive": archive.name,
        "sha256": archive_sha256,
        "files": {
            path.relative_to(package).as_posix(): checksum
            for path, checksum in sorted(files.items())
        },
    }


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--target", required=True)
    parser.add_argument("--package", type=Path, required=True)
    args = parser.parse_args()
    root = Path(__file__).resolve().parent.parent
    archive = root / f"discodex-runtime-{args.target}.tar.gz"
    with (root / "Cargo.toml").open("rb") as source:
        version = tomllib.load(source)["package"]["version"]
    commit = subprocess.check_output(
        ["git", "-C", str(root / "third_party/codex"), "rev-parse", "HEAD"],
        text=True,
    ).strip()
    package = args.package.resolve()
    manifest = build_manifest(
        version=version,
        commit=commit,
        target=args.target,
        archive=archive,
        archive_sha256=digest(archive),
        package=package,
        files={path: digest(path) for path in package.rglob("*") if path.is_file()},
    )
    (root / f"discodex-runtime-{args.target}.json").write_text(
        json.dumps(manifest, indent=2) + "\n", encoding="utf-8"
    )


if __name__ == "__main__":
    main()
