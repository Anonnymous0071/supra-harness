#!/usr/bin/env bash
# Package a built binary into dist/ for a release target.
#
# There is no binary to package before T30 supra_cli. Failing loudly is
# correct: a tag pushed today has nothing to ship, and a silent success would
# publish an empty release.
set -euo pipefail

target=${1:?usage: package.sh <target-triple>}

echo "packaging is implemented in T30 (supra_cli + supra_update)." >&2
echo "requested target: $target" >&2
exit 1
