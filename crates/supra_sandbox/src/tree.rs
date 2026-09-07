//! Process-tree accounting.
//!
//! The T12.5 notes bind this directly: "the env-marker guard layer can be
//! stripped by a command that deliberately clears the environment, and the
//! process-tree budget catches the consequence rather than the intent." A
//! process whose `SUPRA_SPAWN` marker is missing has nothing left to verify
//! it by, and refusing every such process would refuse the user's own
//! shell, so the budget is the only mechanism that catches a tree the guard
//! cannot.
//!
//! The budget is a single number: the number of **direct** children this
//! process started since startup. A grandchild does not consume a slot, by
//! design - what the guard cannot verify is the first hop, and the first
//! hop is what the budget limits. The total across a session is what the
//! TUI status line shows; the per-turn burst is what `exceeded_burst`
//! answers.
//!
//! # Why a counter, not a tree
//!
//! The T12.5 note is about *consequence*, not lineage. A process that
//! escapes the marker check has no chain to trace: it is, by definition,
//! detached. The accounting needs the *count* of the unmarked processes
//! the host has started, because that is the number the user can watch
//! grow, and the only input the budget needs to enforce.

use std::sync::atomic::{AtomicU32, Ordering};

/// Per-host counter for the children the guard cannot otherwise catch.
///
/// `Ordering::Relaxed` is sufficient: the only operation that reads is a
/// read-and-compare against the burst limit, and the limit is a UI affordance
/// rather than a security boundary. A lost increment would be a lost UI
/// beat, not a missing guarantee.
#[derive(Debug)]
pub struct TreeBudget {
    /// Cumulative children started, monotonic for the process lifetime.
    total: AtomicU32,
    /// Children started in the most recent burst window.
    burst: AtomicU32,
    /// Burst size the operator has approved for this session.
    burst_limit: u32,
}

impl TreeBudget {
    /// A new budget. `burst_limit` is the maximum number of children that
    /// may be started in a single burst; the TUI's status line shows the
    /// current burst count.
    #[must_use]
    pub const fn new(burst_limit: u32) -> Self {
        Self { total: AtomicU32::new(0), burst: AtomicU32::new(0), burst_limit }
    }

    /// Record one child started. The guard's marker check is the *primary*
    /// gate; this counter is the *consequence* catch.
    ///
    /// Returns the post-increment value so a caller can render the new
    /// total without a second atomic load.
    pub fn record_child(&self) -> u32 {
        self.burst.fetch_add(1, Ordering::Relaxed);
        self.total.fetch_add(1, Ordering::Relaxed) + 1
    }

    /// Children started since the last call to [`reset_burst`](Self::reset_burst).
    #[must_use]
    pub fn burst_count(&self) -> u32 {
        self.burst.load(Ordering::Relaxed)
    }

    /// Total children started, since this [`TreeBudget`] was created.
    #[must_use]
    pub fn total_count(&self) -> u32 {
        self.total.load(Ordering::Relaxed)
    }

    /// The burst limit set at construction.
    #[must_use]
    pub const fn burst_limit(&self) -> u32 {
        self.burst_limit
    }

    /// Whether `count` exceeds the burst limit.
    ///
    /// `count` is whatever the caller recorded most recently, so the
    /// check is independent of the atomic load above: a UI thread can ask
    /// "should I show a warning?" with the value it already has.
    #[must_use]
    pub const fn exceeded_burst(&self, count: u32) -> bool {
        count > self.burst_limit
    }

    /// Reset the burst counter. The TUI calls this on user-visible
    /// boundaries - a turn ending, a slash command completing - so the
    /// displayed burst is the size of the current activity, not the
    /// whole session.
    pub fn reset_burst(&self) {
        self.burst.store(0, Ordering::Relaxed);
    }
}

impl Default for TreeBudget {
    fn default() -> Self {
        // 32 is a small enough number to be a visible spike, large enough to
        // be invisible during normal compilation. The value is a UI hint;
        // authority is in the guard, not here.
        Self::new(32)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_counter_starts_at_zero() {
        let budget = TreeBudget::new(8);
        assert_eq!(budget.total_count(), 0);
        assert_eq!(budget.burst_count(), 0);
        assert!(!budget.exceeded_burst(0));
    }

    #[test]
    fn record_child_advances_total_and_burst() {
        let budget = TreeBudget::new(8);
        let now = budget.record_child();
        assert_eq!(now, 1);
        assert_eq!(budget.total_count(), 1);
        assert_eq!(budget.burst_count(), 1);
    }

    #[test]
    fn burst_reset_keeps_total() {
        let budget = TreeBudget::new(8);
        for _ in 0..5 {
            budget.record_child();
        }
        assert_eq!(budget.total_count(), 5);
        assert_eq!(budget.burst_count(), 5);

        budget.reset_burst();
        assert_eq!(budget.burst_count(), 0);
        assert_eq!(budget.total_count(), 5, "the session total survives a burst reset");
    }

    #[test]
    fn exceeded_burst_compares_against_the_limit() {
        let budget = TreeBudget::new(2);
        assert!(!budget.exceeded_burst(0));
        assert!(!budget.exceeded_burst(1));
        assert!(!budget.exceeded_burst(2));
        assert!(budget.exceeded_burst(3), "the limit is 2, not 3");
    }

    #[test]
    fn the_default_limit_is_a_modest_burst() {
        let budget = TreeBudget::default();
        // 32 is what T29 shows as "a busy turn". The value is a hint to the
        // operator; the guard is the gate, and the guard does not consult it.
        assert_eq!(budget.burst_limit(), 32);
    }
}
