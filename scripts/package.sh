#!/usr/bin/env bash
# Package the supra binary into dist/ for a release target.
#
# Produces the raw binary plus a .tar.gz carrying the binary, README,
# LICENSE files, and a shell completion stub — the tarball the
# Homebrew formula and the install script consume. .deb/.rpm come from
# scripts/package-os.sh, which runs after this script in the release job.
set -euo pipefail

target=${1:?usage: package.sh <target-triple>}

case "$target" in
    *-windows-*) binary="supra.exe" ;;
    *) binary="supra" ;;
esac

cargo build --locked --release -p supra_cli --target "$target"
mkdir -p dist
name="supra-${target}"
cp "target/${target}/release/${binary}" "dist/${name}${binary#supra}"
(
    cd dist
    sha256sum "${name}${binary#supra}" > "${name}${binary#supra}.sha256"
)
echo "packaged dist/${name}${binary#supra}"

stage=$(mktemp -d)
trap 'rm -rf "$stage"' EXIT
mkdir -p "$stage/$name"
cp "target/${target}/release/${binary}" "$stage/$name/$binary"
cp README.md CHANGELOG.md LICENSE-MIT LICENSE-APACHE "$stage/$name/"
"target/${target}/release/${binary}" --help > "$stage/$name/supra.txt"

archive="dist/${name}.tar.gz"
if [[ "$target" == *-windows-* ]]; then
    archive="dist/${name}.zip"
    python3 - "$stage" "$name" "$archive" <<'PY'
import os
from pathlib import Path
import sys
import zipfile

stage, name, archive = map(Path, sys.argv[1:])
epoch = int(os.environ.get("SOURCE_DATE_EPOCH", "315532800"))
# ZIP timestamps cannot represent dates before 1980.
when = __import__("time").gmtime(max(epoch, 315532800))[:6]
with zipfile.ZipFile(archive, "w", compression=zipfile.ZIP_DEFLATED) as output:
    for path in sorted((stage / name).rglob("*")):
        if not path.is_file():
            continue
        info = zipfile.ZipInfo(str(path.relative_to(stage)).replace(os.sep, "/"), when)
        info.external_attr = (0o755 if path.name.endswith(".exe") else 0o644) << 16
        info.compress_type = zipfile.ZIP_DEFLATED
        output.writestr(info, path.read_bytes())
PY
else
    epoch=${SOURCE_DATE_EPOCH:-0}
    tar_cmd=tar
    if [[ "$(uname -s)" == Darwin ]]; then
        command -v gtar >/dev/null 2>&1 || {
            echo "package: need gtar for deterministic macOS archives" >&2
            exit 1
        }
        tar_cmd=gtar
    fi
    "$tar_cmd" --sort=name --mtime="@${epoch}" --owner=0 --group=0 --numeric-owner \
        -czf "$archive" -C "$stage" "$name"
fi
(
    cd dist
    sha256sum "$(basename "$archive")" > "$(basename "$archive").sha256"
)
echo "packaged $archive"
