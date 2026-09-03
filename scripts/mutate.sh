#!/usr/bin/env bash
# Mutation harness.
#
# Applies a source mutation, rebuilds, and reports whether the test suite caught
# it. Exists as a script rather than an inline loop because an earlier inline
# version had a defect that silently invalidated its own results: it ignored the
# build exit code, so a mutation rejected by -Werror left the previous, correct
# binary in place and ctest passed. Three mutations were reported as SURVIVED when
# they had never been compiled at all.
#
# Three outcomes, all distinguished:
#
#   CAUGHT     mutation built and the suite failed  -> the invariant is tested
#   SURVIVED   mutation built and the suite passed  -> a real test gap
#   BUILD_FAIL mutation did not compile             -> not a verdict; the
#              mutation needs adjusting (usually an unused variable under
#              -Werror) before it says anything
#
# Usage: mutate.sh <file> <find-file> <replace-file> <description>
#
# The target's extension picks the build+test pair: C++ sources go through
# `just build-cpp` + `ctest`, Rust sources through `cargo test -p supra_ffi`.
# The same build-failure discipline applies to both, and for Rust the check is
# stricter, because one `cargo test` invocation both compiles and runs: the log
# must be inspected to tell "did not compile" (BUILD_FAIL, not a verdict) from
# "compiled and failed" (CAUGHT).

set -uo pipefail

root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
cd "$root"

file=${1:?usage: mutate.sh <file> <find-file> <replace-file> <description>}
find_file=${2:?}
replace_file=${3:?}
desc=${4:?}

backup=$(mktemp)
cp "$file" "$backup"

restore() {
    cp "$backup" "$file"
    rm -f "$backup"
}
trap restore EXIT

applied=$(
    python3 - "$file" "$find_file" "$replace_file" <<'PY'
import sys
target, find_path, replace_path = sys.argv[1:4]
source = open(target).read()
needle = open(find_path).read()
replacement = open(replace_path).read()
if needle not in source:
    print("NOT_FOUND")
    sys.exit(0)
open(target, "w").write(source.replace(needle, replacement, 1))
print("OK")
PY
)

if [[ "$applied" != "OK" ]]; then
    printf 'SKIP       %s (site not found)\n' "$desc"
    exit 0
fi

if [[ "$file" == *.rs ]]; then
    # Which crate owns the file. Derived rather than hardcoded so this works for
    # every crate the workspace grows; a file outside crates/ falls back to the
    # whole workspace.
    if [[ "$file" =~ ^crates/([^/]+)/ ]]; then
        target=(-p "${BASH_REMATCH[1]}")
    else
        target=(--workspace)
    fi

    # One invocation compiles and runs, so the exit code alone cannot say
    # whether a failure was a compile failure or a caught mutation.
    if cargo test "${target[@]}" --locked >/tmp/mutate-build.log 2>&1; then
        printf 'SURVIVED   %s  <-- TEST GAP\n' "$desc"
    elif grep -qE 'could not compile|failed to run custom build command' /tmp/mutate-build.log; then
        printf 'BUILD_FAIL %s\n' "$desc"
        printf '           %s\n' "$(grep -m1 -E '^error' /tmp/mutate-build.log || echo 'see /tmp/mutate-build.log')"
    else
        printf 'CAUGHT     %s\n' "$desc"
    fi
    exit 0
fi

if ! just build-cpp >/tmp/mutate-build.log 2>&1; then
    # Not a verdict. The mutation must compile before its survival means
    # anything, and reporting it as SURVIVED here is exactly the bug this script
    # was written to prevent.
    printf 'BUILD_FAIL %s\n' "$desc"
    printf '           %s\n' "$(grep -m1 'error:' /tmp/mutate-build.log || echo 'see /tmp/mutate-build.log')"
    exit 0
fi

if timeout 250 ctest --test-dir build --no-tests=error >/tmp/mutate-test.log 2>&1; then
    printf 'SURVIVED   %s  <-- TEST GAP\n' "$desc"
else
    printf 'CAUGHT     %s\n' "$desc"
fi
