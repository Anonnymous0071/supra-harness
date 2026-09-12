//! Session persistence, resume, branch, and export. **T26** of the
//! stage sequence.
//!
//! A session is the identity a resume restores: its id, its completed
//! turns, and the file both live in. Save writes under the session's
//! own id; resume reads it back or answers `None` (absent is a state,
//! not an error); branch copies the turns under a fresh id without
//! touching the original file - a branch is a copy, not a move; export
//! renders the transcript as Markdown.
//!
//! ```
//! use supra_session::Session;
//! use supra_types::TurnId;
//!
//! let mut session = Session::new();
//! session.record_turn(TurnId::generate());
//! assert_eq!(session.turns().len(), 1);
//!
//! let branched = session.branch();
//! assert_ne!(branched.id(), session.id());
//! assert_eq!(branched.turns().len(), 1);
//! ```

#![deny(missing_docs)]
#![cfg_attr(test, allow(clippy::expect_used, clippy::panic, clippy::print_stderr))]

/// Versioned, lossless checkpoint types.
pub mod checkpoint;
/// The refusal shapes.
pub mod error;
/// The session type.
pub mod session;
/// The on-disk store.
pub mod store;

pub use checkpoint::{
    CHECKPOINT_VERSION, CheckpointTurn, LifecycleStatus, ProtocolMessage, SessionCheckpoint,
};
pub use error::SessionError;
pub use session::Session;
pub use store::{list, load, load_checkpoint, save, save_checkpoint};

const _: () = {
    const fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<Session>();
    assert_send_sync::<SessionCheckpoint>();
};
