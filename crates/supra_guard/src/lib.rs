//! Seven anti-self-spawn guard layers: one model must not spawn itself.
//!
//! **T12.5** of the stage sequence. The structural half lives in T6
//! ([`supra_types::Lineage`]: depth 1, no cycles, revalidated on load); this crate is the
//! physical half - proving that a command about to be executed is not this harness - plus
//! the vote rule that keeps a proposer from counting toward its own claim's quorum.
//!
//! # The layers
//!
//! | Layer | Refuses | Mechanism |
//! | ----- | ------- | --------- |
//! | L1 | no identity established | [`identity::is_established`] |
//! | L2 | no marker key generated | [`marker::has_key`] |
//! | L3 | the command names this binary | argv[0], its final component, and its resolved name |
//! | L4 | the command *is* this binary | (device, inode) via `supra_ffi` |
//! | L5 | the marker does not authenticate | HMAC-SHA256 over `version:nonce` |
//! | L6 | the lineage would cycle or nest | [`supra_types::Lineage::child`] |
//! | L7 | a proposer voting its own claim | voter id against proposer id |
//!
//! L1/L2 are readiness, not judgement. L3/L4 are about *this binary*; L5 covers the copy L4
//! cannot see; L6 is about *this agent*; L7 is about *this claim*. Four different questions,
//! which is why one layer cannot substitute for another.
//!
//! # No off switch
//!
//! There is none. `yolo` relaxes consent (T16.7); it does not touch authority, and these
//! layers are authority. Every layer runs on every judgement even after one refuses, because
//! the caller reports *all* refusals: "refused by L3, L4, and L5" tells the operator what was
//! attempted, where "refused by L3" only says what happened.
//!
//! # Usage
//!
//! ```no_run
//! use supra_guard::{establish_identity, generate_marker_key, judge, SpawnRequest};
//! use supra_types::{AgentId, Lineage};
//!
//! establish_identity();
//! generate_marker_key();
//!
//! let host = AgentId::generate();
//! let lineage = Lineage::root(host);
//! let marker = supra_guard::issue_marker().expect("keyed").expect("a marker");
//! let argv = ["/bin/sh", "-c", "cargo test"];
//! let verdict = judge(&SpawnRequest {
//!     argv: &argv,
//!     marker: Some(&marker),
//!     lineage: Some(&lineage),
//!     child: Some(AgentId::generate()),
//!     claim_proposer: None,
//!     voter: None,
//! });
//! assert!(verdict.allowed());
//! # Ok::<(), supra_guard::Refusal>(())
//! ```

#![deny(missing_docs)]
#![forbid(unsafe_code)]
// Tests assert with `.expect()` and `panic!`. Scoped to `cfg(test)` so no allow reaches a
// shipped path.
#![cfg_attr(test, allow(clippy::expect_used, clippy::panic, clippy::print_stderr))]

pub mod error;
pub mod identity;
pub mod layers;
pub mod marker;

pub use error::Refusal;
pub use identity::{ProcessIdentity, current, establish as establish_identity, is_established};
pub use layers::{SpawnRequest, Verdict, judge, layer_of};
pub use marker::{
    MARKER_ENV, generate_key as generate_marker_key, has_key, issue as issue_marker, verify as verify_marker,
};

/// The guard judges spawns from the turn loop and from tool invocations on other tasks, so
/// this is a requirement rather than an observation.
const _: () = {
    const fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<ProcessIdentity>();
};
