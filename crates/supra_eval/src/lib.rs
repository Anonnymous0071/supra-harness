//! Economy gate. **T30** of the stage sequence.
//!
//! Section 10's figures are **derived, not measured** - this crate validates
//! them. Two modes: offline shape-check (no network, always runs in CI) and
//! live probe (needs credentials, measures tier-accuracy, USD/turn, cache hit
//! rate against the pricing in §10).

#![deny(missing_docs)]
#![forbid(unsafe_code)]
#![cfg_attr(test, allow(clippy::expect_used, clippy::panic))]

use serde::{Deserialize, Serialize};

/// One measured turn, as observed by the live probe.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TurnMeasurement {
    /// Tier the estimator predicted.
    pub predicted: String,
    /// Tier the admission actually ran.
    pub admitted: String,
    /// Input tokens (estimate until reconciled).
    pub input_tokens: u32,
    /// Output tokens (estimate until reconciled).
    pub output_tokens: u32,
    /// Whether cache was hit.
    pub cache_hit: bool,
    /// Cost in mills (thousandths of a dollar).
    pub cost_mills: u64,
}

/// Aggregate economy report for a session.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EconomyReport {
    /// Fraction of turns where predicted tier equals admitted tier.
    pub tier_accuracy: f64,
    /// Mean cost per turn in mills.
    pub mean_mills_per_turn: f64,
    /// Cache hit rate over the session.
    pub cache_hit_rate: f64,
    /// Number of turns measured.
    pub turns: usize,
}

impl EconomyReport {
    /// Build from individual measurements.
    #[must_use]
    #[allow(clippy::cast_precision_loss)]
    pub fn from_measurements(measurements: &[TurnMeasurement]) -> Self {
        if measurements.is_empty() {
            return Self { tier_accuracy: 1.0, mean_mills_per_turn: 0.0, cache_hit_rate: 0.0, turns: 0 };
        }
        let total = measurements.len() as f64;
        let hits = measurements.iter().filter(|m| m.predicted == m.admitted).count() as f64;
        let cache_hits = measurements.iter().filter(|m| m.cache_hit).count() as f64;
        let total_mills: u64 = measurements.iter().map(|m| m.cost_mills).sum();
        Self {
            tier_accuracy: hits / total,
            mean_mills_per_turn: total_mills as f64 / total,
            cache_hit_rate: cache_hits / total,
            turns: measurements.len(),
        }
    }

    /// Whether the report passes the gate: accuracy and cost within budget.
    #[must_use]
    pub fn passes(&self, min_accuracy: f64, max_mills_per_turn: f64) -> bool {
        self.tier_accuracy >= min_accuracy && self.mean_mills_per_turn <= max_mills_per_turn
    }

    /// Human-readable one-line summary.
    #[must_use]
    pub fn summary(&self) -> String {
        format!(
            "tier-accuracy {:.1}%, mean ${:.3}/turn, cache-hit {:.1}% over {} turns",
            self.tier_accuracy * 100.0,
            self.mean_mills_per_turn / 1000.0,
            self.cache_hit_rate * 100.0,
            self.turns
        )
    }
}

/// Offline shape-check: verify that §10's tier table admits correctly without
/// any network call.
#[must_use]
pub fn offline_shape_check() -> bool {
    use supra_types::{Tier, admit};
    let tiers = [Tier::E0, Tier::E1, Tier::E2, Tier::E3, Tier::E4, Tier::E5];
    for tier in tiers {
        let (lo, hi) = tier.k_range();
        for k in lo..=hi {
            if Tier::containing(k) != Some(tier) {
                return false;
            }
        }
        for limit in 1..=80usize {
            if let Some((admitted_tier, k)) = admit(tier, limit) {
                if Tier::containing(k) != Some(admitted_tier) {
                    return false;
                }
            }
        }
    }
    true
}

/// Pricing assumption from §1: cache read costs 0.1x base input.
pub const CACHE_READ_MULTIPLIER: f64 = 0.1;

/// Base input price in mills per megatoken ($3/MTok).
pub const BASE_MILLS_PER_MTOK: f64 = 3000.0;

/// Estimate cost in mills for a turn: cached input at 0.1x, uncached at 1x,
/// output at full price (never cached).
#[must_use]
#[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation, clippy::cast_sign_loss)]
pub fn estimate_mills(input_tokens: u32, cached_tokens: u32, output_tokens: u32) -> u64 {
    let cached = f64::from(cached_tokens) * CACHE_READ_MULTIPLIER;
    let uncached = f64::from(input_tokens.saturating_sub(cached_tokens));
    let total_input_mtok = (cached + uncached) / 1_000_000.0;
    let output_mtok = f64::from(output_tokens) / 1_000_000.0;
    ((total_input_mtok + output_mtok) * BASE_MILLS_PER_MTOK).round() as u64
}

/// Build a [`TurnMeasurement`] from a live [`supra_llm::Completion`].
#[must_use]
#[allow(clippy::cast_possible_truncation)]
pub fn measure(
    predicted: &str,
    admitted: &str,
    completion: &supra_llm::Completion,
    input_tokens: u32,
) -> TurnMeasurement {
    let (output_tokens, cached_tokens) = match &completion.usage {
        Some(usage) => (
            u32::try_from(usage.output_tokens).unwrap_or(u32::MAX),
            u32::try_from(usage.cached_tokens).unwrap_or(u32::MAX),
        ),
        None => (0, 0),
    };
    let cost_mills = estimate_mills(input_tokens, cached_tokens, output_tokens);
    TurnMeasurement {
        predicted: predicted.to_owned(),
        admitted: admitted.to_owned(),
        input_tokens,
        output_tokens,
        cache_hit: cached_tokens > 0,
        cost_mills,
    }
}
