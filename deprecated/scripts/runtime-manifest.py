#!/usr/bin/env python3
"""Bind the official package output to one Discodex worker release."""

import argparse
import hashlib
import json
import os
from pathlib import Path, PurePosixPath, PureWindowsPath
import tarfile
import subprocess
import tomllib

ROOT = Path(__file__).resolve().parent.parent


def product_resource():
    return json.loads((ROOT / "resources/product.json").read_text(encoding="utf-8"))


def platform_resource(target: str):
    product = product_resource()
    for platform in product["platforms"]:
        if platform["target"] == target:
            return product, platform
    raise ValueError("runtime manifest target is not in the product platform catalog")


def digest(path: Path) -> str:
    with path.open("rb") as source:
        return hashlib.file_digest(source, "sha256").hexdigest()


def build_manifest(*, version, commit, target, archive, archive_sha256, package, files):
    product, _platform = platform_resource(target)
    return {
        "formatVersion": product["product"]["runtimeManifestFormatVersion"],
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


def package_files(package: Path) -> dict[str, str]:
    return {
        path.relative_to(package).as_posix(): digest(path)
        for path in sorted(package.rglob("*"))
        if path.is_file()
    }


def archive_entries(
    source: tarfile.TarFile,
) -> list[tuple[tarfile.TarInfo, PurePosixPath]]:
    """Validate archive member values and return their normalized paths."""
    entries: list[tuple[tarfile.TarInfo, PurePosixPath]] = []
    seen: set[str] = set()
    for member in source:
        name = member.name
        path = PurePosixPath(name)
        windows_path = PureWindowsPath(name)
        if (
            not name
            or "\\" in name
            or path.is_absolute()
            or windows_path.root
            or windows_path.drive
            or any(part == ".." for part in name.split("/"))
            or path.as_posix() == "."
            or path.as_posix() in seen
            or not (member.isfile() or member.isdir())
        ):
            raise ValueError(f"invalid runtime archive entry: {name!r}")
        seen.add(path.as_posix())
        entries.append((member, path))
    return entries


def extract_archive(archive: Path, package: Path) -> None:
    """Extract regular package files without accepting links or path escapes."""
    with tarfile.open(archive, "r:gz") as source:
        entries = archive_entries(source)
        package.mkdir(parents=True, exist_ok=False)
        for member, path in entries:
            destination = package.joinpath(*path.parts)
            destination.parent.mkdir(parents=True, exist_ok=True)
            if member.isdir():
                destination.mkdir(exist_ok=True)
                continue
            stream = source.extractfile(member)
            if stream is None:
                raise ValueError(f"runtime archive file has no contents: {member.name!r}")
            with stream, destination.open("xb") as output:
                output.write(stream.read())
            try:
                os.chmod(destination, member.mode & 0o777)
            except OSError as error:
                raise OSError(
                    error.errno,
                    f"set runtime archive permissions for {destination}: {error.strerror}",
                    destination,
                ) from error


def rebind_manifest_values(
    *, source, version, commit, target, archive_name, actual_archive_sha256, actual_files
):
    """Validate runtime release values and bind the archive to this worker."""
    product, platform = platform_resource(target)
    metadata = product["product"]
    expected_archive = metadata["runtimeArchiveTemplate"].replace("{target}", target)
    if source.get("formatVersion") != metadata["runtimeManifestFormatVersion"]:
        raise ValueError("unsupported runtime manifest format")
    old_version = source.get("workerVersion")
    if not isinstance(old_version, str) or not old_version:
        raise ValueError("runtime manifest has no worker version")
    if source.get("codexCommit") != commit:
        raise ValueError("runtime manifest Codex commit differs from this worker's pin")
    if (
        not isinstance(commit, str)
        or len(commit) != 40
        or any(char not in "0123456789abcdefABCDEF" for char in commit)
    ):
        raise ValueError("this worker has no full Codex commit pin")
    if source.get("target") != target:
        raise ValueError("runtime manifest target differs from this build target")
    if source.get("archive") != expected_archive or archive_name != expected_archive:
        raise ValueError("runtime archive name differs from this build target")
    archive_sha256 = source.get("sha256")
    if (
        not isinstance(archive_sha256, str)
        or len(archive_sha256) != 64
        or any(char not in "0123456789abcdefABCDEF" for char in archive_sha256)
        or archive_sha256.lower() != actual_archive_sha256
    ):
        raise ValueError("runtime archive checksum does not match its manifest")
    files = source.get("files")
    if not isinstance(files, dict):
        raise ValueError("runtime manifest has no package file checksums")
    for name, checksum in files.items():
        path = PurePosixPath(name) if isinstance(name, str) else PurePosixPath("/")
        if (
            not isinstance(name, str)
            or not name
            or "\\" in name
            or path.is_absolute()
            or any(part in ("", ".", "..") for part in name.split("/"))
            or name == metadata["runtimeManifestFile"]
        ):
            raise ValueError("runtime manifest contains an invalid package path")
        if (
            not isinstance(checksum, str)
            or len(checksum) != 64
            or any(char not in "0123456789abcdefABCDEF" for char in checksum)
        ):
            raise ValueError("runtime manifest contains an invalid package checksum")
    extension = platform["extension"]
    required = {
        path.replace("{extension}", extension)
        for path in product["runtime"]["commonFiles"]
    }
    required.update(product["runtime"]["platformFiles"].get(platform["os"], []))
    if not required.issubset(files):
        raise ValueError("runtime manifest is missing required package components")
    if files != actual_files:
        raise ValueError("runtime package files do not match their manifest checksums")
    return {**source, "workerVersion": version}


def rebind_manifest(*, source, version, commit, target, archive, package):
    """Validate a prior runtime release and bind its identical archive to this worker."""
    return rebind_manifest_values(
        source=source,
        version=version,
        commit=commit,
        target=target,
        archive_name=archive.name,
        actual_archive_sha256=digest(archive),
        actual_files=package_files(package),
    )


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--target", required=True)
    parser.add_argument("--package", type=Path, required=True)
    parser.add_argument("--reuse-manifest", type=Path)
    parser.add_argument("--extract-archive", type=Path)
    args = parser.parse_args()
    root = ROOT
    product = product_resource()
    metadata = product["product"]
    archive_name = metadata["runtimeArchiveTemplate"].replace("{target}", args.target)
    archive = root / archive_name
    with (root / "Cargo.toml").open("rb") as source:
        version = tomllib.load(source)["package"]["version"]
    commit = subprocess.check_output(
        ["git", "-C", str(root / "third_party/codex"), "rev-parse", "HEAD"],
        text=True,
    ).strip()
    package = args.package.resolve()
    if args.extract_archive:
        extract_archive(args.extract_archive.resolve(), package)
    if args.reuse_manifest:
        source = json.loads(args.reuse_manifest.read_text(encoding="utf-8"))
        manifest = rebind_manifest(
            source=source,
            version=version,
            commit=commit,
            target=args.target,
            archive=archive,
            package=package,
        )
    else:
        manifest = build_manifest(
            version=version,
            commit=commit,
            target=args.target,
            archive=archive,
            archive_sha256=digest(archive),
            package=package,
            files={package / path: checksum for path, checksum in package_files(package).items()},
        )
    manifest_name = metadata["runtimeManifestAssetTemplate"].replace("{target}", args.target)
    (root / manifest_name).write_text(
        json.dumps(manifest, indent=2) + "\n", encoding="utf-8"
    )


if __name__ == "__main__":
    main()
