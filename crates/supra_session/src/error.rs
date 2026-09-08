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
