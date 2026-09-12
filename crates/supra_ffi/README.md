# supra_ffi

Safe Rust bindings to the three supra C++20 libraries, behind one flat C ABI per
library. **T5** of the stage sequence, and the only crate in the workspace
permitted to contain `unsafe` - `unsafe_code = "warn"` is set workspace-wide and
re-allowed only here, so a soundness bug has exactly one crate to hide in.

| Wrapped library | Stage | Provides |
|---|---|---|
| `libsupra_width` | T2 | cell width, grapheme segmentation, Unicode 17 tables |
| `libsupra_ansi` | T3 | escape parsing, SGR state, style-safe truncation |
| `libsupra_sandbox` | T4 | native process isolation: Linux namespaces/Landlock, macOS `sandbox_init`/SBPL, and Windows AppContainer with explicit handle inheritance and a job object |

Nothing above this crate needs `unsafe`. Every `extern` block lives in one
private `sys` module, every block carries a SAFETY comment, and
`unsafe_op_in_unsafe_fn` is denied so an unsafe function body does not get an
implicit licence.

## The layout ratchet

The `extern` declarations are hand-written, not generated, because the ABI is
small, stable, and authored in this repository - a code generator would add a
build dependency for no benefit. But hand-writing carries a real risk: a field
added to a C++ header without a matching Rust change produces a layout mismatch
that **does not crash**. The Rust side reads wrong offsets and returns wrong
answers.

`abi_sizes.rs` closes that hole from both directions at compile time:

- `build.rs` feeds every size and alignment to `abi_check.cpp` as `-D` macros,
  where a `static_assert` compares each against the real C++ `sizeof`.
- `sys.rs` asserts the same constants against Rust's `size_of`/`align_of`.

A divergence fails to build on one side or the other, and neither check needs to
run - which also keeps them valid when cross-compiling. Verified by mutation M1
below: changing one constant is refused by the build, not by a test.

## Sentinel discipline

The C ABI's conventions become types at the boundary, so a caller cannot misuse
them:

- width `-1` ("not printable") becomes `Width::NonPrintable`, distinct from
  `Width::Zero` ("occupies no cell") because sanitising output must tell them
  apart - an integer return would let a caller sum the sentinel into a total;
- `Ambiguous` stays a parameter on every measurement rather than resolving once,
  because it is a property of the terminal's locale, and baking in either choice
  corrupts layout for the other half of the world;
- borrowed `char*` arrays in the raw structs (`supra_ansi_token`,
  `supra_sandbox_policy`) are never handed out raw: `Token` owns its structure
  and borrows slices from `&self`; `Policy` owns its paths as `CString`s and
  materialises the raw form only for the duration of a call;
- `Process` kills and reaps on drop unless waited or detached - an agent harness
  spawns enough commands that leaked handles would be measured in hundreds.

## Building

`build.rs` configures CMake under `OUT_DIR` and links the three archives
statically, in reverse dependency order (`supra_width` after `supra_ansi`, since
a static linker resolves left to right). Two overrides:

- `SUPRA_CPP_BUILD_DIR` - reuse an existing configured tree (such as the one
  `just build-cpp` maintains) and skip the second build;
- `SUPRA_CXX` - override the compiler. Native builds default to `clang++` to
  match `just build-cpp`; cross builds select the target C++ compiler
  (`x86_64-linux-musl-g++` or `aarch64-linux-gnu-g++`) and link the matching
  runtime.

## Mutation results

Five deliberate mutations against the FFI invariants, via `scripts/mutate.sh`
(Rust branch: one `cargo test` invocation compiles and runs, so the log is
inspected to separate BUILD_FAIL from CAUGHT):

| Mutation | Verdict | Meaning |
|---|---|---|
| M1 `SIZEOF_SANDBOX_POLICY` 1104 → 1096 | BUILD_FAIL | the ratchet, working as designed - the `static_assert` is the enforcement point, not a test |
| M2 `Process::drop` no longer kills | CAUGHT | `dropping_a_handle_kills_the_process` asserts the pid is gone via `/proc` |
| M3 `Policy::allow` accepts relative paths | CAUGHT | a sandbox whose scope shifts with `chdir` is not a boundary |
| M4 `Tier::from_raw` misreports Landlock | CAUGHT | `probe_is_internally_consistent` fails: a degraded tier must explain itself |
| M5 `fixed_string` reads past the first NUL | CAUGHT | survived first, per the method rule; a load-bearing test now asserts the stop and the sign-preserving byte conversion |

M2 is also a harness story: the first attempt mutated to `self.pid` (a method,
not a field) and was correctly reported BUILD_FAIL - *not a verdict* - before
being fixed and re-run.
