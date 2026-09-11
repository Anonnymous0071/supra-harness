# supra-harness build orchestration.
#
# One entry point for humans and CI. `just ci` is what the pipeline runs and
# what you should run before pushing.
#
# Every cargo invocation passes --locked: Cargo.lock is the reproducibility
# guarantee, since [workspace.dependencies] uses caret ranges rather than
# exact pins (see the policy note in Cargo.toml).

set shell := ["bash", "-euo", "pipefail", "-c"]

# Out-of-source CMake build tree for the C++20 libraries (T2-T4).
build_dir := justfile_directory() / "build"
cmake_build_type := env("SUPRA_CMAKE_BUILD_TYPE", "RelWithDebInfo")

# clang is the pinned C++ compiler: T22 reuses the clang static analyser, and
# pinning one frontend keeps -Werror behaviour identical across machines.
# Override with SUPRA_CXX to build with something else deliberately.
cxx := env("SUPRA_CXX", "clang++")

# WASM target for agent components (T20).
wasm_target := "wasm32-wasip2"

[private]
default:
    @just --list --unsorted

# ---------------------------------------------------------------------------
# Environment
# ---------------------------------------------------------------------------

# Report the toolchain this repository needs and what is actually present.
doctor:
    @bash scripts/check-deps.sh

# Install missing rustup targets and components.
bootstrap:
    @bash scripts/bootstrap.sh

# ---------------------------------------------------------------------------
# Build
# ---------------------------------------------------------------------------

# Configure + build the C++20 libraries (T2 width, T3 ansi, T4 sandbox).
build-cpp:
    cmake -S . -B {{ build_dir }} -DCMAKE_BUILD_TYPE={{ cmake_build_type }} \
        -DCMAKE_CXX_COMPILER={{ cxx }} -DCMAKE_EXPORT_COMPILE_COMMANDS=ON
    cmake --build {{ build_dir }} --parallel

# Build the agent WASM components (T20).
build-wasm:
    @bash scripts/build-wasm.sh {{ wasm_target }}

# Debug build of the host workspace.
build: build-cpp
    cargo build --workspace --locked --all-targets

# Release build: single static binary, size- and startup-optimised.
release: build-cpp
    cargo build --workspace --locked --release

# Development loop: fast rebuild plus the host binary.
dev:
    cargo build --locked

# ---------------------------------------------------------------------------
# Test
# ---------------------------------------------------------------------------

# CTest suites for the C++20 libraries.
#
# An empty suite is expected before T2 but becomes a defect afterwards, so the
# gate flips to --no-tests=error as soon as the first library directory exists.
test-cpp: build-cpp
    #!/usr/bin/env bash
    set -euo pipefail
    if compgen -G "cpp/libsupra_*/CMakeLists.txt" >/dev/null; then
        ctest --test-dir {{ build_dir }} --output-on-failure --no-tests=error
    else
        echo "test-cpp: no C++ libraries yet (they land in T2-T4)"
    fi

# Rust unit + integration tests.
test-rust:
    cargo test --workspace --locked --all-targets --all-features
    cargo test --workspace --locked --doc --all-features

# Build every declared package with the workspace MSRV, not only the newer pinned default.
msrv:
    #!/usr/bin/env bash
    set -euo pipefail
    msrv=$(sed -n 's/^rust-version = "\(.*\)"/\1/p' Cargo.toml | head -1)
    [[ -n "$msrv" ]] || { echo "msrv: Cargo.toml does not declare rust-version" >&2; exit 1; }
    cargo +"$msrv" check --workspace --locked --all-targets --all-features

test: test-cpp test-rust

# ---------------------------------------------------------------------------
# Quality gates
# ---------------------------------------------------------------------------

fmt:
    cargo fmt --all

fmt-check:
    cargo fmt --all -- --check

clippy:
    cargo clippy --workspace --locked --all-targets --all-features -- -D warnings

# Supply chain: advisories, licences, banned crates, source allowlist.
deny:
    cargo deny --all-features check

# clang-tidy over the C++20 sources. Requires build-cpp first for
# compile_commands.json.
tidy: build-cpp
    @bash scripts/run-clang-tidy.sh {{ build_dir }}

# Structural invariant checks the compiler cannot express: no mutation path on
# `Sealed`, no `Sealable` on `EphemeralBlock`, no `Mode` on the authority axis,
# quorum as an integer rational, `unsafe` nowhere but supra_ffi.
invariants:
    @bash scripts/check-invariants.sh

# Build API documentation with every warning promoted to an error.
docs-check:
    RUSTDOCFLAGS="-D warnings" cargo doc --workspace --locked --no-deps --all-features

lint: fmt-check clippy deny invariants docs-check tidy

# What CI runs. Ordered cheapest-first so failures surface fast.
ci: lint msrv test-cpp build-wasm test-rust

# ---------------------------------------------------------------------------
# Measurement
# ---------------------------------------------------------------------------

# Criterion benchmarks against the budgets in docs/ARCHITECTURE.md.
bench:
    cargo bench --workspace --locked

# Economy gate (T30 supra_eval): cache hit rate, USD/turn, tier accuracy.
eval *args:
    cargo run --locked -p supra_cli -- eval {{args}}

docs:
    cargo doc --workspace --locked --no-deps --all-features

# ---------------------------------------------------------------------------
# Distribution
# ---------------------------------------------------------------------------

package target:
    @bash scripts/package.sh {{target}}

# ---------------------------------------------------------------------------
# Housekeeping
# ---------------------------------------------------------------------------

clean:
    cargo clean
    rm -rf {{ build_dir }}

# Regenerate the third-party licence manifest.
licenses:
    @bash scripts/gen-licenses.sh
