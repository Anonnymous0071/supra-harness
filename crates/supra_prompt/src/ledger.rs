//! The append-only prompt ledger: segments in, prefix hash out.
//!
//! # What the ledger owns
//!
//! The ordered sequence of [`Sealed`](supra_types::Sealed) segments that is the prompt
//! prefix, the sequence numbers on them, and the prefix hash over the sequence. Appending
//! is the only mutation; there is no edit, no remove, no reorder. A generation seal
//! freezes the sequence into a [`Generation`] whose hash can be re-proven later.
//!
//! # What the ledger does not own
//!
//! Rendering (T14 owns the text the provider sees, but the *bytes* are the segments'
//! canonical encodings, not a re-rendering); eviction policy (see `evict.rs`); the
//! store (T10 holds the bytes, the ledger holds the order). The ledger knows *which*
//! segments form the prefix and in what order; everything else reads that answer.
//!
//! # Prefix hash construction
//!
//! Over the sequence of `(SeqNo, ContentHash)` pairs, length-prefixed through T6's
//! `CanonicalWriter` under a ledger domain separator (`0x20`, in the downstream range
//! past T10's `0x10`). Not over the segment bytes: the pair sequence is what ordering
//! means, and hashing it directly makes the hash a statement about order rather than
//! about content that already has its own digest.

use supra_types::{CanonicalWriter, ContentHash, Sealable, Sealed, Segment, SegmentId, SeqNo};

use crate::error::PromptError;

/// Domain separator for the prefix hash. Downstream range (`0x10` upward, past T10's
/// `0x10`), disjoint from every `Sealable` kind so a prefix hash cannot collide with a
/// content hash.
const PREFIX_HASH_KIND: u8 = 0x20;

/// One frozen prefix: the segment sequence sealed under its hash.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Generation {
    /// Which rewrite this was. Zero is the session's first prefix, before any rewrite.
    pub generation: u64,
    /// Segment identities in prefix order at seal time.
    pub segments: Vec<SegmentId>,
    /// Hash over the `(SeqNo, ContentHash)` sequence at seal time.
    pub prefix_hash: ContentHash,
}

/// The prompt prefix as an ordered, append-only sequence.
#[derive(Clone, Debug, Default)]
pub struct PromptLedger {
    entries: Vec<Sealed<Segment>>,
    next_seq: SeqNo,
    generation: u64,
}

impl PromptLedger {
    /// An empty ledger at generation zero.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// How many segments are live.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether nothing has been appended.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Which generation the live prefix belongs to.
    #[must_use]
    pub const fn generation(&self) -> u64 {
        self.generation
    }

    /// The next sequence number to be assigned.
    #[must_use]
    pub const fn next_seq(&self) -> SeqNo {
        self.next_seq
    }

    /// Append one segment, assigning the next sequence number.
    ///
    /// The caller supplies an *unsealed* segment; sealing is the ledger's act, which is
    /// what makes the sequence gap-free: no caller can reserve, skip, or reuse a
    /// position. Idempotent retry is the caller's to detect (same `SegmentId`
    /// re-offered reads back as already present); this function refuses the duplicate
    /// loudly rather than silently appending it twice.
    ///
    /// Returns the assigned sequence number, so the caller can read back the position
    /// without a second lookup. Returning the sealed entry itself would hand out a
    /// borrow tied to the ledger's storage - unusable past the next append - while a
    /// `SeqNo` is `Copy` and stays valid.
    ///
    /// # Errors
    ///
    /// [`PromptError::DuplicateSegment`] when the segment id is already present.
    pub fn append(&mut self, segment: Segment) -> Result<SeqNo, PromptError> {
        if self.entries.iter().any(|entry| entry.id() == segment.id()) {
            return Err(PromptError::DuplicateSegment { id: segment.id() });
        }
        let seq = self.next_seq;
        self.next_seq = seq.next();
        self.entries.push(Sealed::seal(seq, segment));
        Ok(seq)
    }

    /// Live segments in prefix order.
    #[must_use]
    pub fn segments(&self) -> &[Sealed<Segment>] {
        &self.entries
    }

    /// Hash over the live `(SeqNo, ContentHash)` sequence.
    ///
    /// Recomputed from the entries every call: the ledger never caches it, so there is
    /// no cached copy to disagree with the sequence. I7's comparison - recompute locally
    /// and compare - is this function called twice.
    #[must_use]
    pub fn prefix_hash(&self) -> ContentHash {
        hash_sequence(self.entries.iter().map(|entry| (entry.seq(), entry.hash())))
    }

    /// Freeze the live prefix into a generation seal.
    ///
    /// Records the generation number, the segment order, and the hash. The ledger keeps
    /// its entries - sealing describes the prefix, it does not clear it - and advances
    /// the generation counter so the next seal is distinct even over identical segments.
    #[must_use]
    pub fn seal_generation(&mut self) -> Generation {
        let seal = Generation {
            generation: self.generation,
            segments: self.entries.iter().map(|entry| entry.id()).collect(),
            prefix_hash: self.prefix_hash(),
        };
        self.generation += 1;
        seal
    }

    /// Re-prove a generation seal against the live sequence.
    ///
    /// # Errors
    ///
    /// [`PromptError::GenerationUnverified`] when the live sequence no longer hashes to
    /// the recorded value. Either the session file was edited around the ledger, or a
    /// segment was appended without resealing - both I1 violations, and resuming into a
    /// state whose cache accounting is wrong is worse than refusing to resume.
    pub fn verify_generation(&self, seal: &Generation) -> Result<(), PromptError> {
        let found = self.prefix_hash();
        if found != seal.prefix_hash {
            return Err(PromptError::GenerationUnverified {
                generation: seal.generation,
                expected: seal.prefix_hash.to_string(),
                found: found.to_string(),
            });
        }
        Ok(())
    }

    /// Drop the live prefix after a generation rewrite, keeping only the seal.
    ///
    /// Called once per rewrite, when the window is genuinely near its limit (92-95%):
    /// the new generation's segments are appended fresh afterwards, and the old bytes
    /// live on in T10 under their own digests. Not per turn, not per eviction - every
    /// rewrite pays a full 1-hour-TTL write, so every avoided one is money kept.
    pub fn clear_for_rewrite(&mut self) {
        self.entries.clear();
    }
}

/// Hash a `(SeqNo, ContentHash)` sequence under the ledger domain separator.
fn hash_sequence(sequence: impl Iterator<Item = (SeqNo, ContentHash)>) -> ContentHash {
    struct Pairs(Vec<(u64, [u8; ContentHash::LEN])>);

    impl Sealable for Pairs {
        const CANONICAL_KIND: u8 = PREFIX_HASH_KIND;
        fn write_canonical(&self, writer: &mut CanonicalWriter) {
            writer.u64(1, self.0.len() as u64);
            for (seq, hash) in &self.0 {
                writer.u64(2, *seq);
                writer.bytes(3, hash);
            }
        }
    }

    let pairs: Vec<(u64, [u8; ContentHash::LEN])> =
        sequence.map(|(seq, hash)| (seq.get(), *hash.as_bytes())).collect();
    ContentHash::of(&Pairs(pairs))
}

#[cfg(test)]
mod tests {
    use super::*;
    use supra_types::{Block, Role, Sealed, SegmentId, SegmentKind, SeqNo, TurnId};

    fn turn_segment(role: Role, text: &str) -> Segment {
        Segment::new(
            SegmentId::generate(),
            SegmentKind::Turn { turn: TurnId::generate(), role },
            vec![Block::Text(text.to_owned())],
        )
        .expect("a text turn is well-formed")
    }

    #[test]
    fn appending_assigns_dense_sequence_numbers() {
        let mut ledger = PromptLedger::new();
        ledger.append(turn_segment(Role::User, "a")).expect("append");
        ledger.append(turn_segment(Role::Assistant, "b")).expect("append");
        let seqs: Vec<u64> = ledger.segments().iter().map(|entry| entry.seq().get()).collect();
        assert_eq!(seqs, vec![0, 1]);
        assert_eq!(ledger.next_seq().get(), 2);
    }

    #[test]
    fn appending_the_same_segment_twice_is_refused() {
        let mut ledger = PromptLedger::new();
        let id = SegmentId::generate();
        let segment = Segment::new(
            id,
            SegmentKind::Turn { turn: TurnId::generate(), role: Role::User },
            vec![Block::Text("once".to_owned())],
        )
        .expect("well-formed");
        ledger.append(segment).expect("first");
        let again = Segment::new(
            id,
            SegmentKind::Turn { turn: TurnId::generate(), role: Role::User },
            vec![Block::Text("twice".to_owned())],
        )
        .expect("well-formed");
        let error = ledger.append(again).expect_err("duplicate id");
        assert!(matches!(error, PromptError::DuplicateSegment { .. }), "{error}");
        assert_eq!(ledger.len(), 1);
    }

    #[test]
    fn the_prefix_hash_moves_with_every_append() {
        let mut ledger = PromptLedger::new();
        let empty = ledger.prefix_hash();
        ledger.append(turn_segment(Role::User, "a")).expect("append");
        let one = ledger.prefix_hash();
        ledger.append(turn_segment(Role::User, "b")).expect("append");
        let two = ledger.prefix_hash();
        assert_ne!(empty, one);
        assert_ne!(one, two);
    }

    #[test]
    fn the_prefix_hash_is_order_sensitive() {
        // Same segments, different order, must hash differently: the hash is a statement
        // about order, and order is what the provider caches on.
        let first = turn_segment(Role::User, "a");
        let second = turn_segment(Role::User, "b");

        let mut forward = PromptLedger::new();
        forward.append(first.clone()).expect("append");
        forward.append(second.clone()).expect("append");

        let mut backward = PromptLedger::new();
        backward.append(second).expect("append");
        backward.append(first).expect("append");

        assert_ne!(forward.prefix_hash(), backward.prefix_hash());
    }

    #[test]
    fn the_prefix_hash_covers_positions_not_just_content() {
        // The M6 gap: content hashes alone cannot tell "same segments, resealed" from
        // "same segments, same positions" - and a rewrite re-seals every segment at new
        // positions, so a position-blind hash would equate two generations with
        // different cache lifetimes. Build the collision directly: one segment sealed
        // at 0 and at 5 must hash the sequence differently.
        let segment = turn_segment(Role::User, "same");
        let at_zero = Sealed::seal(SeqNo::ZERO, segment.clone());
        let at_five = Sealed::seal(SeqNo::from_raw(5), segment);
        assert_eq!(at_zero.hash(), at_five.hash(), "content hashes agree by design");

        let mut low = PromptLedger::new();
        low.entries.push(at_zero);
        let mut high = PromptLedger::new();
        high.entries.push(at_five);
        assert_ne!(
            low.prefix_hash(),
            high.prefix_hash(),
            "positions excluded: a rewrite would verify against the wrong generation"
        );
    }

    #[test]
    fn a_seal_verifies_against_the_sequence_it_froze() {
        let mut ledger = PromptLedger::new();
        ledger.append(turn_segment(Role::User, "a")).expect("append");
        let seal = ledger.seal_generation();
        assert_eq!(seal.generation, 0);
        ledger.verify_generation(&seal).expect("sealed moments ago");

        // Appending afterwards breaks the seal: the seal describes a sequence, and the
        // sequence moved.
        ledger.append(turn_segment(Role::User, "b")).expect("append");
        let error = ledger.verify_generation(&seal).expect_err("sequence moved");
        assert!(matches!(error, PromptError::GenerationUnverified { .. }), "{error}");
        assert_eq!(ledger.generation(), 1, "sealing advanced the counter");
    }

    #[test]
    fn clearing_for_rewrite_empties_the_prefix_but_keeps_the_counter() {
        let mut ledger = PromptLedger::new();
        ledger.append(turn_segment(Role::User, "a")).expect("append");
        let _ = ledger.seal_generation();
        ledger.clear_for_rewrite();
        assert!(ledger.is_empty());
        assert_eq!(ledger.generation(), 1);
        // And the next seal is a new generation even over identical content.
        ledger.append(turn_segment(Role::User, "a")).expect("append");
        let second = ledger.seal_generation();
        assert_eq!(second.generation, 1);
    }
}
