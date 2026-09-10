//! Structural code operations for supra-harness.
//!
//! **T15.7** of the stage sequence: the call-graph precision T15's README
//! promises - cross-file name resolution, byte-range splice, and the reparse
//! gate - plus the outline and query machinery around them.
//!
//! # What this stage is for
//!
//! T15 answers "what is defined where" for the cohort's blast-radius estimate;
//! this crate answers "who refers to what" for the edit that follows. The
//! digest over-approximates by import edge (safe for scrutiny); the edit needs
//! the narrower answer (safe for rewriting). Both are syntactic: no type
//! resolution, no overload disambiguation - which is why every rename and
//! reference result carries `semantic: false`, stated as a field rather than
//! a footnote.
//!
//! # Shape
//!
//! | Module | Owns |
//! |---|
//! | [`splice`] | byte-range splice with the reparse gate |
//! | [`query`] | outline rendering plus reference search through imports |
//! | [`rename()`] | multi-file syntactic rename, atomic, shadow-checked |
//! | [`error`] | failures split by who must act |
//!
//! # Reuse, not duplication
//!
//! Grammars, `Language::detect`, `Symbol`, and the import graph all come from
//! T15 (`supra_digest`): the splicer parses what the indexer indexes, so a
//! file the digest can anchor is a file this gate can verify, and vice versa.
//! Reimplementing the harvest here would give two harvests that can disagree
//! about what a file declares.
//!
//! # Why the permission engine trusts this
//!
//! `replace_node` (this stage) passes a reparse gate with atomic rollback and
//! preserves formatting, so it is provably safer than a blind string-matching
//! `edit_file` on the same file - and T16.7 rates it one reversibility class
//! lower. Verified structure earning greater trust is measurable rather than
//! felt. `yolo` does not skip the gate: the gate is authority (correctness),
//! not consent.
//!
//! # Usage
//!
//! ```no_run
//! use std::path::PathBuf;
//!
//! let path = PathBuf::from("a.rs");
//! let source = b"fn alpha() {\n    1\n}\n".to_vec();
//! let end = source.iter().position(|byte| *byte == b'}').map(|index| index + 1).expect("a brace");
//! let out = supra_ast::replace_node(&path, &source, 0, end, "fn alpha() {\n    99\n}")?;
//! assert!(out.windows(9).any(|window| window == b"99\n}"));
//! # Ok::<(), supra_ast::AstError>(())
//! ```

#![deny(missing_docs)]
#![forbid(unsafe_code)]
// Tests assert with `.expect()` and `panic!`. Scoped to `cfg(test)` so no allow reaches a
// shipped path.
#![cfg_attr(test, allow(clippy::expect_used, clippy::panic, clippy::print_stderr))]

pub mod error;
pub mod query;
pub mod rename;
pub mod splice;

pub use error::AstError;
pub use query::{OutlineEntry, Reference, outline, query_references};
pub use rename::{RenameOutcome, RenamedFile, rename};
pub use splice::replace_node;

/// Splices run on the turn loop while the watcher re-indexes, so this is a
/// requirement rather than an observation.
const _: () = {
    const fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<OutlineEntry>();
    assert_send_sync::<RenameOutcome>();
};
