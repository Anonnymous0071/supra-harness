#!/usr/bin/env bash
set -euo pipefail

root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
cd "$root"

target="x86_64-unknown-linux-musl"
name="supra-${target}"
mkdir -p "$tmp/stage/$name" "$tmp/dist"
printf '#!/bin/sh\necho fixture\n' > "$tmp/stage/$name/supra"
chmod 755 "$tmp/stage/$name/supra"
tar --sort=name --mtime='@0' --owner=0 --group=0 --numeric-owner \
    -czf "$tmp/dist/$name.tar.gz" -C "$tmp/stage" "$name/supra"
python3 scripts/generate-update-manifest.py "$tmp/dist/$name.tar.gz" v0.2.0 "$target" >/dev/null

manifest="$tmp/dist/$name.manifest.json"
python3 - "$manifest" <<'PY'
import json, pathlib, sys
path = pathlib.Path(sys.argv[1])
raw = path.read_bytes()
data = json.loads(raw)
assert raw == (json.dumps(data, sort_keys=True, separators=(",", ":")) + "\n").encode()
assert data["schema"] == 1
assert data["target"] == "x86_64-unknown-linux-musl"
assert data["version"] == "0.2.0"
assert data["archive"] == "supra-x86_64-unknown-linux-musl.tar.gz"
assert data["executable"]["path"] == "supra-x86_64-unknown-linux-musl/supra"
PY

members=$(tar -tzf "$tmp/dist/$name.tar.gz")
[ "$members" = "$name/supra" ] || { echo "package test: unexpected members: $members" >&2; exit 1; }

if MINISIGN_KEY= bash scripts/sign-release.sh "$tmp/dist" >/dev/null 2>&1; then
    echo "package test: signing unexpectedly allowed no key" >&2
    exit 1
fi
if SUPRA_VERSION=v0.2.0 SUPRA_PUBKEY= bash scripts/install.sh >/dev/null 2>&1; then
    echo "package test: installer unexpectedly allowed no public key" >&2
    exit 1
fi

echo "package trust-chain tests: ok"
