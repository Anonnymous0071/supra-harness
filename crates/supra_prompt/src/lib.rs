//! Append-only prompt ledger for supra-harness.
//!
//! **T14** of the stage sequence: the ordered segments that form the prompt prefix,
//! the four breakpoints over them, lossless eviction into T10, generation rewrites,
//! and the hash guard that turns an invisible cost leak into a debuggable defect.
//!
//! # What this stage is for
//!
//! > Every turn recomputes the prefix hash locally and compares. An unexpected change
//! > emits `Event::CacheBreak` with the **causing diff**.
//!
//! Everything here serves I1 (append-only), I4 (lossless eviction), I5 (four
//! breakpoints), and I7 (hash guard). T6 owns the segment types and their canonical
//! encodings; this crate owns the *order* - the sequence, its hash, and the proof
//! that neither moved.
//!
//! # Module map
//!
//! | Module | Owns |
//! | ------ | ---- |
//! | `ledger` | append-only sequence, sequence numbers, prefix hash, generation seals |
//! | `breakpoints` | four offsets over the sequence, lookback bound, policy truncation |
//! | `evict` | verbatim-to-SQLite-first ordering, T13.5 thinking disposition, recall |
//! | `generation` | one rewrite at 92-95% while idle, order-preserving, auditable |
//! | `error` | failures split by who must act |
//!
//! # Usage
//!
//! ```no_run
//! use supra_prompt::{Plan, PromptLedger, plan_breakpoints};
//! use supra_types::{Block, Role, Segment, SegmentId, SegmentKind, TurnId};
//!
//! let mut ledger = PromptLedger::new();
//! let segment = Segment::new(
//!     SegmentId::generate(),
//!     SegmentKind::Turn { turn: TurnId::generate(), role: Role::User },
//!     vec![Block::Text("hello".to_owned())],
//! )
//! .expect("well-formed");
//! ledger.append(segment).expect("append");
//!
//! let plan: Plan = plan_breakpoints(ledger.segments())?;
//! assert_eq!(plan.bp4_previous, 1);
//! # Ok::<(), supra_prompt::PromptError>(())
//! ```

#![deny(missing_docs)]
#![forbid(unsafe_code)]
// Tests assert with `.expect()` and `panic!`. Scoped to `cfg(test)` so no allow reaches a
// shipped path.
#![cfg_attr(test, allow(clippy::expect_used, clippy::panic, clippy::print_stderr))]

pub mod breakpoints;
pub mod error;
pub mod evict;
pub mod generation;
pub mod ledger;

pub use breakpoints::{
    Plan, check_lookback, find_reversal, plan_breakpoints, plan_breakpoints_at, truncate_for_policy, validate,
};
pub use error::PromptError;
pub use evict::{
    ThinkingDisposition, evict_turn, index_entry, index_segment, plan_eviction, recall_turn, render_body,
};
pub use generation::{needs_rewrite, rewrite, verify_rewrite};
pub use ledger::{Generation, LedgerSnapshot, PromptLedger};

/// The ledger is shared between the turn loop and recall tasks, so this is a
/// requirement rather than an observation.
const _: () = {
    const fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<PromptLedger>();
};
