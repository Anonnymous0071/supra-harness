//! Repo digest for supra-harness.
//!
//! **T15** of the stage sequence: orientation with zero LLM calls. A tree-sitter
//! symbol index, a dependency graph, and git churn, maintained incrementally by a
//! file watcher; a local hybrid BM25 + vector retrieval selects ~10 anchors, or
//! ~300 tokens of precise pointers, appended in the suffix.
//!
//! # What this stage is for
//!
//! Turn step 1 is `digest.retrieve(task) -> ~300 token anchors (0 LLM calls)`.
//! Everything here serves that: fast enough to disappear inside a turn
//! (per-turn overhead, no LLM: < 50 ms), cheap enough in memory to sit beside a
//! coding session, and exact enough that an anchor is the one the corpus would
//! have chosen.
//!
//! # Shape
//!
//! | Module | Owns |
//! |---|
//! | [`symbol`] | what a symbol is: name, kind, byte range, fingerprint |
//! | [`parse`] | bytes into symbols, one grammar per language, errors refused |
//! | [`index`] | what is defined where: scan, fingerprint skip, watcher patch |
//! | [`graph`] | what imports what: edges, blast radius, churn |
//! | [`anchors`] | ~10 pointers, ~300 tokens: gists, budget, suffix rendering |
//! | [`digest`] | the four parts composed: open, retrieve, and watcher events |
//!
//! # Zero LLM calls, structurally
//!
//! Nothing here opens a provider client, formats a prompt, or parses a
//! completion. Gists are deterministic strings from symbol records; ranks are
//! integer arithmetic over T11's lanes. This crate does not depend on
//! `supra_llm`, and the cohort (T15.5) declares the same direction - enforced
//! there by a negative test, here by the manifest.
//!
//! # Usage
//!
//! ```no_run
//! use std::sync::Arc;
//! use supra_store::Store;
//!
//! let store = Arc::new(Store::open("/tmp/supra/store.db")?);
//! let digest = supra_digest::Digest::open("/repo".into(), Arc::clone(&store))?;
//! let vector = supra_vector::VectorIndex::open(
//!     store,
//!     "model",
//!     8,
//!     &[0.0; 8],
//!     supra_vector::IndexOptions::default(),
//! )?;
//! let anchors = digest.retrieve("recall the turn", &|_| None, &vector)?;
//! assert!(anchors.len() <= supra_digest::MAX_ANCHORS);
//! # Ok::<(), supra_digest::DigestError>(())
//! ```

#![deny(missing_docs)]
#![forbid(unsafe_code)]
// Tests assert with `.expect()` and `panic!`. Scoped to `cfg(test)` so no allow reaches a
// shipped path.
#![cfg_attr(test, allow(clippy::expect_used, clippy::panic, clippy::print_stderr))]

pub mod anchors;
pub mod digest;
pub mod error;
pub mod graph;
pub mod index;
pub mod parse;
pub mod symbol;

pub use anchors::{
    Anchor, BYTES_PER_TOKEN, MAX_ANCHORS, SUFFIX_TOKENS, check_budget, gist_for_entry, gist_for_symbol,
    render_suffix,
};
pub use digest::{DEFAULT_RESCAN_SECS, Digest};
pub use error::DigestError;
pub use graph::{DependencyGraph, Edge, churn, import_targets};
pub use index::{ScanStats, SymbolIndex, scan_tree};
pub use symbol::{Fingerprint, Language, Symbol, SymbolKind};

/// The digest is opened once and queried from the turn loop while the watcher
/// patches it, so this is a requirement rather than an observation.
const _: () = {
    const fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<Digest>();
};
