//! Why assembly can fail, and what each failure obliges.
//!
//! The variants split on *who must act*: the caller retrying will not help any of
//! these — every one is a defect in what was handed over (an over-budget index entry,
//! an unwritable store, a generation that cannot be proven), not a transient the next
//! attempt would survive. A caller that needs the remedy reads the message; a caller
//! that needs the category matches the variant.

use thiserror::Error;

use supra_types::{SegmentId, TurnId};

/// What ledger assembly or eviction can fail with.
#[derive(Clone, Debug, PartialEq, Eq, Error)]
#[non_exhaustive]
pub enum PromptError {
    /// A memory-index entry exceeds its byte budget.
    ///
    /// Refused rather than truncated: truncation would silently turn I4's ~15-token
    /// budget into a suggestion, and only the caller knows how to say the same thing
    /// more briefly. T6 owns the constructor that refuses;
    /// this variant carries the refusal across the ledger boundary.
    #[error("index entry for turn {turn} is {bytes} content bytes; the budget is 80")]
    IndexEntryTooLong {
        /// The turn the entry stands in for.
        turn: TurnId,
        /// Combined topic plus gist bytes offered.
        bytes: usize,
    },

    /// The store refused the eviction write.
    ///
    /// The turn stays live in the prefix: eviction commits to SQLite *before* the
    /// prefix drops the turn, so a failed commit leaves the ledger exactly as it was
    /// rather than half-evicted. T10 owns the underlying failure;
    /// this variant names the turn it stranded.
    #[error("eviction of turn {turn} failed before commit; the turn stays live: {detail}")]
    EvictionFailed {
        /// The turn that could not be evicted.
        turn: TurnId,
        /// The store's message.
        detail: String,
    },

    /// A recalled body does not verify against its digest.
    ///
    /// Returned with **no bytes**, like T10's own `Corrupt`: handing back altered
    /// content with a warning would let the model carry on with a conversation that
    /// is no longer what it contained, and nothing downstream could tell.
    #[error("recalled turn {turn} does not verify; no bytes returned")]
    RecallCorrupt {
        /// The turn whose bytes changed under a valid digest.
        turn: TurnId,
    },

    /// A recalled turn was never evicted.
    ///
    /// Not a fault: the caller asked for a turn the ledger still holds live, or for
    /// one that never existed. The remedy is to read the live prefix, not to retry.
    #[error("turn {turn} was never evicted; it is live or unknown")]
    RecallMissing {
        /// The turn that was asked for.
        turn: TurnId,
    },

    /// A generation seal cannot be proven: the ledger's segment sequence does not hash
    /// to the generation's recorded prefix hash.
    ///
    /// This is the hash guard firing. Either the session file was edited around the
    /// ledger, or a segment was appended without resealing — both are I1 violations,
    /// and resuming into a state whose cache accounting is wrong is worse than
    /// refusing to resume.
    #[error("generation {generation} does not verify: expected {expected}, recomputed {found}")]
    GenerationUnverified {
        /// Which generation failed the check.
        generation: u64,
        /// The prefix hash recorded at seal time.
        expected: String,
        /// The prefix hash recomputed from the segment sequence.
        found: String,
    },

    /// A segment id is already present in the ledger.
    ///
    /// Sequence positions are assigned by the ledger, never by the caller, so a
    /// duplicate id means the same sealed value was appended twice — a retry that
    /// must be idempotent, or a defect that must be loud. This variant is the loud
    /// half; idempotent retry is handled before it is reached.
    #[error("segment {id} is already in the ledger")]
    DuplicateSegment {
        /// The repeated segment identity.
        id: SegmentId,
    },
}
