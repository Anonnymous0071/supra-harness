# Changelog

All notable changes to this project are documented here.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and
this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- **T2** `cpp/libsupra_width`: terminal cell width and grapheme segmentation.
  - Flat C ABI: UTF-8 decoding, per-code-point width, UAX #29 grapheme cluster
    segmentation, string measurement, cluster-safe truncation, validation, and a
    startup glyph probe.
  - East Asian Ambiguous resolved from a caller-supplied locale flag rather than
    baked in, because the class is a property of the terminal rather than of the
    text. `supra_width_probe` lets the TUI measure its own glyph set and fall
    back to an ASCII tier.
  - Tables generated from vendored Unicode 17.0.0 extracts and committed, so the
    build needs neither Python nor network. The generator asserts that every
    table is sorted, non-overlapping, and coalesced.
  - Hangul LV/LVT derived arithmetically instead of tabulated, removing 798
    ranges (31% of the total); the generator verifies the derivation against the
    UCD on every run.
  - Malformed UTF-8 yields U+FFFD and advances exactly one byte, guaranteeing
    forward progress for every caller's scan loop on arbitrary bytes.
  - Five CTest suites, all passing under both RelWithDebInfo and ASan+UBSan:
    766 official UAX #29 conformance cases, exhaustive UTF-8 round-trip over all
    1 112 064 scalars, all 11 172 Hangul syllables, and truncation across
    7 inputs x 21 limits x 2 locales.
  - Conformance suite refuses to pass vacuously: fewer than 500 parsed cases
    exits 2, so a moved fixture cannot look like a green run.
  - Six deliberate mutations introduced and all six caught, including
    GB12/GB13 flag pairing, GB11 pictographic ZWJ, GB9c Indic conjuncts, and
    wide-cluster splitting during truncation.

- **T1** Workspace foundation.
  - Cargo workspace: resolver 3, edition 2024, MSRV 1.85, shared package
    metadata, dependency pinning policy, lint policy, and four build profiles.
  - Toolchain pinned to Rust 1.96.0 with `wasm32-wasip2` and `wasm32-wasip1`.
  - Quality gates: `rustfmt.toml`, `clippy.toml`, `deny.toml` (licence
    allowlist, advisory policy, banned crates, source allowlist).
  - CMake root establishing the shared C++20 contract for T2-T4: C++20,
    `-fno-exceptions` at the ABI boundary, warnings-as-errors, PIC static
    archives, optional sanitizers.
  - `justfile` as the single build entry point; recipes report out-of-scope
    stages instead of failing.
  - CI: lint, supply chain, C++ on three platforms, C++ under
    ASan+UBSan, WASM components, tests on three platforms.
  - Release and security workflows: cross-target build, checksums, signing
    hook, advisories, licence policy, CodeQL, secret scanning.
  - `docs/ARCHITECTURE.md`: the prefix-stability thesis, eight invariants,
    peer-consensus model, permission model, and the 37-stage map, with
    verified/derived/open status recorded per claim.

[Unreleased]: https://github.com/trubs/supra-harness/commits/main
