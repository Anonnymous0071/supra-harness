#!/usr/bin/env bash
# Package the signed-updater bundle for one release target.
set -euo pipefail

target=${1:?usage: package.sh <target-triple> [version]}
version=${2:-$(sed -n '/^\[workspace\.package\]/,/^\[/s/^version = "\([^"]*\)"/\1/p' Cargo.toml)}
case "$target" in
    *-windows-*) binary="supra.exe" ;;
    *) binary="supra" ;;
esac

cargo build --locked --release -p supra_cli --target "$target"
source_binary="target/${target}/release/${binary}"
[ -f "$source_binary" ] || { echo "package: missing $source_binary" >&2; exit 1; }
mkdir -p dist
name="supra-${target}"

# The updater's archive contract deliberately contains one regular executable
# and nothing else. Documentation and licences remain release-side assets rather
# than unsigned extraction surface.
stage=$(mktemp -d)
trap 'rm -rf "$stage"' EXIT
mkdir -p "$stage/$name"
cp "$source_binary" "$stage/$name/$binary"
chmod 755 "$stage/$name/$binary"
archive="dist/${name}.tar.gz"
epoch=${SOURCE_DATE_EPOCH:-0}
tar_cmd=tar
if [[ "$(uname -s)" == Darwin ]]; then
    command -v gtar >/dev/null 2>&1 || { echo "package: need gtar for deterministic archives" >&2; exit 1; }
    tar_cmd=gtar
fi
"$tar_cmd" --sort=name --mtime="@${epoch}" --owner=0 --group=0 --numeric-owner \
    -czf "$archive" -C "$stage" "$name/$binary"

python3 scripts/generate-update-manifest.py "$archive" "$version" "$target"
(
    cd dist
    sha256sum "$(basename "$archive")" > "$(basename "$archive").sha256"
)
echo "packaged $archive and dist/${name}.manifest.json"
