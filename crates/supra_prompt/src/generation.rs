//! Generation rewrite: one new prefix when the window is genuinely near its limit.
//!
//! # When a rewrite happens
//!
//! At 92-95% context usage, **while idle** - after a turn completes, before the user
//! types - so the 1-hour-TTL write rides on human thinking time. Not at 80%: every
//! avoided rewrite is one full rewrite not paid for, and I4's threshold is 92-95%
//! for exactly that reason.
//!
//! # What a rewrite is
//!
//! The live turns are re-sealed into a fresh ledger at generation N+1; evicted turns
//! stay evicted (their index entries are re-appended, not re-evicted - the bytes are
//! already in T10 under their digests, and re-committing them would be a second
//! write of identical bytes for no new guarantee). The old generation's seal proves
//! what the prefix hashed to before the rewrite; the new generation's seal proves
//! what it hashes to after. Both seals are kept, so the rewrite is auditable rather
//! than a silent replacement.
//!
//! # What a rewrite is not
//!
//! Summarisation, compaction, or truncation in place. Provider documentation lists
//! all three as cache-invalidating: a harness that summarises at 80% destroys its
//! prefix and pays to rebuild it. A rewrite moves the same content into a new
//! generation; it never shortens it by rewording.

use supra_types::Segment;

use crate::error::PromptError;
use crate::ledger::{Generation, PromptLedger};

/// Rewrite the live prefix into a fresh generation.
///
/// Seals the live sequence (the "before" proof), drains it, and re-appends the same
/// segments fresh at generation N+1. Returns both seals plus any segments that
/// refused re-appending: the pair is the audit trail proving the rewrite moved
/// content rather than rewording it, and a non-empty refusal list means the drain
/// produced a duplicate id - a defect, not a condition.
///
/// Evicted turns stay evicted: only live segments move. Their index entries travel
/// with them - re-appended, not re-evicted, because the bytes are already in T10
/// under their digests and re-committing identical bytes buys no new guarantee.
///
/// Sequence numbers restart at zero: they position within a generation's prefix, not
/// across the session's lifetime, and the generation seal carries the continuity.
/// The caller commits evictions first (T10 before the prefix drops anything) and is
/// responsible for what happens after; this function owns only the re-sealing.
#[must_use]
pub fn rewrite(ledger: &mut PromptLedger) -> (Generation, Generation, Vec<PromptError>) {
    let before = ledger.seal_generation();
    let live: Vec<Segment> = ledger.segments().iter().map(|entry| entry.get().clone()).collect();
    ledger.clear_for_rewrite();
    let mut skipped = Vec::new();
    for segment in live {
        // The drain holds each live segment exactly once, so re-appending cannot meet
        // a duplicate - unless the drain itself is defective, in which case skipping
        // silently would rewrite a shorter prefix and certify it. Collect the ids
        // that refuse, and let the caller decide: the rewrite pair's verification
        // compares segment sequences, so a skipped id fails the pair loudly rather
        // than vanishing quietly.
        if let Err(error) = ledger.append(segment) {
            skipped.push(error);
        }
    }
    let after = ledger.seal_generation();
    (before, after, skipped)
}

/// Verify a rewrite pair: the new generation must hold the same segment identities
/// in the same order as the old one.
///
/// A rewrite moves content; it never reorders, drops, or invents. Any of those is an
/// I1 violation wearing a rewrite's clothes, and the pair of seals is what catches
/// it - which is why both are kept rather than only the new one.
///
/// # Errors
///
/// [`PromptError::GenerationUnverified`] when the sequences differ. The message
/// carries both hashes, so the diff is debuggable rather than a bare refusal.
pub fn verify_rewrite(before: &Generation, after: &Generation) -> Result<(), PromptError> {
    if before.segments != after.segments {
        return Err(PromptError::GenerationUnverified {
            generation: after.generation,
            expected: before.prefix_hash.to_string(),
            found: after.prefix_hash.to_string(),
        });
    }
    Ok(())
}

/// Whether usage has reached the rewrite threshold.
///
/// 92-95%: every avoided rewrite is one full 1-hour-TTL write not paid for. Below 92
/// the answer is always no, whatever the caller feels about headroom - the threshold
/// is a number in the document, not a mood.
#[must_use]
pub const fn needs_rewrite(usage_percent: u8) -> bool {
    usage_percent >= 92
}

#[cfg(test)]
mod tests {
    use super::*;
    use supra_types::{Block, Role, Segment, SegmentId, SegmentKind, TurnId};

    fn turn(text: &str) -> Segment {
        Segment::new(
            SegmentId::generate(),
            SegmentKind::Turn { turn: TurnId::generate(), role: Role::User },
            vec![Block::Text(text.to_owned())],
        )
        .expect("text")
    }

    #[test]
    fn a_rewrite_preserves_order_and_content() {
        let mut ledger = PromptLedger::new();
        ledger.append(turn("a")).expect("append");
        ledger.append(turn("b")).expect("append");
        let (before, after, skipped) = rewrite(&mut ledger);
        assert!(skipped.is_empty(), "nothing refuses: {skipped:?}");
        assert_eq!(before.generation, 0);
        assert_eq!(after.generation, 1);
        verify_rewrite(&before, &after).expect("a faithful rewrite verifies");
        assert_eq!(ledger.len(), 2);
        // Two appends before the rewrite, two re-appends after: the counter never
        // rewinds, because sequence numbers must stay gap-free within the process
        // even though each generation's positions restart at zero.
        assert_eq!(ledger.next_seq().get(), 4);
    }

    #[test]
    fn a_reordered_rewrite_is_refused() {
        let mut ledger = PromptLedger::new();
        ledger.append(turn("a")).expect("append");
        ledger.append(turn("b")).expect("append");
        let (mut before, after, skipped) = rewrite(&mut ledger);
        assert!(skipped.is_empty(), "nothing refuses: {skipped:?}");
        before.segments.reverse();
        let error = verify_rewrite(&before, &after).expect_err("reordered");
        assert!(matches!(error, PromptError::GenerationUnverified { .. }), "{error}");
    }

    #[test]
    fn the_threshold_is_92_not_80() {
        assert!(!needs_rewrite(80));
        assert!(!needs_rewrite(91));
        assert!(needs_rewrite(92));
        assert!(needs_rewrite(95));
        assert!(needs_rewrite(100));
    }
}
