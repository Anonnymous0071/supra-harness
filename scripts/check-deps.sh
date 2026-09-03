#!/usr/bin/env bash
# Report the toolchain this repository needs against what is installed.
#
# Exit 0 when every required tool is present. Optional tools are reported but
# never fail the check: they gate individual features, and the message says
# which feature degrades.
set -uo pipefail

missing=0

req() {
    local name=$1 why=$2
    if command -v "$name" >/dev/null 2>&1; then
        printf '  \033[32m ok \033[0m %-14s %s\n' "$name" "$("$name" --version 2>&1 | head -1)"
    else
        printf '  \033[31mmiss\033[0m %-14s required: %s\n' "$name" "$why"
        missing=$((missing + 1))
    fi
}

opt() {
    local name=$1 why=$2
    if command -v "$name" >/dev/null 2>&1; then
        printf '  \033[32m ok \033[0m %-14s %s\n' "$name" "$("$name" --version 2>&1 | head -1)"
    else
        printf '  \033[33mskip\033[0m %-14s optional: %s\n' "$name" "$why"
    fi
}

echo
echo "required"
req rustc "host workspace"
req cargo "host workspace"
req rustup "wasm targets for T20 agent components"
req cmake "C++20 libraries (T2-T4)"
req ctest "C++20 test suites"
req clang++ "C++20 libraries; the pinned compiler for this tree"
req git "digest churn signals (T15)"

echo
echo "optional"
opt clang-tidy "C++ lint via 'just tidy'"
opt clang "clang static analyser in T22 introspector; skipped when absent"
opt ninja "faster CMake builds"
opt bwrap "Linux sandbox backend (T4); tool execution is unsandboxed without it"
opt cargo-deny "supply-chain gate via 'just deny'"
opt cargo-nextest "faster test runs"

echo
echo "rustup targets"
for target in wasm32-wasip2 wasm32-wasip1; do
    if rustup target list --installed 2>/dev/null | grep -qx "$target"; then
        printf '  \033[32m ok \033[0m %s\n' "$target"
    else
        printf '  \033[31mmiss\033[0m %s  (run: just bootstrap)\n' "$target"
        missing=$((missing + 1))
    fi
done

echo
if [[ $missing -gt 0 ]]; then
    printf '\033[31m%d required item(s) missing.\033[0m Run: just bootstrap\n' "$missing"
    exit 1
fi
printf '\033[32mToolchain complete.\033[0m\n'
