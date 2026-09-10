#!/usr/bin/env bash
# Package the supra binary into dist/ for a release target.
#
# Produces the raw binary plus a .tar.gz carrying the binary, README,
# LICENSE files, and a shell completion stub — the tarball the
# Homebrew formula and the install script consume. .deb/.rpm come from
# scripts/package-os.sh, which runs after this script in the release job.
set -euo pipefail

target=${1:?usage: package.sh <target-triple>}

cargo build --locked --release -p supra_cli --target "$target"
mkdir -p dist
name="supra-${target}"
cp "target/${target}/release/supra" "dist/${name}"
sha256sum "dist/${name}" > "dist/${name}.sha256"
echo "packaged dist/${name}"

stage=$(mktemp -d)
trap 'rm -rf "$stage"' EXIT
mkdir -p "$stage/$name"
cp "target/${target}/release/supra" "$stage/$name/supra"
cp README.md CHANGELOG.md LICENSE-MIT LICENSE-APACHE "$stage/$name/"
"target/${target}/release/supra" --help > "$stage/$name/supra.txt" 2>/dev/null || true
tar -czf "dist/${name}.tar.gz" -C "$stage" "$name"
sha256sum "dist/${name}.tar.gz" > "dist/${name}.tar.gz.sha256"
echo "packaged dist/${name}.tar.gz"
