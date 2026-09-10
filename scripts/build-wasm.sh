#!/usr/bin/env bash
# Build every agent WASM component declared by the repository.
#
# A green component gate must compile at least one component. The runtime's WIT host lives
# in `supra_plugin`, but a WIT declaration alone is not a guest artefact and cannot satisfy
# this gate.
set -euo pipefail

target=${1:-wasm32-wasip2}
root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)

shopt -s nullglob
manifests=("$root"/agents/*/Cargo.toml)
if [[ ${#manifests[@]} -eq 0 ]]; then
    echo "build-wasm: no agent component manifests found under agents/" >&2
    exit 1
fi

built=0
for manifest in "${manifests[@]}"; do
    name=$(basename "$(dirname "$manifest")")
    echo "==> building agent component: $name -> $target"
    cargo build --locked --release --target "$target" --manifest-path "$manifest"

    artifact=$(python3 - "$manifest" "$target" <<'PY'
import json
import pathlib
import subprocess
import sys

manifest, target = sys.argv[1:]
metadata = json.loads(subprocess.check_output([
    "cargo", "metadata", "--locked", "--no-deps", "--format-version", "1",
    "--manifest-path", manifest,
], text=True))
packages = [package for package in metadata["packages"] if package["manifest_path"] == str(pathlib.Path(manifest))]
if len(packages) != 1:
    raise SystemExit(f"build-wasm: expected one package for {manifest}, found {len(packages)}")
name = packages[0]["name"].replace("-", "_")
print(pathlib.Path(metadata["target_directory"]) / target / "release" / f"{name}.wasm")
PY
    )
    if [[ ! -s "$artifact" ]]; then
        echo "build-wasm: expected non-empty component artifact missing: $artifact" >&2
        exit 1
    fi
    SUPRA_COMPONENT_ARTIFACT="$artifact" \
        cargo test --locked -p supra_plugin component_contract_accepts -- --ignored
    built=$((built + 1))
done

echo "build-wasm: built and executed $built component contract(s) for $target"
