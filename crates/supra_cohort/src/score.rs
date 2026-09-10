//! Evidence to tier: a deterministic ladder over signal bands, zero LLM calls.
//!
//! # Why a ladder and not a score
//!
//! A weighted sum would need weights, and weights would need tuning, and tuning
//! would need a dataset nobody has: the "right" cohort size for a task is not
//! measured anywhere, because T30's tier-accuracy metric does not exist yet.
//! A ladder - ordered rules, first match wins - needs no weights, only an order,
//! and the order is argued per rule below rather than fitted. When the metric
//! lands, the ladder's mistakes become visible per rule, which is exactly the
//! shape that lets data improve it without replacing it.
//!
//! # The ladder, strongest tier first
//!
//! Rules are evaluated top-down; the first matching rule sets a floor, and
//! stronger floors win over weaker ones. Every rule names the document trigger
//! it implements:
//!
//! 1. **E5**: user-requested (`force_full`), or two or more consecutive failures
//!    (`repeat_failures >= 2`) - repeated-failure escalation, and the failures
//!    saturate rather than accumulate because the third failure teaches nothing
//!    the second did not.
//! 2. **E4**: auth, crypto, migrations (`sensitive_area`), or any active finding
//!    (`active_findings >= 1`) - a confirmed finding raises the tier, and one is
//!    enough because a finding is already a confirmed defect, not a suspicion.
//! 3. **E3**: wide blast (`Wide`, above 10 dependents) or hot churn (`Hot`) -
//!    cross-module, high churn, the document's words.
//! 4. **E2**: contained blast (`Contained`), several anchors (`Many`), active
//!    churn (`Active`), or an irreversible request (`R3`) - multi-file,
//!    blast radius within ten.
//! 5. **E1**: any side effect at all (`R1` and above), or a few anchors - a
//!    single-file edit with something to verify.
//! 6. **E0**: everything else - Q&A, a single-file read, blast radius zero.
//!
//! # Why the floor composes by max, not by first match
//!
//! A task can trip several rules at once (wide blast *and* an active finding).
//! First-match-wins would make the outcome depend on rule order for overlapping
//! evidence, and rule order is the thing most likely to be edited casually.
//! Taking the maximum over all matching rules makes the outcome order-independent:
//! reordering rules changes nothing, and each rule can be reasoned about alone.
//!
//! # Determinism is the contract
//!
//! Same signals, same tier, on every machine, on every run. No floats (T6's
//! rule), no hash-map iteration, no time, no randomness. The turn loop calls
//! this once per task and persists the answer with the task profile - which is
//! what makes T30's tier-accuracy metric meaningful: the metric compares a
//! recorded decision against an outcome, and a decision that wobbles cannot be
//! measured.

use supra_types::{Reversibility, Tier};

use crate::signals::{AnchorBand, BlastBand, ChurnBand, Signals};

/// Whether the task touches an area that always earns E4 scrutiny.
///
/// Auth, crypto, and migrations: the document's words. Matched against the
/// task's file paths by substring, because paths are the only task evidence
/// that names areas - and matched narrowly, because a broad match would
/// escalate every security-adjacent task on a substring.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct AreaFlags {
    /// The task touches auth, crypto, or migration paths.
    pub sensitive_area: bool,
    /// The user explicitly requested full scrutiny.
    pub force_full: bool,
}

impl AreaFlags {
    /// No area signal.
    #[must_use]
    pub const fn none() -> Self {
        Self { sensitive_area: false, force_full: false }
    }

    /// Derive from task file paths. Case-insensitive, because filesystems disagree
    /// about case and an `Auth/` directory is auth whatever its capitalisation.
    /// Matched on path components rather than the raw substring, so
    /// `src/author.rs` is not auth.
    #[must_use]
    pub fn of_paths(paths: &[&str]) -> Self {
        let sensitive = paths.iter().any(|path| {
            path.split(['/', '\\']).filter(|component| !component.is_empty()).any(|component| {
                let lower = component.to_lowercase();
                let stem = lower.strip_suffix(".rs").or_else(|| lower.strip_suffix(".ts")).unwrap_or(&lower);
                let stem = stem.strip_suffix(".py").unwrap_or(stem);
                stem == "auth" || stem == "crypto" || stem.starts_with("migrat")
            })
        });
        Self { sensitive_area: sensitive, force_full: false }
    }
}

/// Estimate the tier for a task from its signals.
///
/// Pure and total: every input combination yields exactly one tier, and the same
/// inputs always yield the same tier. See the module documentation for the
/// ladder and the reason it composes by maximum rather than first match.
///
/// # Why `u32` thresholds and not `usize`
///
/// Signal counts arrive as `u32` (churn) and `usize` (findings); the ladder
/// compares both against small constants. Comparing in one width avoids a cast
/// at every rule - and `u32` is the narrower, so the `usize` counts convert
/// with saturation rather than truncation.
#[must_use]
pub fn estimate(signals: &Signals, areas: &AreaFlags) -> Tier {
    let mut tier = Tier::E0;

    // E5: user-requested, or repeated-failure escalation.
    if areas.force_full || signals.repeat_failures >= 2 {
        tier = tier.max(Tier::E5);
    }
    // E4: sensitive areas, or any active finding.
    if areas.sensitive_area || signals.active_findings > 0 {
        tier = tier.max(Tier::E4);
    }
    // E3: wide blast or hot churn.
    if signals.blast == BlastBand::Wide || signals.churn == ChurnBand::Hot {
        tier = tier.max(Tier::E3);
    }
    // E2: contained blast, many anchors, active churn, or irreversible request.
    if signals.blast == BlastBand::Contained
        || signals.anchors == AnchorBand::Many
        || signals.churn == ChurnBand::Active
        || signals.reversibility == Reversibility::R3
    {
        tier = tier.max(Tier::E2);
    }
    // E1: any side effect, or a few anchors.
    if signals.reversibility >= Reversibility::R1 || signals.anchors == AnchorBand::Few {
        tier = tier.max(Tier::E1);
    }
    tier
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::signals::Signals;

    fn signals() -> Signals {
        Signals::minimal()
    }

    fn areas() -> AreaFlags {
        AreaFlags::none()
    }

    #[test]
    fn minimal_evidence_is_e0() {
        assert_eq!(estimate(&signals(), &areas()), Tier::E0);
    }

    #[test]
    fn a_read_with_anchors_is_still_e1_not_e2() {
        // A few anchors means something to verify, not something sprawling.
        let mut evidence = signals();
        evidence.anchors = AnchorBand::Few;
        assert_eq!(estimate(&evidence, &areas()), Tier::E1);
    }

    #[test]
    fn contained_blast_is_e2() {
        let mut evidence = signals();
        evidence.blast = BlastBand::Contained;
        assert_eq!(estimate(&evidence, &areas()), Tier::E2);
    }

    #[test]
    fn wide_blast_is_e3_even_with_nothing_else() {
        let mut evidence = signals();
        evidence.blast = BlastBand::Wide;
        assert_eq!(estimate(&evidence, &areas()), Tier::E3);
    }

    #[test]
    fn hot_churn_alone_is_e3() {
        // The M4 gap: wide blast had a test, hot churn did not - so deleting the
        // churn half of the E3 rule changed nothing observable. Same evidence
        // shape, different signal: heat without width still earns E3.
        let mut evidence = signals();
        evidence.churn = ChurnBand::Hot;
        assert_eq!(estimate(&evidence, &areas()), Tier::E3);
    }

    #[test]
    fn an_irreversible_request_is_e2_even_when_isolated() {
        // The M5 gap: R3 sat in a four-way OR with three covered signals, so
        // deleting it changed nothing any test observed. Minimal evidence -
        // isolated, calm, anchorless - plus R3 alone must clear E2.
        let mut evidence = signals();
        evidence.reversibility = Reversibility::R3;
        assert_eq!(estimate(&evidence, &areas()), Tier::E2);
    }

    #[test]
    fn one_active_finding_is_e4() {
        let mut evidence = signals();
        evidence.active_findings = 1;
        assert_eq!(estimate(&evidence, &areas()), Tier::E4);
    }

    #[test]
    fn sensitive_paths_are_e4() {
        let areas = AreaFlags::of_paths(&["src/auth/session.rs"]);
        assert_eq!(estimate(&signals(), &areas), Tier::E4);
        let areas = AreaFlags::of_paths(&["db/migrations/012.rs"]);
        assert_eq!(estimate(&signals(), &areas), Tier::E4);
        let areas = AreaFlags::of_paths(&["src/turns.rs"]);
        assert_eq!(estimate(&signals(), &areas), Tier::E0);
    }

    #[test]
    fn two_failures_escalate_to_e5_and_saturate() {
        let mut evidence = signals();
        evidence.repeat_failures = 1;
        assert_eq!(estimate(&evidence, &areas()), Tier::E0, "one failure teaches patience");
        evidence.repeat_failures = 2;
        assert_eq!(estimate(&evidence, &areas()), Tier::E5);
        evidence.repeat_failures = u32::MAX;
        assert_eq!(estimate(&evidence, &areas()), Tier::E5, "saturation, not accumulation");
    }

    #[test]
    fn overlapping_evidence_takes_the_maximum() {
        // Wide blast (E3) plus an active finding (E4): the finding wins regardless
        // of rule order, because the composition is a max, not first match.
        let mut evidence = signals();
        evidence.blast = BlastBand::Wide;
        evidence.active_findings = 3;
        assert_eq!(estimate(&evidence, &areas()), Tier::E4);
    }

    #[test]
    fn force_full_beats_everything() {
        let mut evidence = signals();
        evidence.blast = BlastBand::Wide;
        evidence.active_findings = 5;
        let areas = AreaFlags { sensitive_area: false, force_full: true };
        assert_eq!(estimate(&evidence, &areas), Tier::E5);
    }

    #[test]
    fn area_matching_is_case_insensitive_and_narrow() {
        assert!(AreaFlags::of_paths(&["src/Auth/mod.rs"]).sensitive_area);
        assert!(AreaFlags::of_paths(&["src/CRYPTO/x.rs"]).sensitive_area);
        // A component that merely contains the letters must not escalate:
        // only whole auth/crypto/migration component names do.
        assert!(!AreaFlags::of_paths(&["src/author.rs"]).sensitive_area);
        assert!(!AreaFlags::of_paths(&["src/authority.rs"]).sensitive_area);
        assert!(!AreaFlags::of_paths(&["src/description.rs"]).sensitive_area);
        assert!(!AreaFlags::of_paths(&["src/secret.rs"]).sensitive_area);
    }
}
