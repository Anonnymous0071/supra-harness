#!/usr/bin/env bash
# Package the supra binary into dist/ for a release target.
set -euo pipefail

target=${1:?usage: package.sh <target-triple>}

cargo build --locked --release -p supra_cli --target "$target"
mkdir -p dist
name="supra-${target}"
cp "target/${target}/release/supra" "dist/${name}"
sha256sum "dist/${name}" > "dist/${name}.sha256"
echo "packaged dist/${name}"
