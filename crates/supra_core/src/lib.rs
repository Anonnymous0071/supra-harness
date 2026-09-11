//! The turn loop. **T23**: thirteen steps, one session, no orchestrator.
//!
//! Section 9's loop, driven one vote at a time. The loop owns no LLM
//! client: answers arrive through `Turn::record`, so the choreography -
//! publish, evaluate per vote, abort on reach, escalate on unreachable -
//! is testable without a network, and the runtime (T30) supplies
//! answers as its shards complete.
//!
//! The three test-enforced properties of section 9, restated as this
//! crate's contract: a turn never waits on a late peer while quorum
//! remains reachable (each `record` returns the step, and `Collecting`
//! means reachable); unreachable quorum escalates immediately, never
//! after a timeout; and every vote is an event, so the TUI never
//! blocks.
//!
//! ```
//! use std::sync::Arc;
//! use supra_blackboard::Blackboard;
//! use supra_core::{PeerAnswer, Step, Turn};
//! use supra_eventbus::Bus;
//! use supra_types::{AgentId, Confidence, Verdict, Vote};
//!
//! let store = Arc::new(supra_store::Store::open_in_memory().expect("store"));
//! let board = Blackboard::open(store).expect("board");
//! let agents = [AgentId::generate(), AgentId::generate(), AgentId::generate()];
//!
//! let mut turn =
//!     Turn::start(board, Bus::new(), "the fix", agents.to_vec()).expect("turn");
//! assert_eq!(
//!     turn.record(PeerAnswer::proposal(agents[0], "fix it in place"))?,
//!     Step::Collecting
//! );
//! let verdict = Verdict::new(Vote::Yes, Confidence::Medium, "checked independently", None)
//!     .expect("valid verdict");
//! let step = turn.record(PeerAnswer::validation(agents[1], verdict))?;
//! assert_eq!(step, Step::Collecting);
//! # Ok::<(), supra_core::TurnError>(())
//! ```

#![deny(missing_docs)]
#![cfg_attr(test, allow(clippy::expect_used, clippy::panic, clippy::print_stderr))]

/// The refusal shapes.
pub mod error;
/// The turn state machine.
pub mod turn;

pub use error::TurnError;
pub use turn::{PeerAnswer, Step, Turn};

const _: () = {
    const fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<Turn>();
    assert_send_sync::<PeerAnswer>();
    assert_send_sync::<Step>();
};
