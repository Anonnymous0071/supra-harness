//! Safe Rust bindings to the supra-harness C++20 libraries.
//!
//! **T5** of the stage sequence, and the only crate in the workspace permitted to
//! contain `unsafe`.
//!
//! # What this crate is for
//!
//! Three C++20 libraries sit beneath supra, each behind a flat C ABI:
//!
//! | Library | Stage | Provides |
//! | ------- | ----- | -------- |
//! | `libsupra_width` | T2 | cell width, grapheme segmentation, Unicode 17 tables |
//! | `libsupra_ansi` | T3 | escape parsing, SGR state, style-safe truncation |
//! | `libsupra_sandbox` | T4 | native process isolation: Linux namespaces/Landlock, macOS `sandbox_init`/SBPL, and Windows `AppContainer` with explicit handle inheritance and a job object |
//!
//! This crate wraps all three so that nothing above it needs `unsafe`. That
//! confinement is a hard rule rather than a preference: `unsafe_code = "warn"` is
//! set workspace-wide and re-allowed only here, so a soundness bug has exactly
//! one crate to hide in.
//!
//! # The layout ratchet
//!
//! The `extern` declarations in the private `sys` module are hand-written, not generated. The ABI
//! is small, stable, and authored in this repository, so a code generator would
//! add a build dependency for no benefit - but hand-writing carries a real risk:
//! a field added to a C++ header without a matching Rust change produces a layout
//! mismatch that **does not crash**. The Rust side simply reads the wrong offsets
//! and returns wrong answers.
//!
//! `abi_sizes.rs` closes that hole from both directions at compile time:
//!
//! - `build.rs` feeds every size and alignment to `abi_check.cpp`, where a
//!   `static_assert` compares it against the real C++ `sizeof`.
//! - the private `sys` module asserts the same constants against Rust's `size_of` and `align_of`.
//!
//! A divergence fails to build on one side or the other, and neither check needs
//! to run - which also keeps them valid when cross-compiling.
//!
//! # Design notes
//!
//! **Sentinels become types.** The C ABI returns `-1` for "not printable" and
//! `0`/`1` for booleans. Those become [`width::Width`] and real `bool`s, so a
//! caller cannot accidentally sum a sentinel into a running total.
//!
//! **Ambiguity stays a parameter.** [`width::Ambiguous`] is threaded through every
//! measurement rather than resolved once, because the correct answer depends on
//! the terminal's locale and baking in either choice corrupts layout for the other
//! half of the world.
//!
//! **Borrowed data stays borrowed.** [`ansi::Token`] holds the raw structure by
//! value and hands out slices from `&self`, so reading a payload or parameter list
//! allocates nothing.
//!
//! **Lifetimes are owned, not documented.** The raw `supra_sandbox_policy` stores
//! borrowed `char*` pointers. [`sandbox::Policy`] owns its paths as `CString`s and
//! materialises the raw form only for the duration of a call, so the hazard cannot
//! reach a caller.
//!
//! **Processes clean up.** [`sandbox::Process`] kills and reaps on drop. An agent
//! harness spawns enough commands that a leaked handle would be measured in
//! hundreds.
//!
//! # Building
//!
//! `build.rs` configures `CMake` under `OUT_DIR` and links the archives statically.
//! Set `SUPRA_CPP_BUILD_DIR` to an existing configured tree to reuse it; set
//! `SUPRA_CXX` to override the compiler, which defaults to `clang++` to match
//! `just build-cpp`.

// The one crate allowed to contain unsafe. Every block carries a SAFETY comment,
// and `unsafe_op_in_unsafe_fn` is denied so an unsafe function body does not get
// an implicit licence.
#![allow(unsafe_code)]
#![deny(unsafe_op_in_unsafe_fn)]
#![deny(missing_docs)]
// Tests assert invariants with `.expect()` and `panic!` - that is how a failing
// test reports itself - and announce environment-dependent skips on stderr,
// since T8 supra_log does not exist to receive them. The workspace bans those to
// keep a long-running agent process clean, which is not a property tests have.
// Scoped to `cfg(test)` so no allow ever reaches a shipped code path.
#![cfg_attr(test, allow(clippy::expect_used, clippy::panic, clippy::print_stderr))]

pub mod ansi;
pub mod fd;
pub mod piped;
pub mod process;
// Pseudo-terminals are a Unix facility; the module's `posix_openpt`,
// `grantpt`, and `ioctl` constants are pinned to Linux and macOS kernels,
// and a `compile_error!` inside guards any other Unix. Windows has no pty
// API here yet, so the module does not exist there at all - callers gate
// on `cfg(unix)`, never on "is it linked".
#[cfg(unix)]
pub mod pty;
pub mod sandbox;
pub mod width;

mod sys;

/// Confirm the C++ libraries are linked and their layout assertions were compiled.
///
/// Returns 0. Exists so a binary can force the ABI-check translation unit to be
/// retained, and as a trivial smoke test that linking worked at all - a missing
/// archive fails at link time rather than at first use, which is easier to
/// diagnose.
#[must_use]
pub fn linkage_check() -> i32 {
    sys::touch_abi_check()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn libraries_are_linked() {
        assert_eq!(linkage_check(), 0);
    }

    #[test]
    fn the_three_libraries_agree_on_measurement() {
        // A cross-library invariant, and the reason they live in one crate:
        // libsupra_ansi asks libsupra_width for every width, so a styled string
        // must measure the same as its stripped form.
        let styled = b"\x1b[31m\xE4\xB8\xAD\x1b[0mtext";

        let mut stripped = Vec::new();
        ansi::strip(styled, &mut stripped);

        let via_ansi = ansi::measure(styled, width::Ambiguous::Narrow);
        let via_width = width::width(&stripped, width::Ambiguous::Narrow);

        assert_eq!(via_ansi, via_width, "escapes must contribute no cells");
    }

    #[test]
    fn unicode_version_is_the_vendored_one() {
        // Pinned so a table regeneration is a visible change rather than a silent
        // one: a different Unicode version can change how a line is measured.
        assert_eq!(width::unicode_version(), "17.0.0");
    }
}
