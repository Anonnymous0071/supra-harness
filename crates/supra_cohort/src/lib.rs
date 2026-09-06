//! Deterministic cohort tier estimation for supra-harness.
//!
//! **T15.5** of the stage sequence: turn step 2 is
//! `cohort.signals -> score -> tier -> k, quorum, shards (0 LLM calls)`.
//!
//! # What this stage is for
//!
//! Cohort size is a function of evidence, never a constant. This crate reads
//! the evidence - digest signals (blast radius, churn, anchor count), the tool
//! registry (requested reversibility class), findings, and past task profiles -
//! and produces the tier, then the admission (tier under the peer limit), then
//! the numbers the turn loop needs (k, quorum, shards). Pure and total: the
//! same signals always yield the same tier, on every machine, on every run.
//!
//! # Shape
//!
//! | Module | Owns |
//! |---|
//! | [`signals`] | the evidence, as bands and counts - never prose, never floats |
//! | [`score`] | the ladder: bands to tier, composing by maximum |
//! | [`admit`] | the decision: tier under the limit, k, quorum, shards, escalation |
//! | [`profile`] | what past tasks taught: shape keys, saturating failures |
//!
//! # Zero LLM calls, structurally
//!
//! This crate does not depend on `supra_llm`, and a negative test below
//! enforces the direction: estimation reads integers and returns a tier, and
//! anything needing a model lives in T23, not here.
//!
//! # The arithmetic lives in T6
//!
//! `Tier`, `admit`, `quorum`, `byzantine_tolerance`, `shards_needed`, and
//! `QuorumTally` are `supra_types::cohort` - tested there against the document's
//! table, including the gaps. This crate owns only what T6 does not: scoring
//! evidence into a tier. Reimplementing the arithmetic here would give two
//! implementations that can disagree; reusing it gives one.
//!
//! # Usage
//!
//! ```no_run
//! use supra_cohort::{Admission, AreaFlags, Signals, estimate};
//!
//! let signals = Signals::minimal();
//! let tier = estimate(&signals, &AreaFlags::none());
//! let admission = Admission::decide(tier, 16).expect("a non-zero limit admits");
//! assert_eq!(admission.k, 1);
//! # Ok::<(), std::convert::Infallible>(())
//! ```

#![deny(missing_docs)]
#![forbid(unsafe_code)]
// Tests assert with `.expect()` and `panic!`. Scoped to `cfg(test)` so no allow reaches a
// shipped path.
#![cfg_attr(test, allow(clippy::expect_used, clippy::panic, clippy::print_stderr))]

pub mod admit;
pub mod profile;
pub mod score;
pub mod signals;

pub use admit::{Admission, Escalation};
pub use profile::{TaskProfile, TaskShape};
pub use score::{AreaFlags, estimate};
pub use signals::{AnchorBand, BlastBand, ChurnBand, Signals};

/// Re-exported so callers read one crate: the tier type and the admission
/// arithmetic live in T6, and every consumer of this crate needs them beside
/// the estimation this crate adds.
pub use supra_types::{Tier, admit, byzantine_tolerance, quorum, shards_needed};

/// The cohort is estimated once per task and read from the turn loop while
/// votes arrive, so this is a requirement rather than an observation.
const _: () = {
    const fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<Signals>();
    assert_send_sync::<Admission>();
};

#[cfg(test)]
mod tests {
    /// Estimation declares no dependency on the provider crate. A probe adding
    /// one must fail this test - and the manifest check in
    /// `scripts/check-invariants.sh` must fail the build first, so this test
    /// is the second net, not the first.
    #[test]
    fn estimation_needs_no_provider() {
        let manifest = include_str!("../Cargo.toml");
        assert!(
            !manifest.contains("supra_llm"),
            "supra_cohort must not depend on supra_llm: estimation is zero LLM calls"
        );
    }
}
