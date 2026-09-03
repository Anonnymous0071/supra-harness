#!/usr/bin/env bash
# Sign release artefacts with minisign.
#
# Signing is only meaningful once there are artefacts to sign, which arrives
# with T30. Kept as a named step so the release pipeline shows the obligation.
set -euo pipefail

dist=${1:?usage: sign-release.sh <dist-dir>}

echo "release signing is implemented in T30 (supra_update verifies these)." >&2
echo "requested dist dir: $dist" >&2
exit 1
