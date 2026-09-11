#!/usr/bin/env bash
# Sign release artefacts with minisign.
set -euo pipefail

dist=${1:?usage: sign-release.sh <dist-dir>}

if [ -z "${MINISIGN_KEY:-}" ]; then
    echo "sign-release: MINISIGN_KEY must name the official secret-key file" >&2
    exit 1
fi
if [ ! -f "$MINISIGN_KEY" ]; then
    echo "sign-release: secret-key file does not exist: $MINISIGN_KEY" >&2
    exit 1
fi
command -v minisign >/dev/null 2>&1 || {
    echo "sign-release: minisign is required" >&2
    exit 1
}

shopt -s nullglob
artefacts=()
for artefact in "$dist"/*; do
    case "$artefact" in
        *.minisig|*.sha256) continue ;;
        "$MINISIGN_KEY") continue ;;
        *) artefacts+=("$artefact") ;;
    esac
done
if [ "${#artefacts[@]}" -eq 0 ]; then
    echo "sign-release: no release artefacts found in $dist" >&2
    exit 1
fi

for artefact in "${artefacts[@]}"; do
    minisign -Sm "$artefact" -s "$MINISIGN_KEY"
done
echo "signed ${#artefacts[@]} artefacts in $dist"
