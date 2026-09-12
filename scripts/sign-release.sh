#!/usr/bin/env bash
# Sign every canonical per-target update manifest with Minisign.
set -euo pipefail

dist=${1:?usage: sign-release.sh <dist-dir>}
: "${MINISIGN_KEY:?sign-release: MINISIGN_KEY must name the official secret-key file}"
[ -f "$MINISIGN_KEY" ] || { echo "sign-release: secret-key file does not exist: $MINISIGN_KEY" >&2; exit 1; }
command -v minisign >/dev/null 2>&1 || { echo "sign-release: minisign is required" >&2; exit 1; }

shopt -s nullglob
manifests=("$dist"/supra-*.manifest.json)
[ "${#manifests[@]}" -gt 0 ] || { echo "sign-release: no update manifests found in $dist" >&2; exit 1; }
for manifest in "${manifests[@]}"; do
    python3 - "$manifest" <<'PY'
import json
from pathlib import Path
import sys

path = Path(sys.argv[1])
raw = path.read_bytes()
value = json.loads(raw)
canonical = (json.dumps(value, sort_keys=True, separators=(",", ":")) + "\n").encode()
if raw != canonical:
    raise SystemExit(f"sign-release: non-canonical manifest: {path}")
PY
    minisign -Sm "$manifest" -s "$MINISIGN_KEY"
done
echo "signed ${#manifests[@]} canonical update manifests in $dist"
