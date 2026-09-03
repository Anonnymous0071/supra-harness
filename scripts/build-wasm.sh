#!/usr/bin/env bash
# Build the agent WASM components (T20).
#
# No components exist before T20. Exiting 0 with a note keeps `just ci` honest
# during T1..T19 rather than forcing a placeholder crate into the graph.
set -euo pipefail

target=${1:-wasm32-wasip2}
root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)

if [[ ! -d "$root/agents" ]]; then
    echo "no agents/ directory yet; WASM components land in T20 (target: $target)"
    exit 0
fi

shopt -s nullglob
manifests=("$root"/agents/*/Cargo.toml)
if [[ ${#manifests[@]} -eq 0 ]]; then
    echo "agents/ contains no component crates yet (target: $target)"
    exit 0
fi

for manifest in "${manifests[@]}"; do
    name=$(basename "$(dirname "$manifest")")
    echo "==> building agent component: $name -> $target"
    cargo build --locked --release --target "$target" --manifest-path "$manifest"
done
