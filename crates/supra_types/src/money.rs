//! Money, in integers.
//!
//! Cost is tracked in micro-dollars and never in floating point. Two reasons, and
//! the second is the one that matters here.
//!
//! Accumulating a session's spend by adding `f64` values loses cents in a long
//! session, which is the ordinary argument against floats for money. But this crate
//! also has a stricter obligation: invariant I7 requires byte-stable serialisation,
//! because unstable output is a documented cache breaker. A float's text form is the
//! one primitive whose stability across platforms and libraries is not obvious, so
//! the contract layer has no float fields at all - not in [`MicroUsd`], not in
//! [`crate::Confidence`], not in the cache multipliers. Removing floats here removes
//! the question from every stage above.

use core::fmt;
use core::iter::Sum;
use core::ops::{Add, AddAssign, Sub};

use serde::{Deserialize, Serialize};

/// An amount in millionths of a US dollar.
///
/// Signed, because a reconciliation delta can go either way: a live estimate that
/// overshot produces a negative correction, and clamping that at zero would hide the
/// drift that [`crate::Event::UsageDrift`] exists to report.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct MicroUsd(i64);

impl MicroUsd {
    /// Nothing.
    pub const ZERO: Self = Self(0);

    /// Micro-dollars in one dollar.
    pub const PER_DOLLAR: i64 = 1_000_000;

    /// From a raw micro-dollar count.
    #[must_use]
    pub const fn from_micros(micros: i64) -> Self {
        Self(micros)
    }

    /// The raw micro-dollar count.
    #[must_use]
    pub const fn micros(self) -> i64 {
        self.0
    }

    /// Cost of `tokens` at `per_mtok_micros` per million tokens, scaled by a
    /// percentage multiplier.
    ///
    /// One function rather than three, because the three token classes differ only
    /// in their multiplier: 10 for a cache read, 100 for uncached input, 125 or 200
    /// for a cache write. Rounding is toward zero, which under-reports by less than
    /// a micro-dollar per call.
    #[must_use]
    pub const fn for_tokens(tokens: u32, per_mtok_micros: i64, multiplier_percent: u32) -> Self {
        let tokens = tokens as i64;
        let multiplier = multiplier_percent as i64;
        // tokens * rate / 1e6 * percent / 100, ordered so the division happens last
        // and a small amount does not round to nothing.
        Self(tokens * per_mtok_micros * multiplier / (Self::PER_DOLLAR * 100))
    }

    /// Percentage by which `self` differs from `reference`, rounded toward zero.
    ///
    /// Returns 0 when `reference` is zero: an estimate of nothing that turned out to
    /// be nothing has not drifted. Above 10%, the turn loop emits
    /// [`crate::Event::UsageDrift`], because the local token counter needs
    /// calibration and that is worth knowing rather than absorbing.
    ///
    /// Computed through `abs_diff` and `unsigned_abs` rather than `abs`, so it cannot
    /// panic on `i64::MIN` under the overflow checks this workspace enables in every
    /// profile, and saturates rather than truncating on a ratio no real bill reaches.
    #[must_use]
    pub fn drift_percent_from(self, reference: Self) -> u32 {
        let magnitude = reference.0.unsigned_abs();
        if magnitude == 0 {
            return 0;
        }
        let percent = self.0.abs_diff(reference.0).saturating_mul(100) / magnitude;
        u32::try_from(percent).unwrap_or(u32::MAX)
    }
}

impl Add for MicroUsd {
    type Output = Self;

    fn add(self, rhs: Self) -> Self {
        Self(self.0 + rhs.0)
    }
}

impl AddAssign for MicroUsd {
    fn add_assign(&mut self, rhs: Self) {
        self.0 += rhs.0;
    }
}

impl Sub for MicroUsd {
    type Output = Self;

    fn sub(self, rhs: Self) -> Self {
        Self(self.0 - rhs.0)
    }
}

impl Sum for MicroUsd {
    fn sum<I: Iterator<Item = Self>>(iter: I) -> Self {
        iter.fold(Self::ZERO, Add::add)
    }
}

impl fmt::Display for MicroUsd {
    /// Three decimal places, as the status line shows it: `$0.014`.
    ///
    /// Three because a turn's delta is measured in thousandths of a dollar, so two
    /// would round most turns to `$0.01` and flatten exactly the differences a user
    /// is watching for.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let sign = if self.0 < 0 { "-" } else { "" };
        let millis = self.0.abs() / 1_000;
        write!(f, "{sign}${}.{:03}", millis / 1_000, millis % 1_000)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cache::{CacheTtl, READ_MULTIPLIER_PERCENT, UNCACHED_MULTIPLIER_PERCENT};

    #[test]
    fn displays_as_the_status_line_shows_it() {
        assert_eq!(MicroUsd::from_micros(14_000).to_string(), "$0.014");
        assert_eq!(MicroUsd::ZERO.to_string(), "$0.000");
        assert_eq!(MicroUsd::from_micros(1_030_000).to_string(), "$1.030");
        assert_eq!(MicroUsd::from_micros(-14_000).to_string(), "-$0.014");
        assert_eq!(MicroUsd::from_micros(6_300_000).to_string(), "$6.300");
    }

    #[test]
    fn sub_micro_amounts_round_toward_zero_rather_than_up() {
        assert_eq!(MicroUsd::from_micros(999).to_string(), "$0.000");
        assert_eq!(MicroUsd::from_micros(1_000).to_string(), "$0.001");
    }

    #[test]
    fn accumulation_loses_nothing() {
        // A hundred turns of an awkward amount. In f64 this drifts; in integers it
        // cannot.
        let per_turn = MicroUsd::from_micros(12_345);
        let total: MicroUsd = core::iter::repeat_n(per_turn, 100).sum();
        assert_eq!(total.micros(), 1_234_500);
        assert_eq!(total.to_string(), "$1.234");

        let mut running = MicroUsd::ZERO;
        for _ in 0..100 {
            running += per_turn;
        }
        assert_eq!(running, total, "+= and sum must agree exactly");
    }

    #[test]
    fn token_pricing_reproduces_the_multiplier_table() {
        // $3 per million input tokens, the figure the budget section uses.
        let rate = 3_000_000_i64;

        let uncached = MicroUsd::for_tokens(21_000, rate, UNCACHED_MULTIPLIER_PERCENT);
        assert_eq!(uncached.micros(), 63_000, "21k uncached at $3/MTok is $0.063");

        let read = MicroUsd::for_tokens(21_000, rate, READ_MULTIPLIER_PERCENT);
        assert_eq!(read.micros(), 6_300, "the same tokens cached are a tenth");
        assert_eq!(uncached.micros() / read.micros(), 10);

        let write = MicroUsd::for_tokens(5_400, rate, CacheTtl::OneHour.write_multiplier_percent());
        assert_eq!(write.micros(), 32_400, "5.4k at 200% is $0.0324");
    }

    #[test]
    fn the_hundred_turn_baseline_matches_the_budget_table() {
        // Section 10: a cache-unaware baseline sends ~21k uncached per turn and
        // spends $6.30 over 100 turns at $3/MTok. Reproduced so the headline figure
        // in the document is arithmetic rather than assertion.
        let rate = 3_000_000_i64;
        let per_turn = MicroUsd::for_tokens(21_000, rate, UNCACHED_MULTIPLIER_PERCENT);
        let total: MicroUsd = core::iter::repeat_n(per_turn, 100).sum();
        assert_eq!(total.to_string(), "$6.300");
    }

    #[test]
    fn drift_is_reported_against_the_reconciled_figure() {
        let estimate = MicroUsd::from_micros(14_000);
        let actual = MicroUsd::from_micros(20_000);
        assert_eq!(estimate.drift_percent_from(actual), 30);
        assert_eq!(actual.drift_percent_from(estimate), 42);

        assert_eq!(estimate.drift_percent_from(estimate), 0);
        assert_eq!(MicroUsd::ZERO.drift_percent_from(MicroUsd::ZERO), 0);

        // The 10% threshold the architecture document sets.
        let within = MicroUsd::from_micros(10_500);
        assert!(within.drift_percent_from(MicroUsd::from_micros(10_000)) <= 10);
        let beyond = MicroUsd::from_micros(11_500);
        assert!(beyond.drift_percent_from(MicroUsd::from_micros(10_000)) > 10);
    }

    #[test]
    fn arithmetic_handles_a_negative_correction() {
        let estimated = MicroUsd::from_micros(20_000);
        let actual = MicroUsd::from_micros(18_000);
        assert_eq!(actual - estimated, MicroUsd::from_micros(-2_000));
        assert!(actual - estimated < MicroUsd::ZERO);
    }
}
