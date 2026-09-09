#!/usr/bin/env bash
# Sign release artefacts with minisign.
set -euo pipefail

dist=${1:?usage: sign-release.sh <dist-dir>}

if [ -z "${MINISIGN_KEY:-}" ]; then
    echo "sign-release: MINISIGN_KEY not set; skipping signatures (artefacts remain in $dist)." >&2
    exit 0
fi
for artefact in "$dist"/supra-*; do
    case "$artefact" in
        *.minisig|*.sha256) continue;;
    esac
    minisign -Sm "$artefact" -s "$MINISIGN_KEY"
done
echo "signed artefacts in $dist"
