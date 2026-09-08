//! Three debug adapters over one DAP client: breakpoints and stack
//! traces through the Debug Adapter Protocol. **T25** of the stage
//! sequence.
//!
//! DAP is LSP's framing with a different vocabulary: `seq`/`request_seq`
//! instead of `id`, `command` instead of `method`, and events the
//! adapter pushes (`stopped`, `terminated`) that a debugger must wait
//! for. The client covers the session shape the turn loop needs -
//! initialize, set a breakpoint, launch, read a stack trace, disconnect -
//! and refuses the languages whose debuggers the harness does not wire
//! (TypeScript, JavaScript), the same refusal T24 makes for uncovered
//! languages.
//!
//! ```
//! use supra_dap::Adapter;
//! use supra_digest::Language;
//!
//! assert_eq!(Adapter::for_language(Language::Rust), Some(Adapter::CodeLldb));
//! assert_eq!(Adapter::for_language(Language::Go), Some(Adapter::Delve));
//! assert_eq!(Adapter::for_language(Language::TypeScript), None);
//! ```

#![deny(missing_docs)]
#![cfg_attr(test, allow(clippy::expect_used, clippy::panic, clippy::print_stderr))]

/// The adapter catalogue and language coverage.
pub mod adapters;
/// The live client: initialize, breakpoint, launch, stack trace.
pub mod client;
/// The refusal shapes.
pub mod error;
/// DAP wire framing: Content-Length headers over stdio.
pub mod framing;

pub use adapters::Adapter;
pub use client::{Breakpoint, Client, StackTrace};
pub use error::DapError;
pub use framing::{Source, StackFrame};

const _: () = {
    const fn assert_send<T: Send>() {}
    assert_send::<Client>();
};
