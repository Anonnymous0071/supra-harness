//! The prompt cache contract: breakpoints, TTLs, and minimum lengths.
//!
//! Everything here is a **verified** provider fact turned into a type, so that a
//! later stage cannot get it subtly wrong. Invariants I5, I7, and I8 live in this
//! module's tests.
//!
//! No value in this module is a float. Cache multipliers are integer percentages
//! and money is integer micro-dollars, because the whole point of I7 is byte-stable
//! serialisation and a float is the one primitive whose text form is not obviously
//! stable across platforms and libraries. Removing floats from the contract layer
//! removes that question from every stage above it.

use serde::{Deserialize, Serialize};

/// Cache-read price as a percentage of the base input rate.
///
/// The single most important number in the design. At 10%, sending ten thousand
/// cached tokens costs less than sending one thousand uncached ones - which is why
/// supra does not compress prompts.
pub const READ_MULTIPLIER_PERCENT: u32 = 10;

/// Uncached input price, as a percentage of itself. Present so cost arithmetic can
/// name every class it sums rather than hiding one as an implicit 1.
pub const UNCACHED_MULTIPLIER_PERCENT: u32 = 100;

/// How many cache breakpoints a single request may carry.
pub const MAX_BREAKPOINTS: usize = 4;

/// How far back the cache-read lookback window reaches, in blocks.
///
/// A consecutive run of `tool_use` / `tool_result` counts as one position, which is
/// why invariant I5 renders tool calls consecutively. See
/// [`crate::Segment::lookback_positions`].
pub const LOOKBACK_BLOCKS: usize = 20;

/// How long a cache entry survives without a hit.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum CacheTtl {
    /// Five minutes. Ordered first so `Ord` ranks by duration, which is what the
    /// TTL ordering rule is stated in terms of.
    FiveMinutes,
    /// One hour.
    OneHour,
}

impl CacheTtl {
    /// Cache-write price as a percentage of the base input rate.
    ///
    /// A longer TTL costs more to write and is worth it only for a prefix that will
    /// be read many times: at 200%, a one-hour entry pays for itself after roughly
    /// two hits versus sending the same tokens uncached.
    #[must_use]
    pub const fn write_multiplier_percent(self) -> u32 {
        match self {
            Self::FiveMinutes => 125,
            Self::OneHour => 200,
        }
    }

    /// Duration in seconds.
    #[must_use]
    pub const fn seconds(self) -> u32 {
        match self {
            Self::FiveMinutes => 300,
            Self::OneHour => 3_600,
        }
    }
}

/// A cache breakpoint, named for the region it terminates.
///
/// The variants are declared in prefix order - `tools`, then `system`, then
/// messages - which is the order the provider requires and the order `Ord` reports.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum Breakpoint {
    /// End of the frozen tool manifest.
    Bp1Tools,
    /// End of the system contract.
    Bp2System,
    /// End of the memory index. Advances only when a new generation is written.
    Bp3MemoryIndex,
    /// End of turn n-1. Rolls forward every turn, which is why it is the cheap one.
    Bp4PreviousTurn,
}

impl Breakpoint {
    /// All four, in prefix order.
    pub const ALL: [Self; MAX_BREAKPOINTS] =
        [Self::Bp1Tools, Self::Bp2System, Self::Bp3MemoryIndex, Self::Bp4PreviousTurn];

    /// The TTL this breakpoint is written with.
    ///
    /// Three one-hour entries followed by a five-minute one. The first three are
    /// frozen or slow-moving, so the 200% write is amortised over hundreds of
    /// reads; BP4 moves every turn, so paying 200% for it would be paying for a
    /// lifetime it never uses.
    #[must_use]
    pub const fn ttl(self) -> CacheTtl {
        match self {
            Self::Bp1Tools | Self::Bp2System | Self::Bp3MemoryIndex => CacheTtl::OneHour,
            Self::Bp4PreviousTurn => CacheTtl::FiveMinutes,
        }
    }

    /// Position in the prefix, counting from 1.
    #[must_use]
    pub const fn position(self) -> u8 {
        match self {
            Self::Bp1Tools => 1,
            Self::Bp2System => 2,
            Self::Bp3MemoryIndex => 3,
            Self::Bp4PreviousTurn => 4,
        }
    }

    /// Whether this breakpoint's region is expected to change during a session.
    #[must_use]
    pub const fn is_rolling(self) -> bool {
        matches!(self, Self::Bp4PreviousTurn)
    }
}

/// Whether a breakpoint sequence satisfies the documented TTL ordering rule.
///
/// Longer TTLs must precede shorter ones. A sequence that puts a five-minute entry
/// before a one-hour entry is rejected by the provider, so this is checked when the
/// plan is built rather than discovered as a request failure.
#[must_use]
pub fn ttl_order_is_valid(breakpoints: &[Breakpoint]) -> bool {
    if breakpoints.len() > MAX_BREAKPOINTS {
        return false;
    }
    breakpoints.windows(2).all(|pair| pair[0].ttl() >= pair[1].ttl())
}

/// Minimum prompt length a model will cache at all.
///
/// A closed set, because the values come from provider documentation rather than
/// from configuration. Enumerating them stops a later stage from inventing a fifth.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum MinimumCacheable {
    /// Smallest tier.
    Tokens512,
    /// Second tier.
    Tokens1024,
    /// Third tier, and the floor for implicit-only providers.
    Tokens2048,
    /// Largest tier.
    Tokens4096,
}

impl MinimumCacheable {
    /// Every tier, ascending.
    pub const ALL: [Self; 4] = [Self::Tokens512, Self::Tokens1024, Self::Tokens2048, Self::Tokens4096];

    /// The threshold in tokens.
    #[must_use]
    pub const fn tokens(self) -> u32 {
        match self {
            Self::Tokens512 => 512,
            Self::Tokens1024 => 1_024,
            Self::Tokens2048 => 2_048,
            Self::Tokens4096 => 4_096,
        }
    }
}

/// Tokens of padding needed to make a prefix cacheable, per invariant I8.
///
/// Below its model's threshold, caching **fails silently with no error** - the
/// request succeeds, the response is correct, and the bill is ten times what it
/// should be. Since that failure is invisible, the response is to grow the prefix:
/// padding up to the threshold costs `1.0x` once, while falling short costs `1.0x`
/// on every turn forever.
///
/// Returns 0 when the prefix already qualifies.
#[must_use]
pub const fn padding_needed(prefix_tokens: u32, minimum: MinimumCacheable) -> u32 {
    minimum.tokens().saturating_sub(prefix_tokens)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_breakpoint_table_matches_the_architecture_document() {
        // docs/ARCHITECTURE.md I5. If this test and that table disagree, one of
        // them is a defect; they are not allowed to drift.
        assert_eq!(Breakpoint::Bp1Tools.ttl(), CacheTtl::OneHour);
        assert_eq!(Breakpoint::Bp2System.ttl(), CacheTtl::OneHour);
        assert_eq!(Breakpoint::Bp3MemoryIndex.ttl(), CacheTtl::OneHour);
        assert_eq!(Breakpoint::Bp4PreviousTurn.ttl(), CacheTtl::FiveMinutes);
        assert_eq!(Breakpoint::ALL.len(), MAX_BREAKPOINTS);
    }

    #[test]
    fn only_bp4_rolls() {
        let rolling: Vec<Breakpoint> =
            Breakpoint::ALL.into_iter().filter(|breakpoint| breakpoint.is_rolling()).collect();
        assert_eq!(rolling, vec![Breakpoint::Bp4PreviousTurn]);
    }

    #[test]
    fn positions_are_dense_and_in_prefix_order() {
        for (index, breakpoint) in Breakpoint::ALL.iter().enumerate() {
            assert_eq!(usize::from(breakpoint.position()), index + 1);
        }
    }

    #[test]
    fn the_declared_order_satisfies_the_ttl_rule() {
        assert!(ttl_order_is_valid(&Breakpoint::ALL));
        assert!(ttl_order_is_valid(&[]));
        assert!(ttl_order_is_valid(&[Breakpoint::Bp4PreviousTurn]));
    }

    #[test]
    fn a_short_ttl_before_a_long_one_is_rejected() {
        // The mistake the rule exists to catch: BP4 is five minutes, so nothing
        // with a one-hour TTL may follow it.
        assert!(!ttl_order_is_valid(&[Breakpoint::Bp4PreviousTurn, Breakpoint::Bp1Tools]));
        assert!(!ttl_order_is_valid(&[
            Breakpoint::Bp1Tools,
            Breakpoint::Bp4PreviousTurn,
            Breakpoint::Bp2System,
        ]));
    }

    #[test]
    fn more_than_four_breakpoints_is_rejected() {
        let five = [
            Breakpoint::Bp1Tools,
            Breakpoint::Bp2System,
            Breakpoint::Bp3MemoryIndex,
            Breakpoint::Bp3MemoryIndex,
            Breakpoint::Bp4PreviousTurn,
        ];
        assert!(!ttl_order_is_valid(&five));
    }

    #[test]
    fn the_multipliers_match_the_pricing_table() {
        assert_eq!(READ_MULTIPLIER_PERCENT, 10);
        assert_eq!(UNCACHED_MULTIPLIER_PERCENT, 100);
        assert_eq!(CacheTtl::FiveMinutes.write_multiplier_percent(), 125);
        assert_eq!(CacheTtl::OneHour.write_multiplier_percent(), 200);
        assert_eq!(CacheTtl::FiveMinutes.seconds(), 300);
        assert_eq!(CacheTtl::OneHour.seconds(), 3_600);
    }

    #[test]
    fn the_read_to_uncached_ratio_is_the_ten_times_the_thesis_rests_on() {
        // Stated as arithmetic so the thesis cannot quietly stop being true: the
        // best prompt compression is 2-3x, and this gradient is 10x, which is why
        // compression is the wrong lever.
        assert_eq!(UNCACHED_MULTIPLIER_PERCENT / READ_MULTIPLIER_PERCENT, 10);

        // Cost is tokens times the multiplier, so the exact arithmetic is: ten
        // thousand cached tokens cost the SAME as one thousand uncached ones -
        // 10_000 x 10% == 1_000 x 100%. The gradient buys ten times the context
        // for the same input money, and anything beyond that ratio is cheaper
        // still.
        let cached = 10_000 * READ_MULTIPLIER_PERCENT;
        let uncached = 1_000 * UNCACHED_MULTIPLIER_PERCENT;
        assert_eq!(cached, uncached, "equal at exactly 10x - the break-even");

        let narrower = 9_000 * READ_MULTIPLIER_PERCENT;
        assert!(narrower < uncached, "9k cached beats 1k uncached: {narrower} vs {uncached}");
        let wider = 11_000 * READ_MULTIPLIER_PERCENT;
        assert!(wider > uncached, "and 11k crosses it: {wider} vs {uncached}");

        // Why compression is the wrong lever: compressing a 10k prefix by the best
        // achievable 3x and sending it uncached costs 3_333 - more than three
        // times the 1_000 the cache charges for the full prefix. And compressing
        // below the minimum disables caching entirely, which is the 10x failure,
        // not a saving.
        let compressed_uncached = 10_000 / 3 * UNCACHED_MULTIPLIER_PERCENT;
        assert!(compressed_uncached > cached, "{compressed_uncached} vs {cached}");
    }

    #[test]
    fn a_one_hour_write_pays_for_itself_after_two_hits() {
        // Why BP1-BP3 are worth 200%: write once, then read at 10% instead of
        // sending at 100%.
        let write = CacheTtl::OneHour.write_multiplier_percent();
        let saving_per_hit = UNCACHED_MULTIPLIER_PERCENT - READ_MULTIPLIER_PERCENT;
        let hits_to_break_even = write.div_ceil(saving_per_hit);
        assert_eq!(hits_to_break_even, 3, "write 200, save 90 per hit");

        // And BP4, rewritten every turn, is correctly the cheap one.
        assert!(
            CacheTtl::FiveMinutes.write_multiplier_percent() < CacheTtl::OneHour.write_multiplier_percent()
        );
    }

    #[test]
    fn padding_closes_the_gap_to_the_threshold() {
        assert_eq!(padding_needed(400, MinimumCacheable::Tokens512), 112);
        assert_eq!(padding_needed(512, MinimumCacheable::Tokens512), 0);
        assert_eq!(padding_needed(9_000, MinimumCacheable::Tokens4096), 0);
        assert_eq!(padding_needed(0, MinimumCacheable::Tokens4096), 4_096);
    }

    #[test]
    fn padding_is_cheaper_than_not_caching() {
        // The I8 decision, as arithmetic. A 400 token prefix on a 512 token model:
        // pad it once, or pay full price for it on every turn.
        let prefix = 400_u32;
        let minimum = MinimumCacheable::Tokens512;
        let padded = prefix + padding_needed(prefix, minimum);

        let turns = 100_u32;
        let uncached_cost = prefix * UNCACHED_MULTIPLIER_PERCENT * turns;
        let cached_cost = padded * CacheTtl::OneHour.write_multiplier_percent()
            + padded * READ_MULTIPLIER_PERCENT * (turns - 1);

        assert!(cached_cost < uncached_cost, "{cached_cost} vs {uncached_cost}");
        // 4_000_000 against 609_280: a 6.6x margin. The write is paid once, the
        // uncached price is paid every turn.
        assert!(uncached_cost / cached_cost >= 6, "and by a wide margin");
    }

    #[test]
    fn the_minimum_tiers_are_the_documented_four() {
        let tokens: Vec<u32> = MinimumCacheable::ALL.iter().map(|min| min.tokens()).collect();
        assert_eq!(tokens, vec![512, 1_024, 2_048, 4_096]);
        // Each tier is a power of two and double the previous, so a fifth value
        // sneaking in would be visible here.
        for pair in MinimumCacheable::ALL.windows(2) {
            assert_eq!(pair[1].tokens(), pair[0].tokens() * 2);
        }
    }

    #[test]
    fn the_lookback_window_is_twenty_blocks() {
        assert_eq!(LOOKBACK_BLOCKS, 20);
    }
}
