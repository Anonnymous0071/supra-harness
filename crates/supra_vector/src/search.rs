//! The retrieval algorithms, with no SQL in them.
//!
//! Two lanes and a fusion:
//!
//! - **semantic**: scan every code, keep the closest `R`, then order those by their exact
//!   vectors. See [`crate::codes`] for the measurements behind that shape.
//! - **lexical**: FTS5 with `bm25()`, which lives in [`crate::index`] because it is a query.
//! - **fusion**: [`fuse`], reciprocal rank fusion over integer ranks.
//!
//! # Why fusion happens over ranks and not scores
//!
//! The two lanes produce numbers that are not comparable. A cosine is bounded and roughly
//! linear; `bm25()` is unbounded, negative, and its scale depends on the corpus's term
//! statistics - so the same document scores differently once unrelated documents are added.
//! Normalising them onto a shared range means choosing a min and a max, and both move as the
//! corpus changes, which makes the fused ranking depend on corpus size in a way nobody asked
//! for.
//!
//! Reciprocal rank fusion needs only the *positions*. `SCALE / (K + rank)`, summed as integers.
//! That is reproducible bit-for-bit, has no tuning surface beyond `K`, and sidesteps the
//! question of what a relevance score means. It also keeps this crate out of float comparison
//! for anything that is stored or compared for equality, which is the discipline T6 set.

use std::collections::HashMap;

use crate::codes;

/// The `k` from the reciprocal rank fusion paper. A larger value flattens the contribution of
/// the top ranks; 60 is the published default and is kept because nothing here has measured a
/// reason to differ.
pub const RRF_K: u64 = 60;

/// Rank depth at which contributions are guaranteed to stay distinct.
///
/// Past this, two adjacent ranks can produce the same integer contribution, and the fused order
/// between them comes from the tie-break on the slot id rather than from the lanes. That is
/// harmless where it happens - at rank 1000 a contribution is about a sixtieth of rank 1's, so
/// the tail is already negligible - but it is a property worth having a name and a bound rather
/// than discovering.
pub const MAX_FUSION_DEPTH: u64 = 1000;

/// Numerator, chosen so integer division keeps its resolution to [`MAX_FUSION_DEPTH`].
///
/// Contributions at adjacent ranks differ by about `RRF_SCALE / d^2`, where `d` is the
/// denominator `RRF_K + rank`. So they stay distinct while `RRF_SCALE >= d^2`, which is the
/// bound asserted below.
///
/// The first value tried here was 1,000,000, with a comment claiming distinctness to rank 1000.
/// It collapses at rank 941: `1_000_000 / 1001^2` is less than one. The test found it, which is
/// the reason the claim is now derived and compile-checked rather than asserted in prose.
pub const RRF_SCALE: u64 = 10_000_000;

/// The resolution claim above, enforced where it cannot drift out of agreement with the constants
/// it is about.
const _: () = assert!(
    RRF_SCALE >= (RRF_K + MAX_FUSION_DEPTH) * (RRF_K + MAX_FUSION_DEPTH),
    "RRF_SCALE is too small to keep adjacent ranks distinct to MAX_FUSION_DEPTH"
);

/// The resident first tier: one code per entry, in one contiguous buffer.
///
/// Position-major rather than keyed by slot, because the scan's cost is bytes moved and a map
/// would scatter them. Positions are internal and unstable - a removal moves the last entry
/// into the hole - so nothing outside this type may hold one.
///
/// The slot-to-position map beside it is not an optimisation of the scan, which never consults
/// it. It is what keeps *loading* linear: without it every `upsert` would search the slot list,
/// so restoring a 100k-entry index would be quadratic - about five billion comparisons for a
/// table that takes 9.6 MB to hold.
#[derive(Debug)]
pub struct CodeTable {
    stride: usize,
    data: Vec<u8>,
    slots: Vec<i64>,
    positions: HashMap<i64, usize>,
}

impl CodeTable {
    /// An empty table for codes of `stride` bytes.
    #[must_use]
    pub fn new(stride: usize) -> Self {
        Self { stride, data: Vec::new(), slots: Vec::new(), positions: HashMap::new() }
    }

    /// An empty table with room for `capacity` entries reserved.
    ///
    /// Loading an index knows its size from a `count(*)`, and growing a 9.6 MB buffer by
    /// doubling copies it about seventeen times on the way.
    #[must_use]
    pub fn with_capacity(stride: usize, capacity: usize) -> Self {
        Self {
            stride,
            data: Vec::with_capacity(stride * capacity),
            slots: Vec::with_capacity(capacity),
            positions: HashMap::with_capacity(capacity),
        }
    }

    /// How many entries are resident.
    #[must_use]
    pub fn len(&self) -> usize {
        self.slots.len()
    }

    /// Whether nothing is resident.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.slots.is_empty()
    }

    /// Bytes held resident, which is the memory this tier costs.
    ///
    /// Counts the codes, the slot list, and the map: a report that left out the map would
    /// understate the tier by about a third.
    #[must_use]
    pub fn resident_bytes(&self) -> usize {
        self.data.len()
            + self.slots.len() * size_of::<i64>()
            + self.positions.len() * (size_of::<i64>() + size_of::<usize>())
    }

    /// Code width in bytes.
    #[must_use]
    pub const fn stride(&self) -> usize {
        self.stride
    }

    /// Add or replace one entry's code.
    ///
    /// Returns `false` and changes nothing when `code` is not [`CodeTable::stride`] bytes: a
    /// short code would make every later position read into its neighbour, which is a
    /// corruption that ranks rather than fails.
    pub fn upsert(&mut self, slot: i64, code: &[u8]) -> bool {
        if code.len() != self.stride {
            return false;
        }
        if let Some(position) = self.positions.get(&slot).copied() {
            self.data[position * self.stride..(position + 1) * self.stride].copy_from_slice(code);
        } else {
            self.positions.insert(slot, self.slots.len());
            self.slots.push(slot);
            self.data.extend_from_slice(code);
        }
        true
    }

    /// Drop one entry. Returns whether it was there.
    ///
    /// The last entry is moved into the hole rather than shifting the rest: the alternative is
    /// `O(n)` per removal, and an incrementally maintained index removes an entry every time a
    /// symbol is renamed.
    pub fn remove(&mut self, slot: i64) -> bool {
        let Some(position) = self.positions.remove(&slot) else { return false };
        let last = self.slots.len() - 1;
        if position != last {
            let (head, tail) = self.data.split_at_mut(last * self.stride);
            head[position * self.stride..(position + 1) * self.stride].copy_from_slice(&tail[..self.stride]);
            let moved = self.slots[last];
            self.slots[position] = moved;
            // The moved entry's own map entry has to follow it, or a later lookup would read
            // the hole - and the hole now holds somebody else's code.
            self.positions.insert(moved, position);
        }
        self.slots.pop();
        self.data.truncate(last * self.stride);
        true
    }

    /// Forget everything, keeping the allocation for a reload.
    pub fn clear(&mut self) {
        self.data.clear();
        self.slots.clear();
        self.positions.clear();
    }

    /// Whether a slot is resident.
    #[must_use]
    pub fn contains(&self, slot: i64) -> bool {
        self.positions.contains_key(&slot)
    }

    /// The `width` closest slots to `query`, nearest first.
    ///
    /// Ties break on the slot id, ascending, so the candidate set is the same on every run. A
    /// Hamming distance produces a lot of ties - it is an integer over a few hundred bits - and
    /// leaving them to the sort's own order would make the rerank set depend on the insertion
    /// history.
    #[must_use]
    pub fn nearest(&self, query: &[u8], width: usize) -> Vec<Candidate> {
        if width == 0 || self.slots.is_empty() || query.len() != self.stride {
            return Vec::new();
        }

        let mut scored: Vec<Candidate> = self
            .data
            .chunks_exact(self.stride)
            .zip(&self.slots)
            .map(|(code, slot)| Candidate { distance: codes::distance(query, code), slot: *slot })
            .collect();

        let keep = width.min(scored.len());
        // Partition first: sorting the whole corpus to read ten entries is the difference
        // between a scan and a sort, and at 100k entries that is milliseconds against tens.
        let pivot = keep - 1;
        scored.select_nth_unstable_by_key(pivot, |candidate| (candidate.distance, candidate.slot));
        scored.truncate(keep);
        scored.sort_unstable_by_key(|candidate| (candidate.distance, candidate.slot));
        scored
    }
}

/// One entry that survived the code scan.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Candidate {
    /// Hamming distance to the query's code. Smaller is nearer.
    pub distance: u32,
    /// The entry's slot.
    pub slot: i64,
}

/// Order candidates by their exact vectors, highest similarity first.
///
/// Ties break on the slot id, ascending. That is not decoration: the lane split in
/// [`codes::similarity`] changes the order of the additions, so two entries whose true
/// similarity is identical can land one ULP apart in either direction, and without a
/// deterministic tie-break the reported order would depend on the arithmetic's rounding.
#[must_use]
pub fn order_exact(mut scored: Vec<Scored>) -> Vec<Scored> {
    scored.sort_by(|left, right| {
        right
            .similarity
            .partial_cmp(&left.similarity)
            .unwrap_or(core::cmp::Ordering::Equal)
            .then(left.slot.cmp(&right.slot))
    });
    scored
}

/// One entry with its exact similarity.
#[derive(Clone, Copy, Debug)]
pub struct Scored {
    /// The entry's slot.
    pub slot: i64,
    /// Cosine similarity against the query, from the exact vectors.
    pub similarity: f32,
}

/// Combine ranked lanes by reciprocal rank fusion.
///
/// Each input is one lane's result, best first. A slot's fused score is the sum of
/// `RRF_SCALE / (RRF_K + rank)` over the lanes that returned it, with `rank` 1-based. A slot
/// missing from a lane contributes nothing from it - not a penalty, because a lane that never
/// saw an entry has said nothing about it, and treating silence as a negative judgement would
/// let the narrower lane veto the wider one.
///
/// Output is sorted by score descending, then slot ascending.
#[must_use]
pub fn fuse(lanes: &[&[i64]]) -> Vec<Fused> {
    let mut fused: Vec<Fused> = Vec::new();
    // Slot to position in `fused`. A linear search would be quadratic in the combined lane
    // width, and a lane is free to return five hundred candidates.
    let mut seen: HashMap<i64, usize> = HashMap::new();

    for (lane, ranked) in lanes.iter().enumerate() {
        for (index, slot) in ranked.iter().enumerate() {
            let rank = index as u64 + 1;
            let contribution = RRF_SCALE / (RRF_K + rank);
            if let Some(position) = seen.get(slot).copied() {
                fused[position].score += contribution;
                fused[position].lanes.push(LaneRank { lane, rank });
            } else {
                seen.insert(*slot, fused.len());
                fused.push(Fused { slot: *slot, score: contribution, lanes: vec![LaneRank { lane, rank }] });
            }
        }
    }

    fused.sort_unstable_by(|left, right| right.score.cmp(&left.score).then(left.slot.cmp(&right.slot)));
    fused
}

/// One entry's position in one lane.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LaneRank {
    /// Index of the lane in the slice passed to [`fuse`].
    pub lane: usize,
    /// 1-based position within that lane.
    pub rank: u64,
}

/// One fused result.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Fused {
    /// The entry's slot.
    pub slot: i64,
    /// Summed reciprocal-rank contribution. Comparable only against other entries in the same
    /// fusion; it has no units and no absolute meaning.
    pub score: u64,
    /// Which lanes returned this entry, and where. Kept because "found by both lanes" and
    /// "found near the top of one" are different situations that can fuse to the same score,
    /// and a diagnostic that cannot tell them apart is not much of a diagnostic.
    pub lanes: Vec<LaneRank>,
}

/// How separable a corpus is: the gap between what a query's best matches score and what an
/// arbitrary entry scores.
///
/// This exists because a recall number taken over a corpus with no structure is meaningless,
/// and it looks exactly like a recall number taken over a real one. Two of this stage's
/// measurements were invalidated that way before the generator was asked to state its own
/// separability. It is public because the same question applies to a live index: if the signal
/// is near zero, the semantic lane is not contributing and the caller should hear that rather
/// than infer it.
#[derive(Clone, Copy, Debug)]
pub struct CorpusSignal {
    /// Mean similarity over the sampled entries.
    pub mean: f64,
    /// Mean similarity of the best `k`.
    pub best: f64,
    /// `best - mean`. Near zero means nothing is retrievable.
    pub signal: f64,
}

impl CorpusSignal {
    /// Measure separability by scoring `query` against every vector in `corpus`.
    ///
    /// `corpus` is one contiguous buffer of `dims`-wide unit vectors. Returns `None` for an
    /// empty corpus, because a signal over nothing is not zero - it is undefined, and
    /// reporting zero would look like a diagnosis.
    #[must_use]
    pub fn measure(query: &[f32], corpus: &[f32], dims: usize, best_of: usize) -> Option<Self> {
        if dims == 0 || corpus.len() < dims || query.len() != dims || best_of == 0 {
            return None;
        }

        let mut scores: Vec<f32> =
            corpus.chunks_exact(dims).map(|vector| codes::similarity(query, vector)).collect();
        if scores.is_empty() {
            return None;
        }

        // Summed in `f64`. Adding a hundred thousand `f32` similarities in `f32` loses the tail
        // of the sum entirely once the running total is a few thousand - and this number exists
        // to be compared against another mean, so a systematic error in it is the one error that
        // would not cancel out.
        let count = scores.len();
        let mean = mean_of(&scores, count)?;

        let keep = best_of.min(count);
        scores.sort_by(|left, right| right.partial_cmp(left).unwrap_or(core::cmp::Ordering::Equal));
        let best = mean_of(&scores[..keep], keep)?;

        Some(Self { mean, best, signal: best - mean })
    }
}

/// Mean of `values`, taken over `count` of them.
///
/// Kept in `f64` all the way out: this number exists only to be compared against another mean,
/// and narrowing it back to `f32` at the end would discard the precision the `f64` sum was for.
///
/// `count` is converted through `u32` so no lossy cast appears: a corpus of more than four
/// billion entries would need 412 GB just for its codes, so the conversion cannot fail in any
/// reachable configuration - and returning `None` rather than saturating means an unreachable
/// case cannot silently produce a plausible number.
fn mean_of(values: &[f32], count: usize) -> Option<f64> {
    let divisor = f64::from(u32::try_from(count).ok()?);
    if divisor == 0.0 {
        return None;
    }
    let total: f64 = values.iter().map(|value| f64::from(*value)).sum();
    Some(total / divisor)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn code(byte: u8, stride: usize) -> Vec<u8> {
        vec![byte; stride]
    }

    #[test]
    fn an_empty_table_returns_nothing() {
        let table = CodeTable::new(8);
        assert!(table.is_empty());
        assert_eq!(table.len(), 0);
        assert!(table.nearest(&code(0, 8), 10).is_empty());
    }

    #[test]
    fn entries_are_found_nearest_first() {
        let mut table = CodeTable::new(1);
        assert!(table.upsert(10, &[0b0000_0000]));
        assert!(table.upsert(20, &[0b0000_0011]));
        assert!(table.upsert(30, &[0b0000_0001]));

        let found = table.nearest(&[0b0000_0000], 3);
        assert_eq!(found[0], Candidate { distance: 0, slot: 10 });
        assert_eq!(found[1], Candidate { distance: 1, slot: 30 });
        assert_eq!(found[2], Candidate { distance: 2, slot: 20 });
    }

    #[test]
    fn ties_break_on_the_slot_so_the_candidate_set_is_stable() {
        // A Hamming distance is a small integer over a few hundred bits, so ties are the rule
        // rather than the exception. Without a deterministic tie-break the rerank set would
        // depend on insertion order, and two runs over the same corpus could answer differently.
        let mut table = CodeTable::new(1);
        for slot in [50_i64, 10, 40, 20, 30] {
            assert!(table.upsert(slot, &[0b0000_0001]));
        }
        let found = table.nearest(&[0b0000_0000], 3);
        assert_eq!(found.iter().map(|candidate| candidate.slot).collect::<Vec<_>>(), vec![10, 20, 30]);
    }

    #[test]
    fn asking_for_more_than_exists_returns_everything() {
        let mut table = CodeTable::new(1);
        table.upsert(1, &[0]);
        table.upsert(2, &[1]);
        assert_eq!(table.nearest(&[0], 100).len(), 2);
    }

    #[test]
    fn asking_for_none_returns_none() {
        let mut table = CodeTable::new(1);
        table.upsert(1, &[0]);
        assert!(table.nearest(&[0], 0).is_empty());
    }

    #[test]
    fn a_query_of_the_wrong_width_returns_nothing_rather_than_a_partial_comparison() {
        let mut table = CodeTable::new(4);
        table.upsert(1, &code(0, 4));
        assert!(table.nearest(&code(0, 3), 10).is_empty());
        assert!(table.nearest(&code(0, 5), 10).is_empty());
    }

    #[test]
    fn a_code_of_the_wrong_width_is_refused() {
        // Accepting it would misalign every later position, so each entry would be compared
        // against a window spanning two of its neighbours - a corruption that ranks.
        let mut table = CodeTable::new(4);
        assert!(!table.upsert(1, &code(0, 3)));
        assert!(!table.upsert(1, &code(0, 5)));
        assert!(table.is_empty());
        assert!(table.upsert(1, &code(0, 4)));
        assert_eq!(table.len(), 1);
    }

    #[test]
    fn upserting_the_same_slot_replaces_its_code() {
        let mut table = CodeTable::new(1);
        table.upsert(7, &[0b0000_0000]);
        table.upsert(7, &[0b1111_1111]);
        assert_eq!(table.len(), 1, "the slot was duplicated");
        assert_eq!(table.nearest(&[0b1111_1111], 1)[0], Candidate { distance: 0, slot: 7 });
    }

    #[test]
    fn removing_the_middle_entry_keeps_the_rest_readable() {
        // The hole is filled by moving the last entry into it. If the data buffer and the slot
        // list ever disagreed about which position moved, a later scan would report one entry's
        // distance under another's slot - the failure this test exists to catch.
        let mut table = CodeTable::new(1);
        table.upsert(1, &[0b0000_0001]);
        table.upsert(2, &[0b0000_0011]);
        table.upsert(3, &[0b0000_0111]);
        table.upsert(4, &[0b0000_1111]);

        assert!(table.remove(2));
        assert_eq!(table.len(), 3);

        let found = table.nearest(&[0b0000_0000], 3);
        assert_eq!(found[0], Candidate { distance: 1, slot: 1 });
        assert_eq!(found[1], Candidate { distance: 3, slot: 3 });
        assert_eq!(found[2], Candidate { distance: 4, slot: 4 });
    }

    #[test]
    fn the_entry_moved_by_a_removal_is_still_addressable() {
        // The swap-remove moves the last entry into the hole, and its position map entry has to
        // follow it. Nothing in `nearest` consults that map - the scan walks the code buffer
        // directly - so a stale entry is invisible to every test that only searches. It surfaces
        // on the next *write*: an upsert of the moved slot lands at its old position, overwriting
        // a different entry's code, or indexes past the end of a buffer that was just truncated.
        //
        // A mutation deleting the map fix survived the entire suite until this test existed.
        let mut table = CodeTable::new(1);
        for slot in 1_i64..=4 {
            assert!(table.upsert(slot, &[u8::try_from(slot).unwrap_or(0)]));
        }

        // Removing the first entry moves slot 4 into position 0.
        assert!(table.remove(1));
        assert!(table.contains(4), "the moved entry is no longer addressable");

        assert!(table.upsert(4, &[0b1111_1111]));
        assert_eq!(table.len(), 3, "the upsert added a duplicate instead of replacing");

        let found = table.nearest(&[0b1111_1111], 3);
        let distances: std::collections::BTreeMap<i64, u32> =
            found.iter().map(|candidate| (candidate.slot, candidate.distance)).collect();
        assert_eq!(distances[&4], 0, "the moved entry's own code was not the one replaced");
        assert_eq!(distances[&2], (0b0000_0010_u8 ^ 0b1111_1111).count_ones());
        assert_eq!(distances[&3], (0b0000_0011_u8 ^ 0b1111_1111).count_ones());
    }

    #[test]
    fn every_slot_holds_the_code_last_written_for_it() {
        // The general form: a shadow map of what each slot should hold, checked after every
        // operation. The code buffer, the slot list and the position map are three views of one
        // set, and any pair drifting produces a table that answers plausibly and wrongly.
        let mut table = CodeTable::new(2);
        let mut expected: std::collections::BTreeMap<i64, [u8; 2]> = std::collections::BTreeMap::new();

        let mut state = 0x2545_F491_4F6C_DD1D_u64;
        let mut next = move || {
            state = state.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
            state >> 33
        };

        for step in 0..300_u64 {
            let slot = i64::try_from(next() % 24).unwrap_or(0);
            let code = [u8::try_from(next() % 256).unwrap_or(0), u8::try_from(next() % 256).unwrap_or(0)];

            if step % 3 == 2 {
                let removed = table.remove(slot);
                assert_eq!(
                    removed,
                    expected.remove(&slot).is_some(),
                    "step {step}: the table and the shadow disagreed about a removal"
                );
            } else {
                assert!(table.upsert(slot, &code));
                expected.insert(slot, code);
            }

            assert_eq!(table.len(), expected.len(), "step {step}: length drifted");
            for (slot, code) in &expected {
                assert!(table.contains(*slot), "step {step}: slot {slot} is not addressable");
                // Read the code back through the scan: it must be distance zero from itself.
                let found = table.nearest(code, table.len());
                let mine = found.iter().find(|candidate| candidate.slot == *slot);
                assert_eq!(
                    mine.map(|candidate| candidate.distance),
                    Some(0),
                    "step {step}: slot {slot} does not hold the code last written for it"
                );
            }
        }
    }

    #[test]
    fn removing_the_last_entry_is_not_a_special_case() {
        let mut table = CodeTable::new(1);
        table.upsert(1, &[0b0000_0001]);
        table.upsert(2, &[0b0000_0011]);
        assert!(table.remove(2));
        assert_eq!(table.len(), 1);
        assert_eq!(table.nearest(&[0], 1)[0].slot, 1);

        assert!(table.remove(1));
        assert!(table.is_empty());
        assert_eq!(table.resident_bytes(), 0);
    }

    #[test]
    fn removing_something_absent_says_so() {
        let mut table = CodeTable::new(1);
        table.upsert(1, &[0]);
        assert!(!table.remove(99));
        assert_eq!(table.len(), 1);
    }

    #[test]
    fn every_entry_survives_a_removal_of_each_position_in_turn() {
        // The swap-remove is the only place the two parallel buffers can drift, so it is
        // exercised at every position rather than at one.
        for victim in 0_i64..6 {
            let mut table = CodeTable::new(1);
            for slot in 0_i64..6 {
                let byte = u8::try_from(slot).unwrap_or(0);
                assert!(table.upsert(slot, &[byte]));
            }
            assert!(table.remove(victim));

            let found = table.nearest(&[0], 6);
            assert_eq!(found.len(), 5, "removing {victim}");
            for candidate in found {
                let expected = u8::try_from(candidate.slot).unwrap_or(0).count_ones();
                assert_eq!(
                    candidate.distance, expected,
                    "slot {} reported a distance belonging to another entry",
                    candidate.slot
                );
            }
        }
    }

    #[test]
    fn resident_bytes_tracks_the_corpus() {
        // Includes the position map, which is a third of the cost at this width. The first
        // version of this test left it out and asserted 104 against a real 120 - so the figure a
        // memory report would have shown was understated by exactly the amount the map costs.
        let mut table = CodeTable::new(96);
        assert_eq!(table.resident_bytes(), 0);
        table.upsert(1, &code(0, 96));
        let per_entry = 96 + size_of::<i64>() + size_of::<i64>() + size_of::<usize>();
        assert_eq!(table.resident_bytes(), per_entry);
    }

    #[test]
    fn clearing_keeps_the_stride() {
        let mut table = CodeTable::new(96);
        table.upsert(1, &code(0, 96));
        table.clear();
        assert!(table.is_empty());
        assert_eq!(table.stride(), 96);
        assert!(table.upsert(2, &code(0, 96)));
    }

    // ------------------------------------------------------------------ exact ordering

    #[test]
    fn exact_ordering_is_highest_first() {
        let ordered = order_exact(vec![
            Scored { slot: 1, similarity: 0.2 },
            Scored { slot: 2, similarity: 0.9 },
            Scored { slot: 3, similarity: 0.5 },
        ]);
        assert_eq!(ordered.iter().map(|entry| entry.slot).collect::<Vec<_>>(), vec![2, 3, 1]);
    }

    #[test]
    fn equal_similarities_order_by_slot() {
        let ordered = order_exact(vec![
            Scored { slot: 9, similarity: 0.5 },
            Scored { slot: 3, similarity: 0.5 },
            Scored { slot: 6, similarity: 0.5 },
        ]);
        assert_eq!(ordered.iter().map(|entry| entry.slot).collect::<Vec<_>>(), vec![3, 6, 9]);
    }

    #[test]
    fn a_nan_similarity_does_not_reorder_the_rest() {
        // `validate` keeps NaN out of the index, so this is the defensive path. What it must not
        // do is scramble the entries that are fine: `partial_cmp` returns `None` against NaN,
        // and a comparator that panicked or reordered on that would turn one bad row into a
        // wrong answer for every other row.
        let ordered = order_exact(vec![
            Scored { slot: 1, similarity: 0.9 },
            Scored { slot: 2, similarity: f32::NAN },
            Scored { slot: 3, similarity: 0.5 },
        ]);
        let good: Vec<i64> =
            ordered.iter().filter(|entry| entry.similarity.is_finite()).map(|entry| entry.slot).collect();
        assert_eq!(good, vec![1, 3], "the finite entries lost their order");
    }

    // ------------------------------------------------------------------ fusion

    #[test]
    fn fusing_one_lane_preserves_its_order() {
        let lane = [10_i64, 20, 30];
        let fused = fuse(&[&lane]);
        assert_eq!(fused.iter().map(|entry| entry.slot).collect::<Vec<_>>(), vec![10, 20, 30]);
    }

    #[test]
    fn an_entry_in_both_lanes_outranks_one_in_either() {
        // The point of fusing: agreement between an independent lexical and semantic judgement
        // is stronger evidence than a high position in one of them.
        let semantic = [1_i64, 2];
        let lexical = [3_i64, 1];
        let fused = fuse(&[&semantic, &lexical]);
        assert_eq!(fused[0].slot, 1, "the entry both lanes found should lead");
        assert_eq!(fused[0].lanes.len(), 2);
    }

    #[test]
    fn a_missing_entry_is_not_penalised() {
        // A lane that never saw an entry has said nothing about it. Scoring silence as a
        // negative would let the narrower lane veto the wider one - and the lexical lane is
        // always narrower, because it only returns entries containing the query's terms.
        let semantic = [1_i64, 2, 3];
        let lexical = [2_i64];
        let fused = fuse(&[&semantic, &lexical]);
        let ranks: Vec<i64> = fused.iter().map(|entry| entry.slot).collect();
        assert_eq!(ranks[0], 2, "found by both");
        assert!(ranks.contains(&1) && ranks.contains(&3), "the semantic-only entries survived");
        assert_eq!(fused.len(), 3);
    }

    #[test]
    fn fusion_is_integer_arithmetic_and_reproducible() {
        // Reproducible bit-for-bit is the reason for ranks over scores: the same inputs must
        // fuse to the same numbers on every machine, and a float sum does not promise that
        // across compilers.
        let semantic = [5_i64, 4, 3, 2, 1];
        let lexical = [1_i64, 2, 3];
        let first = fuse(&[&semantic, &lexical]);
        let second = fuse(&[&semantic, &lexical]);
        assert_eq!(first, second);
        assert_eq!(first[0].score, RRF_SCALE / (RRF_K + 5) + RRF_SCALE / (RRF_K + 1));
    }

    #[test]
    fn the_scale_keeps_ranks_distinct_to_the_declared_depth() {
        // A scale too small collapses the tail into ties, and then the fused order comes from the
        // tie-break rather than from the lanes. This is asserted rather than reasoned about
        // because the reasoning was wrong the first time: 1,000,000 was documented as distinct to
        // rank 1000 and collapses at 941.
        let mut seen = std::collections::BTreeSet::new();
        for rank in 1..=MAX_FUSION_DEPTH {
            assert!(seen.insert(RRF_SCALE / (RRF_K + rank)), "rank {rank} collapsed onto another");
        }
    }

    #[test]
    fn the_scale_is_the_smallest_power_of_ten_that_holds_the_claim() {
        // Not a style point: an oversized scale would make a fused score overflow sooner when
        // lanes are added, and an undersized one silently flattens the tail. A tenth of it must
        // fail the same property this crate compile-asserts for the real value.
        let smaller = RRF_SCALE / 10;
        let mut seen = std::collections::BTreeSet::new();
        let collapsed = (1..=MAX_FUSION_DEPTH).any(|rank| !seen.insert(smaller / (RRF_K + rank)));
        assert!(collapsed, "RRF_SCALE could be ten times smaller and still hold its claim");
    }

    #[test]
    fn fusing_nothing_returns_nothing() {
        assert!(fuse(&[]).is_empty());
        let empty: [i64; 0] = [];
        assert!(fuse(&[&empty, &empty]).is_empty());
    }

    #[test]
    fn a_repeated_slot_within_one_lane_accumulates_rather_than_duplicating() {
        // A lane should not return a duplicate, and if one does the output must stay a set:
        // a duplicated slot downstream would consume two of the ten anchor slots with one
        // answer.
        let lane = [7_i64, 7];
        let fused = fuse(&[&lane]);
        assert_eq!(fused.len(), 1);
        assert_eq!(fused[0].score, RRF_SCALE / (RRF_K + 1) + RRF_SCALE / (RRF_K + 2));
    }

    // ------------------------------------------------------------------ corpus signal

    #[test]
    fn a_structured_corpus_reports_a_signal() {
        // Two clusters, a query sitting in one of them.
        let dims = 8_usize;
        let mut corpus = Vec::new();
        for _ in 0..10 {
            corpus.extend_from_slice(&[1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0]);
        }
        for _ in 0..10 {
            corpus.extend_from_slice(&[0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0]);
        }
        let signal = CorpusSignal::measure(&[1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0], &corpus, dims, 10)
            .expect("measurable");
        assert!((signal.best - 1.0).abs() < 1e-6, "best was {}", signal.best);
        assert!((signal.mean - 0.5).abs() < 1e-6, "mean was {}", signal.mean);
        assert!(signal.signal > 0.4, "signal was {}", signal.signal);
    }

    #[test]
    fn a_corpus_of_identical_vectors_reports_no_signal() {
        // The degenerate case, and the one that invalidated two of this stage's measurements:
        // every entry equally close, so there is no correct answer for any method to find.
        let dims = 8_usize;
        let vector = [1.0_f32, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0];
        let corpus: Vec<f32> = vector.iter().copied().cycle().take(dims * 20).collect();
        let signal = CorpusSignal::measure(&vector, &corpus, dims, 10).expect("measurable");
        assert!(signal.signal.abs() < 1e-6, "signal was {}", signal.signal);
    }

    #[test]
    fn an_empty_corpus_is_undefined_rather_than_zero() {
        // Reporting zero would look like a diagnosis of a flat corpus, which is a different
        // situation from having no corpus at all.
        assert!(CorpusSignal::measure(&[1.0; 8], &[], 8, 10).is_none());
        assert!(CorpusSignal::measure(&[1.0; 8], &[1.0; 8], 0, 10).is_none());
        assert!(CorpusSignal::measure(&[1.0; 8], &[1.0; 8], 8, 0).is_none());
        assert!(CorpusSignal::measure(&[1.0; 4], &[1.0; 8], 8, 10).is_none(), "width mismatch");
    }
}
