#!/usr/bin/env bash
# Print the number of Rust packages in this workspace.
#
# The workspace is deliberately memberless until T5 supra_ffi, and cargo's
# fmt/clippy/metadata front-ends error out on a virtual manifest with no
# members. The quality gates consult this so they can state their scope
# instead of failing on tool misuse or passing vacuously.
set -euo pipefail

cargo metadata --no-deps --format-version 1 2>/dev/null |
    python3 -c 'import json,sys; print(len(json.load(sys.stdin)["packages"]))' 2>/dev/null ||
    echo 0
