//! The second tier: a bounded cache of exact vectors.
//!
//! # What the two tiers actually are
//!
//! Tier one is [`crate::search::CodeTable`] - one binary code per entry, all of it resident,
//! because the first stage has to look at every entry. At 768 dimensions that is 96 bytes each:
//! 9.6 MB for a hundred thousand entries.
//!
//! Tier two is the exact vectors, and they are 3072 bytes each - 307 MB for the same corpus.
//! Holding all of them would defeat the point of the codes. Reading every one of them from
//! SQLite per query would too. But the rerank only needs the *candidates*, which is a hundred
//! or so, and consecutive queries in a session tend to land in the same neighbourhoods.
//!
//! So this cache is what makes the second tier bounded: a fixed byte budget, least-recently-used
//! eviction, and SQLite behind it as the source of truth. Measured, a rerank set fetched from
//! SQLite by rowid costs about 2.6 µs per vector - 256 µs for a hundred - so the cache is not
//! what makes the budget; it is what keeps the budget from growing with the corpus.
//!
//! # Byte budget rather than entry count
//!
//! An entry count would mean a different memory footprint at every width: four thousand vectors
//! is 6 MB at 384 dimensions and 25 MB at 1536. The knob the caller cares about is bytes.

use std::collections::{BTreeMap, HashMap};

/// Default budget for the exact-vector tier.
///
/// 16 MiB, which is about 5,400 vectors at 768 dimensions. Chosen against the first tier rather
/// than in the abstract: at a hundred thousand entries the codes take 9.6 MB, so this keeps the
/// two tiers the same order of magnitude instead of letting the exact vectors dominate a design
/// whose whole point was not to hold them.
pub const DEFAULT_CACHE_BYTES: usize = 16 * 1024 * 1024;

/// A bounded, least-recently-used cache of exact vectors.
///
/// Not thread-safe by itself. [`crate::VectorIndex`] keeps it behind a mutex, which is also
/// what makes `get` able to take `&mut self` and record the access.
#[derive(Debug)]
pub struct ExactCache {
    budget_bytes: usize,
    used_bytes: usize,
    /// Monotonic access counter. `u64` rather than a wrapping tick: at one access per
    /// nanosecond it lasts 584 years, so the wrap that would silently make the oldest entry
    /// look newest cannot be reached.
    tick: u64,
    vectors: HashMap<i64, Cached>,
    /// Access order, least recent first. Keyed by tick, which is unique by construction.
    order: BTreeMap<u64, i64>,
    hits: u64,
    misses: u64,
    evictions: u64,
}

#[derive(Debug)]
struct Cached {
    vector: Vec<f32>,
    tick: u64,
}

impl ExactCache {
    /// A cache holding at most `budget_bytes` of vector data.
    ///
    /// A budget of zero is honoured literally: nothing is retained, every lookup misses, and
    /// the rerank reads from SQLite every time. That is a legitimate configuration for a
    /// memory-constrained host, so it is not silently raised to a minimum.
    #[must_use]
    pub fn new(budget_bytes: usize) -> Self {
        Self {
            budget_bytes,
            used_bytes: 0,
            tick: 0,
            vectors: HashMap::new(),
            order: BTreeMap::new(),
            hits: 0,
            misses: 0,
            evictions: 0,
        }
    }

    /// Look one up, recording the access.
    ///
    /// Returns a clone rather than a reference. The reference would have to outlive the recency
    /// update, and the caller holds a mutex for the duration of a rerank - so lending out a
    /// borrow would either extend that lock or need the recency update skipped, and a cache
    /// that does not record reads is not an LRU.
    pub fn get(&mut self, slot: i64) -> Option<Vec<f32>> {
        let Some(entry) = self.vectors.get_mut(&slot) else {
            self.misses += 1;
            return None;
        };

        self.order.remove(&entry.tick);
        self.tick += 1;
        entry.tick = self.tick;
        self.order.insert(self.tick, slot);

        self.hits += 1;
        Some(entry.vector.clone())
    }

    /// Retain one, evicting least-recently-used entries until the budget is met.
    ///
    /// A vector larger than the whole budget is **not** retained, and does not empty the cache
    /// on its way through: evicting everything for something that cannot be kept anyway would
    /// turn one oversized entry into a cache flush.
    pub fn put(&mut self, slot: i64, vector: Vec<f32>) {
        let bytes = vector.len() * size_of::<f32>();
        if bytes > self.budget_bytes {
            return;
        }

        if let Some(previous) = self.vectors.remove(&slot) {
            self.order.remove(&previous.tick);
            self.used_bytes = self.used_bytes.saturating_sub(previous.vector.len() * size_of::<f32>());
        }

        while self.used_bytes + bytes > self.budget_bytes {
            if !self.evict_one() {
                break;
            }
        }

        self.tick += 1;
        self.order.insert(self.tick, slot);
        self.used_bytes += bytes;
        self.vectors.insert(slot, Cached { vector, tick: self.tick });
    }

    /// Drop one, if present. Returns whether it was there.
    ///
    /// Called when an entry is updated or removed: a stale exact vector under a live slot would
    /// rerank the new code against the old vector, and the answer would look ordinary.
    pub fn invalidate(&mut self, slot: i64) -> bool {
        let Some(entry) = self.vectors.remove(&slot) else { return false };
        self.order.remove(&entry.tick);
        self.used_bytes = self.used_bytes.saturating_sub(entry.vector.len() * size_of::<f32>());
        true
    }

    /// Drop everything. Counters are kept, because they describe the session, not the contents.
    pub fn clear(&mut self) {
        self.vectors.clear();
        self.order.clear();
        self.used_bytes = 0;
    }

    /// Bytes of vector data retained.
    #[must_use]
    pub const fn used_bytes(&self) -> usize {
        self.used_bytes
    }

    /// The budget it is held to.
    #[must_use]
    pub const fn budget_bytes(&self) -> usize {
        self.budget_bytes
    }

    /// How many vectors are retained.
    #[must_use]
    pub fn len(&self) -> usize {
        self.vectors.len()
    }

    /// Whether nothing is retained.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.vectors.is_empty()
    }

    /// Hits, misses and evictions so far.
    ///
    /// Reported rather than kept private because the TUI shows cache behaviour, and a hit rate
    /// that has quietly collapsed is the first sign that the rerank width and the budget no
    /// longer suit each other.
    #[must_use]
    pub const fn stats(&self) -> CacheStats {
        CacheStats { hits: self.hits, misses: self.misses, evictions: self.evictions }
    }

    fn evict_one(&mut self) -> bool {
        let Some((tick, slot)) = self.order.iter().next().map(|(tick, slot)| (*tick, *slot)) else {
            return false;
        };
        self.order.remove(&tick);
        if let Some(entry) = self.vectors.remove(&slot) {
            self.used_bytes = self.used_bytes.saturating_sub(entry.vector.len() * size_of::<f32>());
        }
        self.evictions += 1;
        true
    }
}

/// What the cache has been doing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CacheStats {
    /// Lookups that were retained.
    pub hits: u64,
    /// Lookups that had to go to SQLite.
    pub misses: u64,
    /// Entries dropped to stay inside the budget.
    pub evictions: u64,
}

impl CacheStats {
    /// Hit rate over the lookups so far, or `None` when there have been none.
    ///
    /// `None` rather than zero: no lookups is not a zero hit rate, and a dashboard that showed
    /// 0% for an idle session would read as a fault.
    #[must_use]
    #[allow(
        clippy::cast_precision_loss,
        reason = "a displayed rate is approximate by nature, and the counters cannot reach 2^53: \
                  that is over a hundred days of doing nothing but cache lookups"
    )]
    pub fn hit_rate(&self) -> Option<f64> {
        let total = self.hits + self.misses;
        (total > 0).then(|| self.hits as f64 / total as f64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DIMS: usize = 8;

    /// Takes a `u8` so the tests never cast a slot id into a float: `f32::from(u8)` is
    /// lossless, and a lossy cast in a fixture is still a lossy cast.
    fn vector(fill: u8) -> Vec<f32> {
        vec![f32::from(fill); DIMS]
    }

    fn marked(slot: i64) -> Vec<f32> {
        vector(u8::try_from(slot).unwrap_or(0))
    }

    fn bytes(count: usize) -> usize {
        count * DIMS * size_of::<f32>()
    }

    #[test]
    fn a_stored_vector_comes_back() {
        let mut cache = ExactCache::new(bytes(4));
        cache.put(1, vector(5));
        assert_eq!(cache.get(1), Some(vector(5)));
        assert_eq!(cache.stats(), CacheStats { hits: 1, misses: 0, evictions: 0 });
    }

    #[test]
    fn a_missing_vector_is_a_miss() {
        let mut cache = ExactCache::new(bytes(4));
        assert_eq!(cache.get(99), None);
        assert_eq!(cache.stats().misses, 1);
        assert_eq!(cache.stats().hits, 0);
    }

    #[test]
    fn the_budget_is_respected() {
        let mut cache = ExactCache::new(bytes(3));
        for slot in 1_i64..=5 {
            cache.put(slot, marked(slot));
        }
        assert_eq!(cache.len(), 3);
        assert!(cache.used_bytes() <= cache.budget_bytes());
    }

    #[test]
    fn the_least_recently_used_entry_goes_first() {
        let mut cache = ExactCache::new(bytes(3));
        cache.put(1, vector(1));
        cache.put(2, vector(2));
        cache.put(3, vector(3));

        // Touch 1, making 2 the oldest.
        assert!(cache.get(1).is_some());
        cache.put(4, vector(4));

        assert!(cache.get(1).is_some(), "the recently used entry was evicted");
        assert!(cache.get(2).is_none(), "the oldest entry survived");
        assert!(cache.get(3).is_some());
        assert!(cache.get(4).is_some());
    }

    #[test]
    fn a_reinsert_does_not_double_count_its_bytes() {
        // The failure this prevents: `used_bytes` drifting upward on every update until the
        // cache believes it is full and evicts everything it holds.
        let mut cache = ExactCache::new(bytes(4));
        for _ in 0..10 {
            cache.put(1, vector(1));
        }
        assert_eq!(cache.len(), 1);
        assert_eq!(cache.used_bytes(), bytes(1));
    }

    #[test]
    fn a_reinsert_refreshes_recency() {
        let mut cache = ExactCache::new(bytes(2));
        cache.put(1, vector(1));
        cache.put(2, vector(2));
        cache.put(1, vector(15));
        cache.put(3, vector(3));

        assert_eq!(cache.get(1), Some(vector(15)), "the refreshed entry was evicted");
        assert!(cache.get(2).is_none(), "the stale entry survived");
    }

    #[test]
    fn a_vector_larger_than_the_budget_is_not_retained_and_does_not_flush() {
        // Evicting everything for something that cannot be kept would turn one oversized
        // entry into a cache flush.
        let mut cache = ExactCache::new(bytes(2));
        cache.put(1, vector(1));
        cache.put(2, vector(2));

        cache.put(3, vec![0.0_f32; DIMS * 10]);

        assert_eq!(cache.len(), 2, "the cache was flushed by an entry it could not keep");
        assert!(cache.get(1).is_some());
        assert!(cache.get(2).is_some());
        assert!(cache.get(3).is_none());
    }

    #[test]
    fn a_zero_budget_retains_nothing() {
        // A legitimate configuration on a memory-constrained host, so it is honoured rather
        // than raised to a minimum.
        let mut cache = ExactCache::new(0);
        cache.put(1, vector(1));
        assert!(cache.is_empty());
        assert!(cache.get(1).is_none());
        assert_eq!(cache.used_bytes(), 0);
    }

    #[test]
    fn invalidating_drops_the_entry_and_its_bytes() {
        // A stale exact vector under a live slot would rerank the new code against the old
        // vector, and the answer would look ordinary.
        let mut cache = ExactCache::new(bytes(4));
        cache.put(1, vector(1));
        assert!(cache.invalidate(1));
        assert!(cache.get(1).is_none());
        assert_eq!(cache.used_bytes(), 0);
        assert!(!cache.invalidate(1), "invalidating twice should report absence");
    }

    #[test]
    fn clearing_frees_the_bytes_but_keeps_the_counters() {
        let mut cache = ExactCache::new(bytes(4));
        cache.put(1, vector(1));
        assert!(cache.get(1).is_some());
        cache.clear();

        assert!(cache.is_empty());
        assert_eq!(cache.used_bytes(), 0);
        assert_eq!(cache.stats().hits, 1, "the counters describe the session, not the contents");
    }

    #[test]
    fn eviction_is_counted() {
        let mut cache = ExactCache::new(bytes(1));
        cache.put(1, vector(1));
        cache.put(2, vector(2));
        assert_eq!(cache.stats().evictions, 1);
    }

    #[test]
    fn the_hit_rate_is_undefined_before_the_first_lookup() {
        let cache = ExactCache::new(bytes(4));
        assert!(cache.stats().hit_rate().is_none());
    }

    #[test]
    fn the_hit_rate_counts_both_outcomes() {
        let mut cache = ExactCache::new(bytes(4));
        cache.put(1, vector(1));
        assert!(cache.get(1).is_some());
        assert!(cache.get(2).is_none());
        let rate = cache.stats().hit_rate().expect("two lookups");
        assert!((rate - 0.5).abs() < f64::EPSILON, "{rate}");
    }

    #[test]
    fn the_recency_index_does_not_leak_as_entries_are_touched() {
        // Every access moves an entry to a new tick, and the old tick has to be removed with
        // it. If it were not, the order index would grow without bound and eviction would keep
        // finding ticks whose entries are long gone.
        let mut cache = ExactCache::new(bytes(4));
        for slot in 1_i64..=4 {
            cache.put(slot, marked(slot));
        }
        for _ in 0..100 {
            for slot in 1_i64..=4 {
                assert!(cache.get(slot).is_some());
            }
        }
        assert_eq!(cache.order.len(), cache.vectors.len(), "the recency index leaked");
        assert_eq!(cache.len(), 4);
    }

    #[test]
    fn eviction_keeps_the_two_indexes_agreeing() {
        // The order map and the vector map are two views of one set. If they drift, eviction
        // starts removing ticks whose slots are absent and the budget stops being enforced.
        let mut cache = ExactCache::new(bytes(3));
        for slot in 1_i64..=20 {
            cache.put(slot, marked(slot));
            if slot % 3 == 0 {
                let _ = cache.get(slot - 1);
            }
            if slot % 5 == 0 {
                cache.invalidate(slot);
            }
            assert_eq!(cache.order.len(), cache.vectors.len(), "drifted at slot {slot}");
            assert!(cache.used_bytes() <= cache.budget_bytes(), "over budget at slot {slot}");
        }
    }
}
