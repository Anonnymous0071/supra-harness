//! The admission decision: tier to field, cohort size, quorum, shards.
//!
//! # What admission owns
//!
//! Turning an estimated tier and a configured peer limit into the cohort that
//! actually runs: the tier after the limit (`admit`), its size, its quorum,
//! its byzantine tolerance, and its shard count. T6 owns the arithmetic
//! (`admit`, `quorum`, `byzantine_tolerance`, `shards_needed`); this module
//! owns the record that carries the answers together, so the turn loop reads
//! one struct instead of recomputing four numbers - and recomputing them is
//! how they drift apart.
//!
//! # Why a struct and not four calls
//!
//! The turn loop needs k, quorum, and shards in three different places
//! (spawn, vote, fan-out). Four separate calls at three sites is twelve
//! chances to pass a different k to one of them. One struct, built once,
//! read everywhere: the numbers cannot disagree because there is one of each.

use supra_types::{Tier, admit, byzantine_tolerance, quorum, shards_needed};

/// The cohort that will actually run.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Admission {
    /// Tier requested by estimation.
    pub requested: Tier,
    /// Tier that will run after the peer limit.
    pub tier: Tier,
    /// Peers to field.
    pub k: usize,
    /// Votes in favour needed to carry a claim.
    pub needed: usize,
    /// Faulty peers the cohort absorbs.
    pub faulty: usize,
    /// Rate-limit shards (I6: at most 15 requests/minute per prefix).
    pub shards: usize,
    /// The configured peer limit the decision respected.
    pub limit: usize,
}

impl Admission {
    /// Admit a requested tier under a peer limit.
    ///
    /// Returns `None` only when the limit is zero - nothing can run at all.
    /// Otherwise the result always satisfies
    /// `Tier::containing(k) == Some(tier)`: the gaps stay unreachable because
    /// selection runs tier to k, never k to tier.
    #[must_use]
    pub fn decide(requested: Tier, limit: usize) -> Option<Self> {
        let (tier, k) = admit(requested, limit)?;
        Some(Self {
            requested,
            tier,
            k,
            needed: quorum(k),
            faulty: byzantine_tolerance(k),
            shards: shards_needed(k),
            limit,
        })
    }

    /// Whether the limit reduced the requested tier.
    #[must_use]
    pub const fn reduced(&self) -> bool {
        self.tier as u8 != self.requested as u8
    }
}

/// Escalation: what the turn loop does when a cohort cannot carry its claim.
///
/// Three triggers, in the order the loop checks them, each with a distinct
/// remedy. The order matters: an unreachable quorum is known *now* (escalate
/// now, do not await the timeout); a blind-verification mismatch is known at
/// E0 only (E0's guarantee is self-consistency, so a mismatch means the model
/// disagrees with itself and needs real peers); a confirmed finding raises the
/// tier for the *next* attempt (the current attempt's evidence is stale).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Escalation {
    /// Quorum can no longer be reached (`yes + pending < needed`).
    ///
    /// Escalate immediately rather than after the 30s timeout: waiting would
    /// pay for peers whose answers cannot change the outcome.
    UnreachableQuorum,
    /// E0's blind re-derivation differs from the claim.
    ///
    /// Escalates E0 straight to E2, skipping E1: a model that disagrees with
    /// itself needs independent review, not one more self-consistent pass, and
    /// E1 (k=2, quorum 2) is still unanimity-shaped scrutiny.
    BlindMismatch,
    /// A finding was confirmed against the claim.
    ///
    /// Raises the tier one step for the next attempt: the defect is confirmed,
    /// so the next cohort earns the next tier's scrutiny.
    FindingConfirmed,
}

impl Escalation {
    /// The tier the next attempt earns.
    ///
    /// `UnreachableQuorum` and `BlindMismatch` jump to E2 minimum: both mean
    /// the current scrutiny was structurally insufficient, not merely unlucky.
    /// `FindingConfirmed` steps one tier: the evidence grew by one finding, so
    /// the tier grows by one step. E5 saturates - there is no E6, and admitting
    /// that is better than wrapping around to E0.
    #[must_use]
    pub const fn next_tier(self, current: Tier) -> Tier {
        match self {
            Self::UnreachableQuorum | Self::BlindMismatch => match current {
                Tier::E0 | Tier::E1 => Tier::E2,
                Tier::E2 => Tier::E3,
                Tier::E3 => Tier::E4,
                Tier::E4 | Tier::E5 => Tier::E5,
            },
            Self::FindingConfirmed => match current {
                Tier::E0 => Tier::E1,
                Tier::E1 => Tier::E2,
                Tier::E2 => Tier::E3,
                Tier::E3 => Tier::E4,
                Tier::E4 | Tier::E5 => Tier::E5,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn admission_carries_consistent_numbers() {
        // One struct, built once: k, quorum, shards cannot disagree because
        // there is one of each. Cross-checked against T6's functions directly.
        for requested in Tier::ALL {
            for limit in [1, 6, 14, 16, 80] {
                let Some(admission) = Admission::decide(requested, limit) else { continue };
                assert_eq!(admission.needed, quorum(admission.k));
                assert_eq!(admission.faulty, byzantine_tolerance(admission.k));
                assert_eq!(admission.shards, shards_needed(admission.k));
                assert!(admission.k <= limit);
                assert_eq!(Tier::containing(admission.k), Some(admission.tier));
            }
        }
    }

    #[test]
    fn a_zero_limit_admits_nothing() {
        assert_eq!(Admission::decide(Tier::E5, 0), None);
    }

    #[test]
    fn reduced_reports_whether_the_limit_bit() {
        let same = Admission::decide(Tier::E1, 80).expect("fits");
        assert!(!same.reduced());
        let cut = Admission::decide(Tier::E5, 6).expect("reduces");
        assert_eq!(cut.tier, Tier::E2);
        assert!(cut.reduced());
    }

    #[test]
    fn unreachable_quorum_skips_e1() {
        assert_eq!(Escalation::UnreachableQuorum.next_tier(Tier::E0), Tier::E2);
        assert_eq!(Escalation::UnreachableQuorum.next_tier(Tier::E1), Tier::E2);
        assert_eq!(Escalation::UnreachableQuorum.next_tier(Tier::E3), Tier::E4);
        assert_eq!(Escalation::UnreachableQuorum.next_tier(Tier::E5), Tier::E5);
    }

    #[test]
    fn blind_mismatch_skips_e1_for_the_same_reason() {
        // E1 (k=2, quorum 2) demands unanimity in effect; a model that
        // disagrees with itself needs peers that can disagree with each other.
        assert_eq!(Escalation::BlindMismatch.next_tier(Tier::E0), Tier::E2);
        assert_eq!(Escalation::BlindMismatch.next_tier(Tier::E2), Tier::E3);
    }

    #[test]
    fn a_confirmed_finding_steps_one_tier() {
        assert_eq!(Escalation::FindingConfirmed.next_tier(Tier::E0), Tier::E1);
        assert_eq!(Escalation::FindingConfirmed.next_tier(Tier::E2), Tier::E3);
        assert_eq!(Escalation::FindingConfirmed.next_tier(Tier::E5), Tier::E5);
    }
}
