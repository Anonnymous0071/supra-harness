//! What the permission gate refused, and who must act.
//!
//! One refusal shape and one question shape. A refusal is final for this
//! request: the gate never queues what it refused (a plan that queues its
//! refusals is a staging area, not a plan). A question is batchable - the
//! whole point of T16.7's batching is that many questions become one prompt.

use thiserror::Error;

/// The permission gate's refusal.
#[derive(Clone, Debug, PartialEq, Eq, Error)]
#[non_exhaustive]
pub enum PermissionError {
    /// The rule set denies this request outright.
    ///
    /// Deny is absolute (T6's literal reading), so this is not a question
    /// the user can answer differently in the same session: the rule must
    /// change, and the rule's owner is the one to change it.
    #[error("a rule from {origin} denies this request")]
    Denied {
        /// Where the denying rule came from.
        origin: supra_types::RuleSource,
    },

    /// The requested batch is empty.
    ///
    /// An empty batch that produced an empty answer would let a caller
    /// report "the user approved nothing" as a decision, when no question
    /// was ever asked. Refusing keeps the two states distinguishable.
    #[error("an empty batch cannot be asked")]
    EmptyBatch,
}
