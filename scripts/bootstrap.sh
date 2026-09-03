#!/usr/bin/env bash
# Install the rustup targets and components this repository pins.
#
# Deliberately limited to rustup. System packages (clang, cmake, bubblewrap)
# differ per distribution and installing them needs privileges this script
# should not assume; `just doctor` names what is missing and why.
set -euo pipefail

if ! command -v rustup >/dev/null 2>&1; then
    echo "rustup not found. Install from https://rustup.rs and re-run." >&2
    exit 1
fi

echo "==> components"
rustup component add rustfmt clippy llvm-tools rust-src

echo "==> targets"
rustup target add wasm32-wasip2 wasm32-wasip1

echo
echo "Cargo-installed tooling is optional; install what you need:"
echo "  cargo install cargo-deny --locked      # supply-chain gate"
echo "  cargo install cargo-nextest --locked   # faster test runs"
echo
echo "System packages are reported by: just doctor"
