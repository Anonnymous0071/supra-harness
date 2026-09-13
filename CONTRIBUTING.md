# Contributing

## Before you start

Read [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md). It is normative: where an
implementation disagrees with an invariant stated there, the implementation is
wrong. Several rules look arbitrary until you read the derivation - the
prohibition on prompt compression, for instance, follows from the 10x pricing
gradient between cached and uncached input.

## Stage ordering

Work is organised as 37 numbered stages, T1 through T30, with insertions like
T12.5 and T15.7 at their correct dependency point.

- A stage may depend only on earlier stages.
- Do not skip ahead. Each stage must be complete and verifiable before anything
  depends on it.
- New features are inserted as a **new stage** at the right dependency point,
  never by backtracking into a completed one.
- Stages carry an explicit definition of done. Do not mark one complete without
  meeting it.

## Setup

```sh
just doctor      # what is missing and why
just bootstrap   # install rustup targets and components
just ci          # the full gate
```

System packages (clang, cmake, bubblewrap) install per-distribution;
`just doctor` names them.

## Gates

`just ci` runs: `fmt-check`, `clippy -D warnings`, `deny`, structural
`invariants`, strict rustdoc via `docs-check`, the Rust 1.86 `msrv` check,
`test-cpp`, the built-component `build-wasm` execution check, and `test-rust`.
It must pass before you push.

Recipes that have nothing to do yet say so and exit 0. `test-cpp` switches to
`--no-tests=error` the moment a `crates/supra_ffi/native/cpp/libsupra_*` directory exists, so an empty
suite stops being acceptable at exactly the point it becomes a defect.

## Code conventions

**Rust.** `unsafe_code = "warn"` workspace-wide; only T5 `supra_ffi` re-allows
it, and every `unsafe` block there needs a safety comment. `unwrap`, `expect`,
and `panic` are lint-flagged: a panic on a tool-call path ends a session that
may have been running for hours. `print_stdout` and `print_stderr` are flagged
because stdout belongs to the protocol channel and diagnostics go through T8
`supra_log`.

**C++20.** The public surface is a flat, `noexcept` C ABI. `-fno-exceptions` is
not an optimisation: an exception unwinding into Rust is undefined behaviour, so
the machinery is removed. Warnings are errors. Tests are plain executables
asserting on exit code - no test framework enters the dependency graph.

**Determinism is correctness here.** Prefix hashing (T14) and canonical
serialisation (T13) require stable key ordering and canonical float formatting.
`clippy::mutable_key_type` is `deny` for this reason. Iteration order over a
`HashMap` reaching a serialised prompt is a cache-invalidation bug, not a style
issue.

## Comments

Comment the *why*, never the *what*. A comment earns its place when it records a
constraint, an invariant, a provider behaviour, or the reason an obvious
alternative was rejected. Delete comments that narrate code.

## Dependencies

`[workspace.dependencies]` in the root `Cargo.toml` is the only place a version
appears. Members write `foo.workspace = true`.

Adding a dependency requires: a reason no existing dependency covers it, a
licence on the `deny.toml` allowlist, and evidence of active maintenance. Weigh
it against the binary-size budget (<30 MB stripped) - `rustls` over OpenSSL,
`tokio` over a second async runtime, and both are enforced by `deny.toml`.

## Verification

Never claim something works without having run it. When you cannot verify, say
so explicitly.

Separate three kinds of claim, and label them:

- **Verified** - you ran it, or it comes from provider documentation you read.
- **Derived** - arithmetic from verified inputs. Every currency figure in
  `docs/ARCHITECTURE.md` is derived, and section 11 says so.
- **Open** - not yet known. Record it as such rather than guessing. T13.5 exists
  because two documentation questions remain open, and T14 stays conservative
  until they are answered.

## Commits

Conventional Commits with the stage in the scope:

```
feat(T2): grapheme cluster segmentation for emoji ZWJ sequences
fix(T14): discard ephemeral blocks before sealing the BP4 delta
docs(T1): record the derivation behind the no-compression decision
```

Explain *why* in the body. The diff already shows what.
