//! The shared peer blackboard: claims, per-claim votes with proposer
//! exclusion, incremental quorum, rotating roles.
//!
//! **T21**. §4's contract, as types: no agent is privileged; peers
//! publish claims to one shared board, validators vote, quorum is
//! `ceil(2k/3)` computed rationally in `supra_types`, and publication
//! contributes the proposer's affirmative vote without creating a validator
//! row; proposer roles attach per claim and rotate within a turn.
//!
//! Every vote updates an incremental quorum tally and returns the
//! claim's new status, so the turn loop (T23) evaluates after every vote:
//! reached aborts in-flight peers, unreachable escalates without waiting
//! for a timeout. Claims and votes persist to SQLite through
//! `schema_component` (`"blackboard"`), the fourth owner after T11,
//! T16.6, and T18.
//!
//! ```
//! use std::sync::Arc;
//! use supra_blackboard::Blackboard;
//! use supra_store::Store;
//! use supra_types::{AgentId, Confidence, TurnId, Verdict, Vote};
//!
//! let store = Arc::new(Store::open_in_memory().expect("store"));
//! let mut board = Blackboard::open(store).expect("board");
//!
//! let proposer = AgentId::generate();
//! let validators = [AgentId::generate(), AgentId::generate()];
//! let claim = board
//!     .publish(TurnId::generate(), proposer, &validators, "the fix")
//!     .expect("publish");
//!
//! let yes = Verdict::new(Vote::Yes, Confidence::High, "holds", None).expect("verdict");
//! let outcome = board.vote(claim, validators[0], &yes).expect("vote");
//! assert!(outcome.is_reached());
//! # Ok::<(), supra_blackboard::BlackboardError>(())
//! ```

#![deny(missing_docs)]
#![cfg_attr(test, allow(clippy::expect_used, clippy::panic, clippy::print_stderr))]

/// The board: publish, vote, outcomes, rotation.
pub mod board;
/// The refusal shapes.
pub mod error;
/// The blackboard's tables in the shared store.
pub mod schema;

pub use board::{Blackboard, MAX_BODY_BYTES, Outcome};
pub use error::BlackboardError;
pub use schema::{COMPONENT, MIGRATIONS, StoredClaim, StoredVote};

const _: () = {
    const fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<Blackboard>();
    assert_send_sync::<Outcome>();
    assert_send_sync::<StoredClaim>();
    assert_send_sync::<StoredVote>();
};
