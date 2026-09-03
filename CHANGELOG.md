# Changelog

All notable changes to this project are documented here.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and
this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- **T4** `cpp/libsupra_sandbox`: OS-level process isolation behind one C ABI.
  - Linux backend is native rather than a bubblewrap wrapper, decided by
    measurement: applying a Landlock ruleset and then exec'ing `bwrap` fails with
    "Failed to make / slave: Operation not permitted", and still fails under a
    ruleset granting write access everywhere. The ordering supra needs - host
    applies policy, then execs - does not work with bwrap under any policy.
    `unshare(NEWUSER|NEWNET|NEWPID|NEWIPC|NEWUTS)` plus Landlock needs no mount
    operations at all, and gives per-port TCP policy that a network namespace
    cannot.
  - Tiered and reported, never assumed. The probe applies a real ruleset in a
    forked child and confirms a denial occurs, because a reported ABI version
    proves only that the LSM is compiled in and not that it is enabled in the
    boot-time LSM list. A filesystem policy the platform cannot enforce refuses
    to run.
  - Policy is a struct, never an argv string, and commands are argv vectors:
    building a command line from user-supplied paths is the injection surface
    T12.5 exists to remove. Zero-initialising yields the most restrictive setting.
  - Child setup failures are reported through a CLOEXEC pipe, so a caller learns
    which of nine steps failed rather than only that the child exited.
  - Verified escape attempts, all denied: reads and writes outside the allowlist,
    `../..` traversal, TCP connect, `chroot` then reaching out, grandchildren via
    nested `sh -c`, installing a wider ruleset, host process visibility, and host
    environment inheritance.
  - `supra_sandbox_self_identity` provides (device, inode) identity for T12.5
    guard layer L4, so a symlink or rename cannot masquerade. The residual gap - a
    *copied* binary has a different inode - is asserted rather than documented
    away.
  - Two bugs found by testing rather than reading: `spawn` blocked until the
    payload exited (measured 5004ms for a 5000ms sleep) because the PID-namespace
    intermediate never execs and so never closed its CLOEXEC copy of the report
    pipe, which also made `kill` untestable; and `readlink` truncated silently,
    where a partial path can resolve to a different file and would make guard
    layer L4 compare the wrong inode.
  - macOS and Windows refuse rather than running unconfined, which keeps T16.7's
    `auto` mode safe on platforms whose backend has not landed.

### Fixed

- `scripts/mutate.sh` replaces an inline mutation loop that **produced false
  results**: it ignored the build exit code, so a mutation rejected by `-Werror`
  left the previous correct binary in place, ctest passed, and it reported
  SURVIVED. Three mutations were recorded as test gaps having never been
  compiled. The script distinguishes CAUGHT, SURVIVED, and BUILD_FAIL, treating
  the last as "not a verdict".
- T4 test gaps found by that harness, both closed: the workspace write test
  overwrote a file **persisting between runs**, so truncation stood in for
  creation and no directory-only Landlock bit was exercised; and the fail-closed
  refusals were unreachable on a Landlock-capable kernel, leaving the protection
  for *older* kernels untested. `supra_sandbox_force_tier_for_testing` pins the
  tier downward to reach them.

- **T3** `cpp/libsupra_ansi`: escape sequence parsing, SGR state, style-safe
  truncation.
  - Resumable state machine over the DEC STD 070 / VT500 grammar, covering the
    cases a regex cannot: both OSC terminators (`ESC \` and `BEL`), colon
    sub-parameters (`ESC[4:3m`, `ESC[38:2::255:0:0m`), 8-bit C1 introducers,
    DCS/APC strings, and sequences split across chunk boundaries.
  - `supra_ansi_plan_truncate` returns a plan rather than a copy: prefix length
    plus a trailer, guaranteeing `cells <= max_cells`, no cut inside an escape
    sequence, no split grapheme cluster, and a terminal left in its default
    state. An unstyled line gets an empty trailer.
  - Positional C1 disambiguation. The C1 range `0x80..0x9F` is a subset of the
    UTF-8 continuation range, so U+6587 (`E6 96 87`, second byte = C1 SGA) and
    U+1F600 (three C1-range bytes) would be torn apart by a byte-wise test. The
    scanner carries the outstanding continuation count, so the distinction
    survives a chunk boundary landing mid-character.
  - Full SGR model: 8 attribute bits, 6 underline styles including the
    sub-parameter form, indexed and RGB colour in both the semicolon and colon
    encodings, underline colour, and OSC 8 hyperlink state. Folding and
    serialisation round-trip, which is what makes the truncation trailer sound.
  - Four CTest suites passing under RelWithDebInfo and ASan+UBSan, clean under
    clang-tidy: chunk-size invariance at every size from 1 up, truncation across
    12 inputs x 26 limits x 2 locales, and totality over all 256 single bytes.
  - Eight deliberate mutations introduced, all eight caught. One initially
    survived - the byte-wise C1 test - proving no test actually exercised the
    invariant; a split-boundary regression test now does.
  - `cpp/testing`: shared assertion helpers, extracted from
    `libsupra_width/tests` when libsupra_ansi became the second consumer.

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
