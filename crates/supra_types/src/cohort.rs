//! Elastic peer cohorts: sizing, quorum arithmetic, and the verdict budget.
//!
//! No agent is privileged. There is no lead, manager, or coordinator; peers publish
//! claims to a shared blackboard and vote on each other's. What this module owns is
//! the arithmetic that decides when they have agreed, because two pieces of it are
//! easy to get wrong in ways that are hard to notice.
//!
//! # Quorum must be rational, not floating point
//!
//! The threshold is `ceil(2k/3)`. Computing it as `(0.67 * k).ceil()` gives 3 at
//! k=3, because `0.67 * 3` is `2.01` - so a three-peer cohort would silently demand
//! unanimity, and any single dissent would escalate. The integer form here is
//! `(2k + 2) / 3`, and a test walks every k from 1 to 80 comparing it against the
//! exact rational value and against the float trap.
//!
//! # Output cost dominates at large k
//!
//! Cache discounts input only. At k=80 the inputs are 79 cache reads at 10% while
//! every peer's output is billed in full, so output is where the money goes. That is
//! why a proposer writes fully and a validator emits a bounded [`Verdict`] - and why
//! the bound is enforced by the constructor rather than requested in a prompt. An
//! instruction is advisory; a schema is not.
//!
//! # Tier ranges
//!
//! [`Tier::k_range`] reproduces the architecture document's table exactly, including
//! its gaps: k of 6, 13 to 15, and 33 to 80 within E4's shadow are simply not
//! produced by any tier. The direction of travel is tier to k - T15.5 scores
//! evidence into a tier, and the tier fixes the range - so a k with no tier is not a
//! case that arises. [`Tier::containing`] returns `None` for those values rather
//! than inventing a mapping.

use serde::{Deserialize, Serialize};

/// Absolute ceiling on peers in one cohort.
///
/// A hard limit, not a default: no configuration raises it.
pub const PEER_CEILING: usize = 80;

/// Default configured ceiling.
///
/// The elastic range is `1..=PEER_CEILING`, and this is where a fresh installation
/// caps it. It is a cap on what tiers may ask for, not a cohort size: a one-file
/// edit still runs at k=1 with the limit set to 16.
pub const DEFAULT_PEER_LIMIT: usize = 16;

/// Maximum requests per minute to one prefix before traffic may migrate machines.
///
/// A cache entry becomes available only after the first response byte, and traffic
/// above this rate may land on a different machine that has no entry. Exceeding it
/// turns k cache reads into k cache writes - a 12.5x error at the 5 minute TTL.
pub const MAX_REQUESTS_PER_MINUTE: usize = 15;

// The default limit must sit inside the absolute ceiling. Checked at compile time
// rather than in a test: a runtime assertion on two constants is a tautology the
// compiler can settle.
const _: () = assert!(DEFAULT_PEER_LIMIT <= PEER_CEILING);

/// Peers needed to carry a claim: `ceil(2k/3)`.
///
/// Integer arithmetic throughout. See the module documentation for why the float
/// spelling of this is a bug rather than a rounding preference.
#[must_use]
pub const fn quorum(k: usize) -> usize {
    if k == 0 {
        return 0;
    }
    // ceil(2k/3) in integers. Spelled with div_ceil rather than the equivalent
    // (2k + 2) / 3, because the name states the intent while the arithmetic trick
    // reads like something to be "simplified" back into a float.
    (2 * k).div_ceil(3)
}

/// Faulty peers a cohort of `k` can absorb: `floor((k-1)/3)`.
///
/// Only meaningful from k=4 upward. Below that the guarantee comes from the
/// deterministic gates - compile, affected tests, introspector, LSP - and not from
/// voting, which is worth stating because a two-peer "consensus" can look like
/// agreement while being one model agreeing with itself.
#[must_use]
pub const fn byzantine_tolerance(k: usize) -> usize {
    if k == 0 {
        return 0;
    }
    (k - 1) / 3
}

/// Shards needed to keep a cohort under the per-minute rate limit.
///
/// Each shard gets its own `prompt_cache_key`, and all shards share a
/// byte-identical prefix.
#[must_use]
pub const fn shards_needed(k: usize) -> usize {
    k.div_ceil(MAX_REQUESTS_PER_MINUTE)
}

/// How much evidence a task carries, and therefore how many peers it earns.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum Tier {
    /// One peer, verified by blind re-derivation. Q&A, a single-file read, blast
    /// radius zero.
    E0,
    /// Two peers. A single-file edit, blast radius at most two.
    E1,
    /// Three to five. Multi-file, blast radius at most ten.
    E2,
    /// Seven to twelve. Cross-module, high churn.
    E3,
    /// Sixteen to thirty-two. Auth, crypto, migrations, active findings.
    E4,
    /// Up to the ceiling. User-requested, or escalation after repeated failure.
    E5,
}

impl Tier {
    /// Every tier, ascending.
    pub const ALL: [Self; 6] = [Self::E0, Self::E1, Self::E2, Self::E3, Self::E4, Self::E5];

    /// The cohort sizes this tier selects from, inclusive.
    #[must_use]
    pub const fn k_range(self) -> (usize, usize) {
        match self {
            Self::E0 => (1, 1),
            Self::E1 => (2, 2),
            Self::E2 => (3, 5),
            Self::E3 => (7, 12),
            Self::E4 => (16, 32),
            Self::E5 => (33, PEER_CEILING),
        }
    }

    /// The tier that selects `k`, if any.
    ///
    /// `None` for the values the table skips. See the module documentation: those
    /// sizes are unreachable rather than unspecified.
    #[must_use]
    pub fn containing(k: usize) -> Option<Self> {
        Self::ALL.into_iter().find(|tier| {
            let (low, high) = tier.k_range();
            (low..=high).contains(&k)
        })
    }

    /// Whether verification at this tier is self-consistency rather than
    /// independent review.
    ///
    /// True only at [`Tier::E0`], and stated as a method so the distinction is hard
    /// to forget. At k=1 the second pass receives the claim **without the reasoning
    /// that produced it** and re-derives it, which catches a slip but not a
    /// misconception: the same model can be wrong the same way twice. The real
    /// guarantee at E0 comes from the deterministic gates running alongside.
    #[must_use]
    pub const fn is_blind_rederivation(self) -> bool {
        matches!(self, Self::E0)
    }

    /// Whether byzantine tolerance is meaningful at this tier's smallest cohort.
    #[must_use]
    pub const fn has_byzantine_tolerance(self) -> bool {
        let (low, _) = self.k_range();
        byzantine_tolerance(low) > 0
    }
}

/// A validator's answer to a claim.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Vote {
    /// The claim holds.
    Yes,
    /// The claim does not hold.
    No,
    /// Not enough information to say.
    ///
    /// Counted separately from `No` because it reduces the reachable total without
    /// being evidence against: a claim that abstains its way below quorum should
    /// escalate for more peers, not be recorded as refuted.
    Abstain,
}

/// How sure a validator is.
///
/// An enum rather than a number, deliberately. A float confidence invites
/// arithmetic - averaging, thresholding - that reads as rigour while resting on a
/// model's self-report, and it would be the only float in the contract layer.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum Confidence {
    /// Plausible, unverified.
    Low,
    /// Checked against the claim's own evidence.
    Medium,
    /// Confirmed against a deterministic gate.
    High,
}

/// A validator's bounded response.
///
/// Bounded because output is never cached and dominates cost at large k. The bound
/// lives in the constructor so it cannot be talked out of.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "VerdictRepr")]
pub struct Verdict {
    vote: Vote,
    confidence: Confidence,
    reason: String,
    evidence_ref: Option<String>,
}

impl Verdict {
    /// Byte budget for the reason.
    ///
    /// The whole verdict is budgeted at 60 tokens. `vote`, `confidence`, the
    /// evidence reference, and the JSON keys carrying them account for roughly 25
    /// of those, leaving about 35 tokens of prose. English prose runs near four
    /// bytes per token, so 140 bytes. T13 does the exact per-model count; this is
    /// the schema bound, which is the one that holds when no one is looking.
    pub const MAX_REASON_BYTES: usize = 140;

    /// Byte budget for the evidence reference.
    ///
    /// A pointer - a file and line, a claim id, a gate name - not a quotation.
    pub const MAX_EVIDENCE_BYTES: usize = 40;

    /// Build a verdict, refusing one that exceeds its budget.
    ///
    /// # Errors
    ///
    /// [`VerdictError::ReasonTooLong`] or [`VerdictError::EvidenceRefTooLong`]. It
    /// refuses rather than truncating: truncating a reason mid-sentence produces a
    /// verdict that reads as complete and is not, and truncating bytes can split a
    /// UTF-8 sequence.
    pub fn new(
        vote: Vote,
        confidence: Confidence,
        reason: impl Into<String>,
        evidence_ref: Option<String>,
    ) -> Result<Self, VerdictError> {
        let reason = reason.into();
        if reason.len() > Self::MAX_REASON_BYTES {
            return Err(VerdictError::ReasonTooLong { bytes: reason.len() });
        }
        if let Some(reference) = &evidence_ref {
            if reference.len() > Self::MAX_EVIDENCE_BYTES {
                return Err(VerdictError::EvidenceRefTooLong { bytes: reference.len() });
            }
        }
        Ok(Self { vote, confidence, reason, evidence_ref })
    }

    /// The vote.
    #[must_use]
    pub const fn vote(&self) -> Vote {
        self.vote
    }

    /// How sure the validator is.
    #[must_use]
    pub const fn confidence(&self) -> Confidence {
        self.confidence
    }

    /// Why.
    #[must_use]
    pub fn reason(&self) -> &str {
        &self.reason
    }

    /// Pointer to supporting evidence, if any.
    #[must_use]
    pub fn evidence_ref(&self) -> Option<&str> {
        self.evidence_ref.as_deref()
    }
}

/// Staging area for [`Verdict`] deserialisation, so a stored verdict is rebudgeted.
#[derive(Deserialize)]
struct VerdictRepr {
    vote: Vote,
    confidence: Confidence,
    reason: String,
    evidence_ref: Option<String>,
}

impl TryFrom<VerdictRepr> for Verdict {
    type Error = VerdictError;

    fn try_from(repr: VerdictRepr) -> Result<Self, Self::Error> {
        Self::new(repr.vote, repr.confidence, repr.reason, repr.evidence_ref)
    }
}

/// Why a verdict was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum VerdictError {
    /// The reason exceeded its byte budget.
    #[error("a verdict reason may hold {max} bytes, found {bytes}", max = Verdict::MAX_REASON_BYTES)]
    ReasonTooLong {
        /// Bytes supplied.
        bytes: usize,
    },
    /// The evidence reference exceeded its byte budget.
    #[error(
        "an evidence reference may hold {max} bytes, found {bytes}",
        max = Verdict::MAX_EVIDENCE_BYTES
    )]
    EvidenceRefTooLong {
        /// Bytes supplied.
        bytes: usize,
    },
}

/// Where a claim stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum QuorumStatus {
    /// Still collecting, and quorum is still reachable.
    Open,
    /// Enough peers agreed. The turn loop aborts whatever is still in flight.
    Reached,
    /// Quorum can no longer be reached. The turn loop escalates **now** rather than
    /// waiting for the remaining peers or for a timeout.
    Unreachable,
}

/// Incremental quorum accounting for one claim.
///
/// Incremental because the turn loop evaluates after every vote: waiting for all k
/// responses would pay for peers whose answers cannot change the outcome, and
/// waiting for a timeout to discover an unreachable quorum wastes the 30 seconds
/// that timeout allows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct QuorumTally {
    k: usize,
    needed: usize,
    yes: usize,
    no: usize,
    abstain: usize,
}

impl QuorumTally {
    /// Start a tally for a cohort of `k`.
    #[must_use]
    pub const fn new(k: usize) -> Self {
        Self { k, needed: quorum(k), yes: 0, no: 0, abstain: 0 }
    }

    /// Cohort size.
    #[must_use]
    pub const fn k(&self) -> usize {
        self.k
    }

    /// Votes in favour needed to carry the claim.
    #[must_use]
    pub const fn needed(&self) -> usize {
        self.needed
    }

    /// Votes in favour so far.
    #[must_use]
    pub const fn yes(&self) -> usize {
        self.yes
    }

    /// Votes against so far.
    #[must_use]
    pub const fn no(&self) -> usize {
        self.no
    }

    /// Abstentions so far.
    #[must_use]
    pub const fn abstain(&self) -> usize {
        self.abstain
    }

    /// Peers who have not answered.
    #[must_use]
    pub const fn pending(&self) -> usize {
        self.k - self.yes - self.no - self.abstain
    }

    /// Record a vote and report where the claim now stands.
    ///
    /// # Errors
    ///
    /// [`TallyError::Overfull`] when more votes arrive than the cohort has peers.
    /// That is a caller defect - a peer voting twice, or a vote attributed to the
    /// wrong claim - and admitting it would corrupt the `pending` count that
    /// [`QuorumStatus::Unreachable`] is computed from.
    pub fn record(&mut self, vote: Vote) -> Result<QuorumStatus, TallyError> {
        if self.pending() == 0 {
            return Err(TallyError::Overfull { k: self.k });
        }
        match vote {
            Vote::Yes => self.yes += 1,
            Vote::No => self.no += 1,
            Vote::Abstain => self.abstain += 1,
        }
        Ok(self.status())
    }

    /// Whether enough peers have agreed.
    #[must_use]
    pub const fn is_reached(&self) -> bool {
        self.yes >= self.needed
    }

    /// Whether quorum can no longer be reached.
    ///
    /// `yes + pending < needed`. Both a `No` and an `Abstain` reduce `pending`, so
    /// either can make a claim unreachable - which is deliberate: a cohort that
    /// cannot answer is as much a reason to escalate as one that disagrees.
    #[must_use]
    pub const fn is_unreachable(&self) -> bool {
        self.yes + self.pending() < self.needed
    }

    /// Where the claim stands.
    #[must_use]
    pub const fn status(&self) -> QuorumStatus {
        if self.is_reached() {
            QuorumStatus::Reached
        } else if self.is_unreachable() {
            QuorumStatus::Unreachable
        } else {
            QuorumStatus::Open
        }
    }
}

/// Why a vote could not be recorded.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum TallyError {
    /// More votes than peers.
    #[error("a cohort of {k} cannot cast more than {k} votes")]
    Overfull {
        /// The cohort size.
        k: usize,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The exact rational `ceil(2k/3)`, computed a different way so the
    /// implementation is checked against something other than itself.
    fn exact_quorum(k: usize) -> usize {
        let mut needed = 0;
        while 3 * needed < 2 * k {
            needed += 1;
        }
        needed
    }

    #[test]
    fn quorum_is_rational_at_every_reachable_cohort_size() {
        for k in 1..=PEER_CEILING {
            assert_eq!(quorum(k), exact_quorum(k), "k = {k}");
        }
        assert_eq!(quorum(0), 0);
    }

    #[test]
    fn the_float_spelling_of_quorum_is_wrong_at_three() {
        // The bug this arithmetic exists to avoid, spelled out so it cannot be
        // reintroduced as a "simplification". 0.67 * 3 is 2.01, whose ceiling is 3,
        // which would demand unanimity from a three-peer cohort.
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "reproducing the float trap is the point of this test"
        )]
        let float_answer = (0.67_f64 * 3.0).ceil() as usize;
        assert_eq!(float_answer, 3, "the trap");
        assert_eq!(quorum(3), 2, "the correct threshold");
        assert_ne!(quorum(3), float_answer);
    }

    #[test]
    fn quorum_is_always_a_strict_majority_and_never_unanimity_above_two() {
        for k in 1..=PEER_CEILING {
            let needed = quorum(k);
            assert!(needed * 2 > k, "k = {k}: {needed} is not a majority");
            assert!(needed <= k, "k = {k}: cannot need more peers than exist");
            if k > 2 {
                assert!(needed < k, "k = {k}: unanimity must not be required");
            }
        }
    }

    #[test]
    fn the_tier_table_matches_the_architecture_document() {
        // Every column of the section 4 table, recomputed. The tier ranges are the
        // input; the quorum and byzantine columns are derived, so this checks the
        // document's own arithmetic as well as the code's.
        let expected = [
            (Tier::E0, (1, 1), (1, 1), (0, 0)),
            (Tier::E1, (2, 2), (2, 2), (0, 0)),
            (Tier::E2, (3, 5), (2, 4), (0, 1)),
            (Tier::E3, (7, 12), (5, 8), (2, 3)),
            (Tier::E4, (16, 32), (11, 22), (5, 10)),
            (Tier::E5, (33, 80), (22, 54), (10, 26)),
        ];

        for (tier, k_range, quorum_range, byzantine_range) in expected {
            assert_eq!(tier.k_range(), k_range, "{tier:?} range");

            let (low, high) = k_range;
            let quorums: Vec<usize> = (low..=high).map(quorum).collect();
            let byzantines: Vec<usize> = (low..=high).map(byzantine_tolerance).collect();

            assert_eq!(
                (*quorums.iter().min().unwrap_or(&0), *quorums.iter().max().unwrap_or(&0)),
                quorum_range,
                "{tier:?} quorum"
            );
            assert_eq!(
                (*byzantines.iter().min().unwrap_or(&0), *byzantines.iter().max().unwrap_or(&0)),
                byzantine_range,
                "{tier:?} byzantine"
            );
        }
    }

    #[test]
    fn tiers_are_disjoint_and_ascending() {
        let mut previous_high = 0;
        for tier in Tier::ALL {
            let (low, high) = tier.k_range();
            assert!(low > previous_high, "{tier:?} overlaps the tier below");
            assert!(low <= high, "{tier:?} range is inverted");
            previous_high = high;
        }
        assert_eq!(previous_high, PEER_CEILING, "the top tier must reach the ceiling");
    }

    #[test]
    fn the_gaps_in_the_table_are_reported_as_gaps() {
        // 6, and 13 to 15, are between tiers. Returning None says "no tier selects
        // this" rather than quietly rounding to a neighbour.
        assert_eq!(Tier::containing(6), None);
        for k in 13..=15 {
            assert_eq!(Tier::containing(k), None, "k = {k}");
        }
        assert_eq!(Tier::containing(0), None);
        assert_eq!(Tier::containing(PEER_CEILING + 1), None);

        assert_eq!(Tier::containing(1), Some(Tier::E0));
        assert_eq!(Tier::containing(2), Some(Tier::E1));
        assert_eq!(Tier::containing(5), Some(Tier::E2));
        assert_eq!(Tier::containing(12), Some(Tier::E3));
        assert_eq!(Tier::containing(32), Some(Tier::E4));
        assert_eq!(Tier::containing(PEER_CEILING), Some(Tier::E5));
    }

    #[test]
    fn byzantine_tolerance_only_becomes_meaningful_from_four_peers() {
        assert_eq!(byzantine_tolerance(0), 0);
        for k in 1..=3 {
            assert_eq!(byzantine_tolerance(k), 0, "k = {k}");
        }
        assert_eq!(byzantine_tolerance(4), 1);

        // Which is why the two smallest tiers rely on deterministic gates instead.
        assert!(!Tier::E0.has_byzantine_tolerance());
        assert!(!Tier::E1.has_byzantine_tolerance());
        assert!(!Tier::E2.has_byzantine_tolerance(), "at k=3 it is still zero");
        assert!(Tier::E3.has_byzantine_tolerance());
        assert!(Tier::E4.has_byzantine_tolerance());
    }

    #[test]
    fn only_e0_is_self_consistency() {
        let blind: Vec<Tier> = Tier::ALL.into_iter().filter(|tier| tier.is_blind_rederivation()).collect();
        assert_eq!(blind, vec![Tier::E0]);
    }

    #[test]
    fn sharding_keeps_every_cohort_under_the_rate_limit() {
        assert_eq!(shards_needed(0), 0);
        assert_eq!(shards_needed(1), 1);
        assert_eq!(shards_needed(15), 1);
        assert_eq!(shards_needed(16), 2);
        // The figure the architecture document quotes for E5.
        assert_eq!(shards_needed(80), 6);

        for k in 1..=PEER_CEILING {
            let shards = shards_needed(k);
            let largest = k.div_ceil(shards);
            assert!(largest <= MAX_REQUESTS_PER_MINUTE, "k = {k} puts {largest} on one shard");
        }
    }

    #[test]
    fn a_tally_starts_open_and_reaches_quorum() {
        let mut tally = QuorumTally::new(3);
        assert_eq!(tally.needed(), 2);
        assert_eq!(tally.status(), QuorumStatus::Open);
        assert_eq!(tally.pending(), 3);

        assert_eq!(tally.record(Vote::Yes), Ok(QuorumStatus::Open));
        assert_eq!(tally.record(Vote::Yes), Ok(QuorumStatus::Reached));
        assert!(tally.is_reached());
        assert_eq!(tally.pending(), 1, "the last peer's answer can no longer matter");
    }

    #[test]
    fn unreachable_quorum_is_detected_before_the_last_vote() {
        // The property the turn loop depends on: escalate the moment quorum becomes
        // impossible, not after a 30 second timeout.
        let mut tally = QuorumTally::new(3);
        assert_eq!(tally.record(Vote::No), Ok(QuorumStatus::Open));
        assert_eq!(
            tally.record(Vote::No),
            Ok(QuorumStatus::Unreachable),
            "1 possible yes against a threshold of 2"
        );
        assert!(tally.is_unreachable());
        assert_eq!(tally.pending(), 1, "with a peer still in flight");
    }

    #[test]
    fn abstentions_can_make_a_quorum_unreachable() {
        // Deliberate: a cohort that cannot answer is as much a reason to escalate as
        // one that disagrees, even though no peer refuted anything.
        let mut tally = QuorumTally::new(4);
        assert_eq!(tally.needed(), 3);
        assert_eq!(tally.record(Vote::Abstain), Ok(QuorumStatus::Open));
        assert_eq!(tally.record(Vote::Abstain), Ok(QuorumStatus::Unreachable));
        assert_eq!(tally.no(), 0, "nothing was refuted");
        assert_eq!(tally.abstain(), 2);
    }

    #[test]
    fn a_tally_never_admits_more_votes_than_peers() {
        let mut tally = QuorumTally::new(2);
        assert!(tally.record(Vote::Yes).is_ok());
        assert!(tally.record(Vote::Yes).is_ok());
        assert_eq!(tally.record(Vote::Yes), Err(TallyError::Overfull { k: 2 }));
        assert_eq!(tally.yes(), 2, "the rejected vote was not counted");
    }

    #[test]
    fn every_cohort_size_terminates_in_exactly_one_state() {
        // Exhaustive: for every k and every split of yes/no/abstain across all k
        // peers, the tally must end Reached or Unreachable, never Open. An Open
        // tally with nothing pending would hang the turn.
        for k in 1..=24_usize {
            for yes in 0..=k {
                for no in 0..=(k - yes) {
                    let abstain = k - yes - no;
                    let mut tally = QuorumTally::new(k);
                    for _ in 0..yes {
                        tally.record(Vote::Yes).expect("within k");
                    }
                    for _ in 0..no {
                        tally.record(Vote::No).expect("within k");
                    }
                    for _ in 0..abstain {
                        tally.record(Vote::Abstain).expect("within k");
                    }

                    assert_eq!(tally.pending(), 0);
                    let status = tally.status();
                    assert_ne!(status, QuorumStatus::Open, "k={k} y={yes} n={no} a={abstain}");
                    let expected =
                        if yes >= quorum(k) { QuorumStatus::Reached } else { QuorumStatus::Unreachable };
                    assert_eq!(status, expected, "k={k} y={yes} n={no} a={abstain}");
                }
            }
        }
    }

    #[test]
    fn reached_and_unreachable_are_mutually_exclusive() {
        for k in 1..=PEER_CEILING {
            let mut tally = QuorumTally::new(k);
            for _ in 0..k {
                assert!(
                    !(tally.is_reached() && tally.is_unreachable()),
                    "k = {k} reached both states at once"
                );
                tally.record(Vote::Yes).expect("within k");
            }
            assert!(tally.is_reached());
            assert!(!tally.is_unreachable());
        }
    }

    #[test]
    fn a_verdict_fits_its_budget() {
        let verdict = Verdict::new(
            Vote::Yes,
            Confidence::High,
            "reparse gate accepted the splice and the affected test passes",
            Some("src/prompt/ledger.rs:88".to_owned()),
        )
        .expect("a realistic verdict must fit");

        assert_eq!(verdict.vote(), Vote::Yes);
        assert_eq!(verdict.confidence(), Confidence::High);
        assert_eq!(verdict.evidence_ref(), Some("src/prompt/ledger.rs:88"));
        assert!(verdict.reason().len() <= Verdict::MAX_REASON_BYTES);
    }

    #[test]
    fn an_over_budget_verdict_is_refused_not_truncated() {
        // The bound is the schema, not a suggestion in a prompt. Output is billed in
        // full at every k, so a validator that writes an essay is the one cost the
        // cache cannot help with.
        let long = "x".repeat(Verdict::MAX_REASON_BYTES + 1);
        assert_eq!(
            Verdict::new(Vote::No, Confidence::Low, long, None),
            Err(VerdictError::ReasonTooLong { bytes: Verdict::MAX_REASON_BYTES + 1 })
        );

        let reference = "y".repeat(Verdict::MAX_EVIDENCE_BYTES + 1);
        assert_eq!(
            Verdict::new(Vote::Yes, Confidence::Low, "ok", Some(reference)),
            Err(VerdictError::EvidenceRefTooLong { bytes: Verdict::MAX_EVIDENCE_BYTES + 1 })
        );
    }

    #[test]
    fn a_stored_verdict_is_rebudgeted_on_load() {
        let verdict = Verdict::new(Vote::Abstain, Confidence::Medium, "unclear", None).expect("valid");
        let json = serde_json::to_string(&verdict).expect("serialise");
        assert_eq!(serde_json::from_str::<Verdict>(&json).expect("deserialise"), verdict);

        let long = "z".repeat(Verdict::MAX_REASON_BYTES + 1);
        let forged = format!(r#"{{"vote":"Yes","confidence":"High","reason":"{long}","evidence_ref":null}}"#);
        let error = serde_json::from_str::<Verdict>(&forged).expect_err("over budget");
        assert!(error.to_string().contains("verdict reason may hold"), "got: {error}");
    }

    #[test]
    fn the_peer_limits_are_the_configured_and_absolute_ones() {
        assert_eq!(PEER_CEILING, 80);
        assert_eq!(DEFAULT_PEER_LIMIT, 16);
        assert_eq!(MAX_REQUESTS_PER_MINUTE, 15);
        // That the default sits inside the ceiling is asserted at compile time; see
        // the const block near the top of this module.
    }
}
