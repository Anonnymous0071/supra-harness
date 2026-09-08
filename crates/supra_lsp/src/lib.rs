//! Language servers over one client: semantic references and crash
//! recovery. **T24** of the stage sequence.
//!
//! T15.7's contract is why this crate exists: syntactic rename and
//! reference results carry `semantic: false`, and T24 is the stage that
//! flips it - a language server proves which occurrences name the symbol
//! under shadowing and overloading.
//!
//! Five servers cover the digest's seven languages: rust-analyzer
//! (Rust), typescript-language-server (TypeScript + JavaScript),
//! pyright (Python), gopls (Go), clangd (C + C++). Coverage is by
//! lookup, never by guessing: a language with no server refuses
//! (`Uncovered`), the same refusal the digest's `Language::detect`
//! makes for unknown extensions.
//!
//! Crash recovery is one restart, not a loop: the client kills the
//! process, re-spawns, re-initializes, and answers the request from the
//! fresh instance. A server that dies twice is refused as `Crashed` -
//! retrying twice only hides a server that keeps dying.
//!
//! ```
//! use supra_digest::Language;
//! use supra_lsp::Server;
//!
//! assert_eq!(Server::for_language(Language::Rust), Some(Server::RustAnalyzer));
//! assert_eq!(
//!     Server::for_language(Language::Cpp),
//!     Some(Server::Clangd),
//!     "clangd covers C and C++"
//! );
//! assert_eq!(Server::for_language(Language::ALL[1]), Some(Server::TypeScript));
//! ```

#![deny(missing_docs)]
#![cfg_attr(test, allow(clippy::expect_used, clippy::panic, clippy::print_stderr))]

/// The live client: spawn, initialize, references, restart.
pub mod client;
/// The refusal shapes.
pub mod error;
/// LSP wire framing: Content-Length headers over stdio.
pub mod framing;
/// The five configured servers and their language coverage.
pub mod servers;

pub use client::{Client, SemanticReferences};
pub use error::LspError;
pub use framing::{Location, Position, Range};
pub use servers::Server;

const _: () = {
    const fn assert_send<T: Send>() {}
    assert_send::<Client>();
};
