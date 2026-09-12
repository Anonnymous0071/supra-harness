#!/usr/bin/env python3
"""Emit the canonical signed-update manifest for one release archive."""

import hashlib
import json
from pathlib import Path
import sys
import tarfile

MAX_ARCHIVE_BYTES = 128 * 1024 * 1024
MAX_EXECUTABLE_BYTES = 128 * 1024 * 1024


def fail(message: str) -> "NoReturn":
    raise SystemExit(f"generate-update-manifest: {message}")


def digest(path: Path) -> str:
    hasher = hashlib.sha256()
    with path.open("rb") as handle:
        while chunk := handle.read(1024 * 1024):
            hasher.update(chunk)
    return hasher.hexdigest()


def main() -> None:
    if len(sys.argv) != 4:
        fail("usage: generate-update-manifest.py <archive> <version> <target>")
    archive = Path(sys.argv[1])
    version = sys.argv[2]
    target = sys.argv[3]
    expected_name = f"supra-{target}.tar.gz"
    if archive.name != expected_name:
        fail(f"archive must be named {expected_name}")
    archive_size = archive.stat().st_size
    if not 0 < archive_size <= MAX_ARCHIVE_BYTES:
        fail("archive size is outside the updater bound")
    executable_name = "supra.exe" if "-windows-" in target else "supra"
    executable_path = f"supra-{target}/{executable_name}"
    with tarfile.open(archive, "r:gz") as bundle:
        members = bundle.getmembers()
        if len(members) != 1:
            fail("updater archive must contain exactly one member")
        member = members[0]
        if member.name != executable_path or not member.isfile():
            fail(f"only regular member {executable_path} is allowed")
        if not 0 < member.size <= MAX_EXECUTABLE_BYTES:
            fail("executable size is outside the updater bound")
        extracted = bundle.extractfile(member)
        if extracted is None:
            fail("executable member could not be read")
        executable = extracted.read(MAX_EXECUTABLE_BYTES + 1)
        if len(executable) != member.size:
            fail("executable member size changed while reading")
    manifest = {
        "archive": archive.name,
        "archive_sha256": digest(archive),
        "archive_size": archive_size,
        "executable": {
            "path": executable_path,
            "sha256": hashlib.sha256(executable).hexdigest(),
            "size": len(executable),
        },
        "schema": 1,
        "target": target,
        "version": version.removeprefix("v"),
    }
    # Sorted keys, compact separators, UTF-8, and one final LF are the
    # canonical bytes signed by scripts/sign-release.sh.
    output = archive.with_name(f"supra-{target}.manifest.json")
    output.write_text(json.dumps(manifest, sort_keys=True, separators=(",", ":")) + "\n", encoding="utf-8")
    print(output)


if __name__ == "__main__":
    main()
