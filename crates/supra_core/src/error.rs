use thiserror::Error;

/// A turn-loop refusal.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum TurnError {
    /// The ledger refused the append.
    #[error("the ledger refused: {0}")]
    Ledger(#[from] supra_prompt::PromptError),

    /// The segment refused construction.
    #[error("the segment refused: {0}")]
    Segment(#[from] supra_types::SegmentError),

    /// A peer produced no answer the loop could use.
    #[error("no usable answer this turn: {0}")]
    NoAnswer(String),

    /// The cohort's quorum was reached but the claim body is empty.
    #[error("the winning claim carries no body")]
    EmptyClaim,
}
