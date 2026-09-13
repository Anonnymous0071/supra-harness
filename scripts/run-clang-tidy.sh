#!/usr/bin/env bash
# Run clang-tidy over the active platform's C++20 translation units using
# compile_commands.json.
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

sources_file=$(mktemp)
trap 'rm -f "$sources_file"' EXIT
python3 - "$build_dir/compile_commands.json" "$root/crates/supra_ffi/native/cpp" >"$sources_file" <<'PY'
import json
import os
import sys

commands_path, source_root = sys.argv[1:]
source_root = os.path.realpath(source_root) + os.sep
with open(commands_path, encoding="utf-8") as commands_file:
    commands = json.load(commands_file)

sources = set()
for command in commands:
    source = command.get("file")
    directory = command.get("directory")
    if not isinstance(source, str) or not isinstance(directory, str):
        continue
    if not os.path.isabs(source):
        source = os.path.join(directory, source)
    source = os.path.realpath(source)
    if source.startswith(source_root) and source.endswith(".cpp"):
        sources.add(source)

for source in sorted(sources):
    sys.stdout.buffer.write(os.fsencode(source) + b"\0")
PY

mapfile -d '' -t sources <"$sources_file"
if [[ ${#sources[@]} -eq 0 ]]; then
    echo "compile_commands.json contains no first-party C++ translation units" >&2
    exit 1
fi

clang-tidy -p "$build_dir" --warnings-as-errors='*' "${sources[@]}"
