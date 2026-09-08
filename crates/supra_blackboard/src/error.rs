use thiserror::Error;

/// A blackboard refusal.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum BlackboardError {
    /// The store refused.
    #[error("the store refused: {0}")]
    Store(#[from] supra_store::StoreError),

    /// The blackboard's own tables refused.
    #[error("the blackboard's tables refused: {0}")]
    Sqlite(#[from] rusqlite::Error),

    /// The claim id names no claim on this board.
    #[error("claim {claim} does not exist")]
    UnknownClaim {
        /// The missing claim.
        claim: supra_types::ClaimId,
    },

    /// The agent has already voted on this claim.
    #[error("agent {agent} already voted on claim {claim}")]
    DuplicateVote {
        /// The repeat voter.
        agent: supra_types::AgentId,
        /// The claim.
        claim: supra_types::ClaimId,
    },

    /// The agent is not one of the claim's k-1 validators.
    #[error("agent {agent} is not part of the cohort for claim {claim}")]
    NotInCohort {
        /// The outsider.
        agent: supra_types::AgentId,
        /// The claim.
        claim: supra_types::ClaimId,
    },

    /// The claim is no longer open.
    #[error("claim {claim} is closed")]
    Closed {
        /// The closed claim.
        claim: supra_types::ClaimId,
    },

    /// The quorum tally refused.
    #[error("the tally refused: {0}")]
    Tally(#[from] supra_types::TallyError),

    /// A proposer tried to vote on its own claim.
    #[error("agent {agent} is not a candidate proposer for claim {claim}")]
    NotAProposer {
        /// The proposer.
        agent: supra_types::AgentId,
        /// Its own claim.
        claim: supra_types::ClaimId,
    },
}
