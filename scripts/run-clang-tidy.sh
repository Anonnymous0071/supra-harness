#!/usr/bin/env bash
# Run clang-tidy over the C++20 sources using compile_commands.json.
set -euo pipefail

build_dir=${1:-build}
root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)

if ! command -v clang-tidy >/dev/null 2>&1; then
    echo "clang-tidy not installed; refusing to skip required C++ lint (see: just doctor)" >&2
    exit 1
fi

if [[ ! -f "$build_dir/compile_commands.json" ]]; then
    echo "$build_dir/compile_commands.json missing; run 'just build-cpp' first" >&2
    exit 1
fi

shopt -s nullglob globstar
sources=("$root"/crates/supra_ffi/native/cpp/**/*.cpp)
if [[ ${#sources[@]} -eq 0 ]]; then
    echo "no C++ sources yet; libraries land in T2-T4"
    exit 0
fi

clang-tidy -p "$build_dir" --warnings-as-errors='*' "${sources[@]}"
