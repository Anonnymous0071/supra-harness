//! Cohort profiles: what past tasks taught, keyed by task shape.
//!
//! # What a profile is
//!
//! The tier a task shape earned last time, and how many consecutive failures
//! it carries. "Task shape" is the stable part of a task - its tool classes,
//! its file areas, its reversibility - not its prose, which never repeats
//! verbatim. Two tasks with the same shape share a profile; the profile is how
//! the second one learns from the first without a model call.
//!
//! # Why saturating, and why two
//!
//! `repeat_failures >= 2` escalates to E5 (the scoring ladder's first rule),
//! so the counter saturates at 2: the third failure teaches nothing the second
//! did not, and an unbounded counter would let one flaky task shape pin every
//! future task at E5. Success resets to zero - a shape that recovers has
//! recovered, and carrying old failures forward would tax every later task
//! for a defect that is gone.
//!
//! # Where profiles live
//!
//! The turn loop persists them (step 13: `task_profiles`), keyed by shape.
//! This crate owns the shape key and the update rule; storage belongs to T26's
//! session file, not here. A profile is pure data - no handles, no locks - so
//! it serialises with serde and compares with `==`, which is what makes the
//! persistence a write rather than a migration.

use serde::{Deserialize, Serialize};

/// What identifies a task shape: the stable parts, never the prose.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TaskShape {
    /// Tool classes the task's tools belong to, sorted.
    pub tool_classes: Vec<String>,
    /// File areas touched (`auth`, `crypto`, `migrat`, or `general`), sorted.
    pub areas: Vec<String>,
    /// Requested reversibility class, as its name.
    pub reversibility: String,
}

impl TaskShape {
    /// The shape key: stable across runs, human-readable in the session file.
    #[must_use]
    pub fn key(&self) -> String {
        format!("{}|{}|{}", self.tool_classes.join(","), self.areas.join(","), self.reversibility)
    }
}

/// What a task shape earned.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TaskProfile {
    /// Consecutive failures, saturating at 2. Success resets to zero.
    pub repeat_failures: u32,
    /// Tier the shape ran at last, for the accuracy metric.
    pub last_tier: supra_types::Tier,
}

impl TaskProfile {
    /// A shape never seen before: no failures, no history.
    #[must_use]
    pub const fn fresh() -> Self {
        Self { repeat_failures: 0, last_tier: supra_types::Tier::E0 }
    }

    /// Record a success: failures reset, tier updates.
    #[must_use]
    pub const fn succeeded(mut self, tier: supra_types::Tier) -> Self {
        self.repeat_failures = 0;
        self.last_tier = tier;
        self
    }

    /// Record a failure: failures saturate at 2, tier updates.
    #[must_use]
    pub const fn failed(mut self, tier: supra_types::Tier) -> Self {
        // `u32::min` is not const-stable on the pinned MSRV (1.85); the branch
        // is the same saturation with no trait bound involved.
        self.repeat_failures = if self.repeat_failures >= 2 { 2 } else { self.repeat_failures + 1 };
        self.last_tier = tier;
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failures_saturate_at_two() {
        let profile = TaskProfile::fresh().failed(supra_types::Tier::E2);
        assert_eq!(profile.repeat_failures, 1);
        let profile = profile.failed(supra_types::Tier::E2);
        assert_eq!(profile.repeat_failures, 2);
        let profile = profile.failed(supra_types::Tier::E2);
        assert_eq!(profile.repeat_failures, 2, "saturation, not accumulation");
    }

    #[test]
    fn success_resets() {
        let profile = TaskProfile::fresh().failed(supra_types::Tier::E2).failed(supra_types::Tier::E2);
        assert_eq!(profile.repeat_failures, 2);
        let profile = profile.succeeded(supra_types::Tier::E1);
        assert_eq!(profile.repeat_failures, 0);
        assert_eq!(profile.last_tier, supra_types::Tier::E1);
    }

    #[test]
    fn shape_keys_are_stable_and_human_readable() {
        let shape = TaskShape {
            tool_classes: vec!["read".to_owned()],
            areas: vec!["general".to_owned()],
            reversibility: "R1".to_owned(),
        };
        assert_eq!(shape.key(), "read|general|R1");
        assert_eq!(shape.key(), shape.clone().key());
    }
}
