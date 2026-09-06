//! Lossless eviction: verbatim to SQLite first, index entry after.
//!
//! # The one ordering rule
//!
//! **Commit the eviction before dropping the turn from the prefix.** T10's module
//! documentation states it, and this module is where it is honoured: `evict` writes
//! the body to the store, and only on success returns the [`MemoryIndexEntry`] that
//! replaces the turn. No setting in T10 can make the other order safe - a lost commit
//! after the prefix already dropped the turn is a lost conversation, not a lost cache
//! entry.
//!
//! # What is preserved, per T13.5
//!
//! Eviction preserves verbatim everything the API requires, and drops only what the
//! API declares omissible:
//!
//! 1. Turns containing `tool_use` keep their thinking blocks verbatim, signature
//!    included - required, and a 400 otherwise.
//! 2. Turns without `tool_use` may drop thinking blocks - allowed, silently accepted.
//! 3. `redacted_thinking` blocks are preserved wherever they appear.
//! 4. Consecutive thinking blocks keep generation order - an evicted turn resent via
//!    `recall` is a message again, and the API checks the latest message's sequence.
//! 5. Never `clear_thinking_20251015`: server-side clearing invalidates cache at the
//!    clearing point, and eviction already reclaimed the space losslessly.
//!
//! The evicted body is the turn's canonical encoding (T6), not a re-rendering: the
//! store's byte-identical promise is measured against bytes this crate produces
//! deterministically, and a renderer that drifts across versions would break recall
//! without breaking any test in this crate.
//!
//! # What the index entry is not
//!
//! A summary. I4 forbids summarisation because provider documentation lists it as
//! cache-invalidating: a harness that summarises at 80% context destroys its prefix
//! and pays to rebuild it. The entry is a pointer (topic, range, one-line gist),
//! bounded to 80 content bytes by T6's constructor - refused, never truncated.

use supra_types::{Block, MemoryIndexEntry, Sealed, Segment, SegmentKind, TurnId};

use crate::error::PromptError;

/// Whether a turn's body keeps its thinking blocks on eviction.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ThinkingDisposition {
    /// The turn contains `tool_use`: thinking stays verbatim, signature included.
    Keep,
    /// The turn has no `tool_use`: thinking may go, silently accepted.
    Drop,
}

impl ThinkingDisposition {
    /// Decide from the turn's blocks. `redacted_thinking` is not a `Block` variant
    /// here - T6 models redaction as a signature-bearing `Thinking` - so presence of
    /// `ToolUse` is the whole decision.
    #[must_use]
    pub fn decide(blocks: &[Block]) -> Self {
        if blocks.iter().any(|block| matches!(block, Block::ToolUse { .. })) {
            Self::Keep
        } else {
            Self::Drop
        }
    }
}

/// Render a turn's blocks into the eviction body, applying the T13.5 disposition.
///
/// `Keep` renders every block verbatim.
///
/// `Drop` renders every block except `Thinking`. A `ToolResult` without its `ToolUse`
/// can never reach here: a turn holding a result always holds the call, because
/// dropping the call while keeping the result would leave an unanswered invocation,
/// which every provider rejects.
#[must_use]
pub fn render_body(blocks: &[Block], disposition: ThinkingDisposition) -> Vec<Block> {
    match disposition {
        ThinkingDisposition::Keep => blocks.to_vec(),
        ThinkingDisposition::Drop => {
            blocks.iter().filter(|block| !matches!(block, Block::Thinking { .. })).cloned().collect()
        }
    }
}

/// Evict one live turn: commit verbatim to the store, then describe the index entry.
///
/// `body` is the canonical encoding of the turn's blocks (T6) - bytes, not a
/// re-rendering. The store commits first; only on success does this return the entry
/// that replaces the turn in the prefix. A failed commit leaves the ledger exactly
/// as it was: the turn stays live, and the error names it.
///
/// # Errors
///
/// [`PromptError::EvictionFailed`] when the store refuses the write. The turn stays
/// live; nothing is half-evicted.
pub fn evict_turn(
    store: &supra_store::Store,
    turn: TurnId,
    body: &[u8],
) -> Result<supra_types::ContentHash, PromptError> {
    store
        .evict_turn(turn, body)
        .map_err(|error| PromptError::EvictionFailed { turn, detail: error.to_string() })
}

/// Build the index entry that replaces an evicted turn.
///
/// Topic, range, and one-line gist in 80 content bytes. Refused when over budget -
/// truncation would silently turn I4's budget into a suggestion, and only the caller
/// (T15's digest, which knows the turn's symbols) knows how to say it more briefly.
///
/// # Errors
///
/// [`PromptError::IndexEntryTooLong`] when topic plus gist exceed the budget.
pub fn index_entry(turn: TurnId, topic: String, gist: String) -> Result<MemoryIndexEntry, PromptError> {
    let bytes = topic.len() + gist.len();
    MemoryIndexEntry::new(turn, topic, gist).map_err(|_| PromptError::IndexEntryTooLong { turn, bytes })
}

/// Recall a turn byte-identically, verifying before returning.
///
/// # Errors
///
/// [`PromptError::RecallMissing`] when the turn was never evicted - it is live or
/// unknown, and the remedy is to read the live prefix, not to retry.
/// [`PromptError::RecallCorrupt`] when the bytes no longer verify - returned with
/// **no bytes**, like T10's own refusal.
pub fn recall_turn(store: &supra_store::Store, turn: TurnId) -> Result<Vec<u8>, PromptError> {
    use supra_store::StoreError;
    match store.recall_turn(turn) {
        Ok(body) => Ok(body),
        Err(StoreError::NotFound { .. }) => Err(PromptError::RecallMissing { turn }),
        Err(StoreError::Corrupt { .. }) => Err(PromptError::RecallCorrupt { turn }),
        Err(other) => Err(PromptError::EvictionFailed { turn, detail: other.to_string() }),
    }
}

/// The sealed segment for an evicted turn's index entry.
///
/// A `MemoryIndex` segment carries its content in the entry, never in blocks: T6's
/// constructor refuses blocks on it, so a parallel free-text copy cannot become a
/// second source of truth.
///
/// Returns `None` when the entry violates the constructor's budget - impossible for
/// an entry built by [`index_entry`], possible for one deserialised from a hand-edited
/// session file. `None` there means "do not seal this", not "crash the turn loop".
#[must_use]
pub fn index_segment(entry: MemoryIndexEntry, id: supra_types::SegmentId) -> Option<Segment> {
    Segment::new(id, SegmentKind::MemoryIndex(entry), vec![]).ok()
}

/// Decide, render, and describe: the full eviction of one turn's sealed segment.
///
/// Reads the turn's blocks from the sealed segment, applies the T13.5 disposition,
/// and returns the body bytes plus the disposition taken - so the caller (T23) can
/// commit the body and then append the index segment, in that order, with nothing
/// implied.
#[must_use]
pub fn plan_eviction(segment: &Sealed<Segment>) -> (Vec<Block>, ThinkingDisposition) {
    let disposition = ThinkingDisposition::decide(segment.blocks());
    let body = render_body(segment.blocks(), disposition);
    (body, disposition)
}

#[cfg(test)]
mod tests {
    use super::*;
    use supra_types::{CanonicalJson, Role, Sealed, SegmentId, SeqNo};

    fn tool_turn() -> Sealed<Segment> {
        Sealed::seal(
            SeqNo::ZERO,
            Segment::new(
                SegmentId::generate(),
                SegmentKind::Turn { turn: TurnId::generate(), role: Role::Assistant },
                vec![
                    Block::Thinking { text: "let me read".to_owned(), signature: Some("sig".to_owned()) },
                    Block::ToolUse {
                        call_id: "c1".to_owned(),
                        name: "read".to_owned(),
                        input: CanonicalJson::empty_object(),
                    },
                    Block::ToolResult { call_id: "c1".to_owned(), content: "ok".to_owned(), is_error: false },
                ],
            )
            .expect("tool run"),
        )
    }

    #[test]
    fn a_tool_turn_keeps_its_thinking_verbatim() {
        let segment = tool_turn();
        let (body, disposition) = plan_eviction(&segment);
        assert_eq!(disposition, ThinkingDisposition::Keep);
        assert_eq!(body, segment.blocks().to_vec(), "verbatim means verbatim");
        assert!(
            body.iter().any(|block| matches!(block, Block::Thinking { signature: Some(_), .. })),
            "the signature must survive: the API verifies provenance"
        );
    }

    #[test]
    fn a_prose_turn_drops_its_thinking() {
        let segment = Sealed::seal(
            SeqNo::ZERO,
            Segment::new(
                SegmentId::generate(),
                SegmentKind::Turn { turn: TurnId::generate(), role: Role::Assistant },
                vec![
                    Block::Thinking { text: "hmm".to_owned(), signature: None },
                    Block::Text("answer".to_owned()),
                ],
            )
            .expect("prose with thinking"),
        );
        let (body, disposition) = plan_eviction(&segment);
        assert_eq!(disposition, ThinkingDisposition::Drop);
        assert_eq!(body, vec![Block::Text("answer".to_owned())]);
    }

    #[test]
    fn order_survives_either_disposition() {
        // T13.5 rule 4: consecutive thinking blocks keep generation order, because a
        // recalled turn is a message again and the API checks the latest sequence.
        let segment = tool_turn();
        let (body, _) = plan_eviction(&segment);
        let thinking: Vec<&Block> =
            body.iter().filter(|block| matches!(block, Block::Thinking { .. })).collect();
        let original: Vec<&Block> =
            segment.blocks().iter().filter(|block| matches!(block, Block::Thinking { .. })).collect();
        assert_eq!(thinking, original);
    }

    #[test]
    fn an_over_budget_entry_is_refused_not_truncated() {
        let turn = TurnId::generate();
        let error = index_entry(turn, "t".repeat(70), "g".repeat(20)).expect_err("over budget");
        assert!(matches!(error, PromptError::IndexEntryTooLong { .. }), "{error}");
    }

    #[test]
    fn an_eviction_round_trips_through_the_store() {
        let store = supra_store::Store::open_in_memory().expect("store");
        let turn = TurnId::generate();
        let body = b"the turn exactly as it was sent";
        let digest = evict_turn(&store, turn, body).expect("evict");
        assert_eq!(digest, supra_store::body_digest(body));
        assert_eq!(recall_turn(&store, turn).expect("recall"), body);
    }

    #[test]
    fn a_failed_eviction_leaves_the_turn_live() {
        // A conflicting second body is refused by the store; the ledger half never
        // runs, so there is nothing half-evicted to reconcile.
        let store = supra_store::Store::open_in_memory().expect("store");
        let turn = TurnId::generate();
        evict_turn(&store, turn, b"first").expect("first");
        let error = evict_turn(&store, turn, b"second").expect_err("conflict");
        assert!(matches!(error, PromptError::EvictionFailed { .. }), "{error}");
        assert_eq!(recall_turn(&store, turn).expect("recall"), b"first");
    }

    #[test]
    fn recalling_live_is_missing_not_corrupt() {
        let store = supra_store::Store::open_in_memory().expect("store");
        let error = recall_turn(&store, TurnId::generate()).expect_err("never evicted");
        assert!(matches!(error, PromptError::RecallMissing { .. }), "{error}");
    }
}
