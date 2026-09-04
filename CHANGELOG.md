# Changelog

All notable changes to this project are documented here.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and
this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- **T8** `crates/supra_log`: structured diagnostics, and the one crate permitted to
  write to stderr.
  - **Redaction happens at the sink**, over the whole formatted line, after every call
    site has had its say. T7 removed every field that could hold a credential from the
    configuration schema and a test still found a leak, so the net goes at the last
    moment before bytes become durable. Two mechanisms, because each misses what the
    other catches: an **exact** field-name match (`api_key` is a secret, `api_key_env`
    is a signpost) and a prefixed value shape (`sk-`, `ghp_`, `AKIA`, `Bearer `, PEM).
  - What must survive is as load-bearing as what is caught. A generic entropy rule would
    eat supra's own diagnostic material - ULIDs, content hashes, cache keys - so there is
    none. A `ContentHash` surviving in both its forms, and `api_key_env`'s value
    surviving, are tests rather than intentions.
  - **One line, one write** on an `O_APPEND` descriptor, so two processes sharing a log
    produce whole lines rather than shredded ones - and so redaction sees a complete line
    exactly once, which is what makes it sound. Threaded and split-write tests cover both
    halves.
  - Size-bounded rotation with a fixed file count; an existing file's length counts
    toward the threshold at open, or a restart would reset the bound. The file is created
    `0600`, for the same reason T7 holds the user's configuration to `0600`.
  - stderr mirroring is suppressed by a **guard**, not a pair of setters: a `set(false)`
    whose `set(true)` is missed would silently discard every diagnostic for the rest of
    the session. A line written under the guard still reaches the file - suppression is
    about the terminal, not the record.
  - A failed write is counted and reported on the next line that succeeds. A gap in a log
    is only debuggable if the log says there is one.
  - `build` returns a subscriber without installing it, so the wiring is testable as many
    times as a test suite likes; exactly one test installs a global subscriber.
  - The crate deliberately does **not** depend on T7. Configuration loading is exactly
    when diagnostics are needed, and a logger that cannot start until configuration has
    loaded cannot report why configuration failed to load.
  - 56 tests plus a doc test. Twelve mutations: eleven caught, one recorded as
    platform-equivalent rather than papered over.

- `scripts/check-invariants.sh` gained the T8 checks: no diagnostic outside `sink.rs` may
  touch stderr, the redaction fast path must read `SHAPES` and `SECRET_FIELDS` rather than
  restate them, the field rule must stay an exact match, and the log file must stay
  `0600`. All five probed against deliberate violations.

- **T7** `crates/supra_config`: layered configuration - discovery, per-field
  precedence, provenance, and fail-fast validation.
  - Precedence is per **field**, not per file: `ConfigLayer` is all-`Option` and
    says only what its file said, `Config` is fully resolved. Without that split a
    project file setting `[cohort] limit` would erase the user's
    `[thinking] budget`. Every resolved setting also records **which layer supplied
    it**, so `/config` can answer the question a user actually has when a setting
    appears to be ignored.
  - Resolution cannot fail. Layers are validated individually, so combining them is
    a total function - which also means a precedence bug cannot hide behind an error
    path.
  - `Config` has no setter and no `&mut` accessor. That is what "the thinking budget
    is frozen per session" amounts to in practice: not a rule to remember but the
    absence of a way to break it.
  - **No configuration field can hold a credential.** Only `api_key_env` and
    `api_key_keyring` - the *name* of an environment variable or keyring entry -
    exist. `api_key` and `auth_token` are declared solely to be refused with the
    remedy, since `deny_unknown_fields` alone would say "unknown field" and leave the
    reader to guess. A value in `api_key_env` that is not shaped like a variable name
    is refused too: otherwise supra would look up a variable literally named `sk-...`,
    report a missing credential, and send the reader looking in the wrong place.
  - **The project layer is not trusted.** A repository is cloned from anywhere, so
    `.supra/config.toml` may not name a provider, an endpoint, or a credential
    source, and its permission mode may only make the session *stricter*. Under plain
    precedence `project` outranks `user`, so a cloned repository shipping
    `mode = "yolo"` would get it.
  - Reading is hardened in two different directions. The permission check runs on the
    **open handle** rather than the path, so the mode reported and the bytes read
    belong to the same file. The file-type check runs **before** the open, because
    opening a FIFO read-only blocks until a writer appears - a handle-based type check
    would already have hung the process.
  - Only the user layer is held to `0600`; a project file is normally committed and
    cannot be. Its safety comes from the schema instead.
  - Environment variables use a reserved `SUPRA_CONFIG_` prefix, and an unrecognised
    one is refused. A bare `SUPRA_` prefix could not be: `SUPRA_CXX`,
    `SUPRA_CPP_BUILD_DIR`, and `SUPRA_CMAKE_BUILD_TYPE` are real build variables, and
    refusing unknown `SUPRA_*` names would break a developer's own shell.
  - Every loader input is injectable, so the precedence rules are tested as
    arithmetic. A loader that reached for the real environment internally could not be
    exercised without mutating process-global state, and `set_var` is both `unsafe` in
    this edition and racy across parallel tests.
  - 82 tests plus a doc test. Twelve mutations against the load-bearing rules, all
    caught.

- `scripts/check-invariants.sh` gained the T7 checks: no mutation path on the
  resolved `Config`, no field that can hold a literal credential, `deny_unknown_fields`
  on every schema shape, and `describe_toml_error` as the single place that sees a
  parser error. The last is checked structurally rather than by grepping for
  `error.to_string()`, because a probe showed that pattern misses `e.to_string()`.

- `scripts/mutate.sh` wraps its Rust branch in `timeout`. A mutation can remove a
  guard against blocking rather than a guard against a wrong answer, and an unbounded
  hang stalls the harness instead of reporting a verdict.

- **T6** `crates/supra_types`: the contract layer, where the architecture's
  invariants stop being prose.
  - **I1** is the type: `Sealed<T>` has no `DerefMut`, no `as_mut`, and no
    `into_inner` - the last one because moving the value out would allow
    edit-and-reseal at the same sequence number, which is a rewrite wearing an
    append's clothes. Every reader that legitimately needs the contents needs only
    `&T`.
  - **I2** is a type error rather than a runtime refusal: the `Sealable` bound sits
    on `Sealed`'s type definition, so `Sealed<EphemeralBlock>` cannot be *written
    down*. The 202k-against-4k arithmetic that justifies the invariant is a test, so
    the reason for the type is executable.
  - Content hashing runs over a length-prefixed canonical encoding owned by this
    crate, deliberately **not** the T13 wire serialiser. Hashing the wire bytes
    would make every stored digest move whenever the wire format did, and I4's
    guarantee that a recalled turn is byte-identical would be measured against a
    moving target. No `serde_json` in the dependency graph at all.
  - Every validated type re-runs its constructor on deserialisation - `Sealed`,
    `Segment`, `MemoryIndexEntry`, `Lineage`, `Verdict`. Deriving `Deserialize`
    would be a hole straight through each invariant: a session file could
    reintroduce exactly the shapes the constructor rejects, and that content is
    about to be sealed into a prefix and paid for on every later turn.
  - No floats anywhere: `MicroUsd` is integer micro-dollars, `Confidence` is an
    enum, cache multipliers are integer percentages, and quorum is `(2k).div_ceil(3)`
    because `(0.67 * 3).ceil()` is 3 and would silently demand unanimity from a
    three-peer cohort. Every documented figure is reproduced as arithmetic: the
    tier table's quorum and byzantine columns at every k, the 15 req/min shard
    count landing on 6 for k=80, the $6.30 hundred-turn baseline, the I8 padding
    trade-off.
  - `Lineage` caps depth at 1 and refuses an id already in the chain, so a peer
    cannot spawn and a model cannot spawn itself. A cohort of 80 is siblings at
    equal depth with no peer in another's ancestry, which is the structural
    statement of "not an orchestrator".
  - The permission model keeps its two axes apart: `decide` consults `ToolClass`
    before `Mode`, and an exhaustive walk of class x invoker x mode x reversibility
    shows no mode - `yolo` included - can turn an authority refusal into permission.
  - 47 event variants across 11 topics, with the topic partition and a serde
    round trip both walked through an exhaustive sample list that fails to compile
    when a variant is added without one.
  - `admit` resolves a requested tier against the configurable peer limit by
    reducing the **tier** rather than truncating k, because a limit of 6 sits
    between E2's ceiling and E3's floor and truncating would yield a cohort in no
    tier. `Tier::containing(k) == Some(tier)` is tested across all 480 tier-limit
    combinations, which is what makes the table's gaps at 6 and 13-15 unreachable
    rather than merely noted.
  - 122 tests. Fifteen mutations against the load-bearing invariants, all caught.

- `scripts/check-invariants.sh`, wired into `just lint`: the CI half of "enforced by
  types and by CI, not by convention", covering what a type system cannot assert -
  an absence. Checks for a mutation path on `Sealed`, a `Sealable` impl for
  `EphemeralBlock`, ephemeral state reaching a segment, a `Mode` on the authority
  axis, a float in the quorum module, and `unsafe` outside `supra_ffi`.

- `scripts/mutate.sh` infers the crate from the mutated file's path, so the Rust
  branch works for every crate the workspace grows rather than only `supra_ffi`.

- **T5** `crates/supra_ffi`: safe Rust bindings to the three C++ libraries, and
  the only crate permitted to contain `unsafe`.
  - Bidirectional layout ratchet: `abi_sizes.rs` is the single source for every
    structure's size and alignment, asserted against C++ `sizeof` by a
    `static_assert` in the build script and against Rust `size_of`/`align_of`
    in `sys.rs`. A hand-written `extern` declaration drifting from its header
    fails to compile instead of silently reading wrong offsets, and both checks
    are compile-time, so they stay valid when cross-compiling.
  - Sentinels become types at the boundary: width `-1` becomes
    `Width::NonPrintable` (distinct from `Width::Zero`, because output
    sanitising must tell them apart), East Asian Ambiguous stays a caller
    parameter per the T2 finding, and every C++ `bool`/`int` protocol becomes a
    real `bool` or enum.
  - Ownership is designed in, not documented in: `Token` owns its 432-byte
    structure and borrows slices from `&self`; `Policy` owns its paths as
    `CString`s and materialises the borrowed-pointer raw form only per call;
    `Process` kills and reaps on drop. Raw policy construction goes through the
    ABI's own `allow`/`allow_port` constructors rather than writing fields, so
    the C side's normalisation rules cannot drift from a second implementation.
  - 47 Rust tests, including cross-library agreement (a styled string measures
    the same as its stripped form), chunk-boundary scanner resumption, sandbox
    execution end-to-end (exit codes, environment non-inheritance, drop-kills),
    and fail-closed refusals reached via `force_tier_for_testing`.
  - Five deliberate mutations via the Rust branch of `scripts/mutate.sh`; four
    caught, one (the layout ratchet) refused at build time by the
    `static_assert`, which is that invariant's designed enforcement point. One
    gap found and closed: nothing asserted `fixed_string` stops at the first
    NUL or preserves bytes >= 0x80 through the negative-`c_char` conversion.
  - `crates/supra_ffi/README.md` records the ratchet rationale, sentinel
    discipline, and the full mutation table.

- `scripts/mutate.sh` gained a Rust branch: for `.rs` targets it runs
  `cargo test -p supra_ffi --locked` and inspects the log to keep the T4
  discipline intact - one invocation both compiles and runs, so "could not
  compile" is reported BUILD_FAIL (not a verdict) and only a compiled-then-
  failed run counts as CAUGHT.

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

- **The redaction fast path silently let a whole class of credential through.** The
  pre-check that lets ordinary lines skip the matcher listed its own literals - `"sk-"`,
  `"gh"`, `"xox"` - and one was wrong: **`github_pat_` does not contain `gh`**, because
  the letters are g-i-t-h. Every GitHub personal access token passed straight through a
  matcher that would have caught it. The pre-check now reads the same tables the matcher
  does, so drift is impossible by construction, and a property test walks every entry.
- **A filter default could silence everything without failing.** `build` took the default
  level as a string, and almost any string parses as a *valid* directive because a bare
  word is read as a target name: `"inf"` becomes `inf=trace`. A typo would not error - it
  would enable trace for a target nothing logs to and silence the rest. The parameter is
  now a `LevelFilter`, which cannot be misspelled.
- **The dropped-line counter had no coverage.** The only test that touched it set it by
  hand, so a mutation deleting the increment survived. Closed with `/dev/full`, which
  answers every write with `ENOSPC` - a deterministic version of a full disk.
- **A pasted credential could leak through the error refusing it.**
  `ConfigError::Invalid` originally carried the `toml` crate's `Display` verbatim,
  which prints the offending **source line** with a caret under it. So the message
  explaining that supra never reads a literal credential from a file quoted that
  credential - into stderr, into the T8 log, and into the next bug report. Errors now
  report the location and the cause and never the content. One residual is named
  rather than glossed: a mistyped *scalar* still has its value in the parser's cause
  text, a surface that needs a value in a field of the wrong type.
- **The project layer could loosen the permission mode.** The tightening rule was
  applied as a pass *after* ordinary precedence, but ordinary precedence had already
  accepted the project's mode - and a pass that can only tighten cannot undo a
  loosening. The exhaustive test missed it because it used `Cli` as the operator
  layer, which outranks `Project`; the bug only appeared when the operator layer sat
  *below* the project. Non-operator-controlled layers are now excluded from the
  ordinary pass, and the test walks every operator-controlled source.
- `docs/ARCHITECTURE.md` section 4 was **missing the configurable peer limit
  entirely**, despite it being a locked requirement (1..=80, default 16). Its
  absence is how a reachable gap in the tier table went unnoticed: the limit can
  fall between a tier's floor and the tier below it, and nothing said what happens
  then. The section now specifies the limit, its default, and the resolution rule.
  E5's row also read `<=80` with no floor, leaving the tiers non-disjoint on paper;
  it now reads `33-80` with its derived quorum and byzantine columns filled in.
- `docs/ARCHITECTURE.md` section 6 stated "deny always winning" as a phrase with
  two possible readings. It now carries the decision and the reasoning: any deny
  beats every allow, precedence orders allows only, and a default is the absence of
  a rule rather than a deny. T16.7 inherits the catalogue of what `builtin` may
  deny, not the question of what deny means.
- `rustfmt.toml` claimed deterministic import ordering through
  `imports_granularity` and `group_imports`, which are nightly-only options: on
  the pinned stable toolchain they emitted a warning on every run and took no
  effect, so the documented guarantee did not exist. Removed; what stable
  actually provides (`reorder_imports`, which sorts within contiguous runs but
  never across blank lines) is kept, and the std/external/crate grouping is
  documented as an author-maintained convention rather than a formatter
  guarantee.
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
