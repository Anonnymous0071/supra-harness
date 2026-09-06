//! Cohort signals: the evidence tier estimation reads, as plain integers.
//!
//! # What a signal is
//!
//! One measured fact about the task at hand, expressed as an integer the
//! scoring ladder can compare. Signals come from four places the architecture
//! document names: the digest (blast radius, churn, anchor count), the tool
//! registry (requested reversibility class), findings (active count), and past
//! task profiles (repeat failures).
//!
//! # Why integers, and why bands rather than raw counts
//!
//! The scoring ladder in [`crate::score`] compares each signal against fixed
//! thresholds, and thresholds over raw counts drift with repository size: a
//! blast radius of 8 is alarming in a 10-file tool and routine in a 10k-file
//! monorepo. Bands (`Small | Medium | Large`) normalise the comparison at the
//! signal's construction site, where the denominator is known, rather than in
//! the ladder, where it is not. The ladder then reads bands, never counts -
//! which is what keeps the ladder stable while repositories grow around it.
//!
//! # No floats anywhere
//!
//! Signal strengths are ordinals, not magnitudes. A float confidence would
//! invite arithmetic - averaging, thresholding - that reads as rigour while
//! resting on a model's self-report, and it would be the only float in the
//! contract layer (T6's rule, enforced by `check-invariants.sh`). Bands are
//! the shape that refuses that arithmetic structurally.

use supra_types::Reversibility;

/// How much blast radius the task's files carry.
///
/// Bands are calibrated against the architecture document's tier triggers:
/// E1 caps at 2, E2 at 10. A radius above 10 is cross-module by the document's
/// own definition, whatever the repository's size.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum BlastBand {
    /// Zero or one dependent: a read, or an edit nothing imports.
    Isolated,
    /// Up to ten dependents: multi-file, within one module's reach.
    Contained,
    /// Above ten: cross-module by definition.
    Wide,
}

impl BlastBand {
    /// Band a file count into [`BlastBand`].
    #[must_use]
    pub const fn of(dependents: usize) -> Self {
        match dependents {
            0 | 1 => Self::Isolated,
            2..=10 => Self::Contained,
            _ => Self::Wide,
        }
    }
}

/// How much churn the task's files carry.
///
/// Bands are calibrated against "high churn" as the E3 trigger: a file touched
/// recently is being actively reworked, and rework plus an edit is how a fix
/// becomes a regression. Counts are commits in the last 90 days, T15's unit.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ChurnBand {
    /// No recent commits: stable ground.
    Calm,
    /// A handful of recent commits: active, not alarming.
    Active,
    /// Sustained recent rework: the ground is moving under the edit.
    Hot,
}

impl ChurnBand {
    /// Band a 90-day commit count into [`ChurnBand`].
    #[must_use]
    pub const fn of(commits_90d: u32) -> Self {
        match commits_90d {
            0 => Self::Calm,
            1..=5 => Self::Active,
            _ => Self::Hot,
        }
    }
}

/// How much the digest found for the task.
///
/// Bands are calibrated against the suffix budget (T15: 10 anchors): a full
/// anchor set means the task touches many symbols, a partial set means a few,
/// an empty set means the digest has nothing to orient on.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum AnchorBand {
    /// No anchors: the digest found nothing relevant.
    None,
    /// Some anchors, under half the budget: a focused task.
    Few,
    /// Half the budget or more: the task sprawls.
    Many,
}

impl AnchorBand {
    /// Band an anchor count into [`AnchorBand`].
    #[must_use]
    pub const fn of(anchors: usize) -> Self {
        match anchors {
            0 => Self::None,
            1..=4 => Self::Few,
            _ => Self::Many,
        }
    }
}

/// The evidence tier estimation reads.
///
/// Every field is a band or a count the ladder thresholds, never prose and
/// never a float. Constructed by the turn loop (T23) from the digest, the tool
/// registry, the findings ledger, and past task profiles - this crate only
/// reads the struct, which is what keeps estimation a pure function.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Signals {
    /// Blast radius of the task's files, banded.
    pub blast: BlastBand,
    /// Churn of the task's files, banded.
    pub churn: ChurnBand,
    /// Anchors the digest returned, banded.
    pub anchors: AnchorBand,
    /// Requested reversibility class of the task's tools.
    pub reversibility: Reversibility,
    /// Active (unresolved, confirmed) findings touching the task's files.
    pub active_findings: usize,
    /// Consecutive failures of this task profile, saturating.
    pub repeat_failures: u32,
}

impl Signals {
    /// The cheapest possible evidence: a read of an isolated, calm file with no
    /// anchors, no findings, no failures, and no side effects.
    #[must_use]
    pub const fn minimal() -> Self {
        Self {
            blast: BlastBand::Isolated,
            churn: ChurnBand::Calm,
            anchors: AnchorBand::None,
            reversibility: Reversibility::R0,
            active_findings: 0,
            repeat_failures: 0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blast_bands_follow_the_document_triggers() {
        assert_eq!(BlastBand::of(0), BlastBand::Isolated);
        assert_eq!(BlastBand::of(1), BlastBand::Isolated);
        assert_eq!(BlastBand::of(2), BlastBand::Contained);
        assert_eq!(BlastBand::of(10), BlastBand::Contained);
        assert_eq!(BlastBand::of(11), BlastBand::Wide);
    }

    #[test]
    fn bands_order_with_severity() {
        assert!(BlastBand::Isolated < BlastBand::Contained);
        assert!(BlastBand::Contained < BlastBand::Wide);
        assert!(ChurnBand::Calm < ChurnBand::Active);
        assert!(ChurnBand::Active < ChurnBand::Hot);
        assert!(AnchorBand::None < AnchorBand::Few);
        assert!(AnchorBand::Few < AnchorBand::Many);
    }

    #[test]
    fn anchor_bands_split_the_suffix_budget() {
        assert_eq!(AnchorBand::of(0), AnchorBand::None);
        assert_eq!(AnchorBand::of(4), AnchorBand::Few);
        assert_eq!(AnchorBand::of(5), AnchorBand::Many);
        assert_eq!(AnchorBand::of(10), AnchorBand::Many);
    }
}
