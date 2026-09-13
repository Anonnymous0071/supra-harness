use supra_types::TurnId;
use thiserror::Error;

/// A session refusal.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum SessionError {
    /// The store refused: I/O, or a file this build will not read.
    #[error("the session store refused: {0}")]
    Io(#[from] std::io::Error),

    /// The file's shape is not a session file.
    #[error("the session file is malformed: {0}")]
    Malformed(String),

    /// A checkpoint was written by a newer, unsupported format.
    #[error("unsupported session checkpoint version {found}")]
    UnsupportedVersion {
        /// Version found in the checkpoint envelope.
        found: u32,
    },

    /// A compare-and-swap save observed another committed revision.
    #[error("session revision conflict: expected {expected}, found {found}")]
    RevisionConflict {
        /// Revision the writer based its update on.
        expected: u64,
        /// Revision currently committed on disk.
        found: u64,
    },

    /// A checkpoint revision did not advance the committed revision.
    #[error("session revision must advance: incoming {incoming}, current {current}")]
    NonMonotonicRevision {
        /// Revision the writer attempted to commit.
        incoming: u64,
        /// Revision currently committed on disk.
        current: u64,
    },

    /// The revision counter cannot advance beyond its maximum value.
    #[error("session revision {revision} cannot advance beyond u64::MAX")]
    RevisionExhausted {
        /// Revision at which no successor can be represented.
        revision: u64,
    },

    /// A completed turn id was recorded more than once.
    #[error("session already contains turn {turn}")]
    DuplicateTurn {
        /// Repeated turn identity.
        turn: TurnId,
    },

    /// A legacy projection would erase committed lossless checkpoint state.
    #[error("legacy session save would lose checkpoint state: {reason}")]
    LossyLegacySave {
        /// The incompatible part of the committed checkpoint.
        reason: String,
    },

    /// A ledger replay hit a segment the builder refused.
    #[error("the ledger refused a replayed segment: {0}")]
    Ledger(#[from] supra_prompt::PromptError),

    /// A branch target that does not name a checkpoint.
    #[error("no checkpoint at {position}")]
    NoCheckpoint {
        /// The sequence position asked for.
        position: u64,
    },
}
