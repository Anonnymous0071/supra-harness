#!/usr/bin/env bash
# Regenerate the third-party licence manifest.
set -euo pipefail

root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
out="$root/THIRD-PARTY-LICENSES.md"

if ! command -v cargo-about >/dev/null 2>&1; then
    echo "cargo-about not installed. Install with:" >&2
    echo "  cargo install cargo-about --locked" >&2
    exit 1
fi

cd "$root"
if [[ ! -f about.toml ]]; then
    cargo about init
fi
cargo about generate --format json >"$out.json"
cargo about generate about.hbs >"$out" 2>/dev/null || {
    echo "no about.hbs template; JSON manifest written to $out.json"
    rm -f "$out"
}
echo "wrote $out"
