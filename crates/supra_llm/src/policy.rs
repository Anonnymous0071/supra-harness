//! Per-provider cache behaviour: what may be cached, for how long, and from what length.
//!
//! # Why a policy per provider
//!
//! The four cache facts differ per provider: how many breakpoints may be carried, which
//! TTLs exist, what prefix order is required, and below what length caching fails
//! silently. T6's [`supra_types::cache`] holds the *verified arithmetic* (multipliers,
//! TTL durations, breakpoint order); this module holds the *per-provider assignment* of
//! that arithmetic to a wire format.
//!
//! # The three providers
//!
//! | Provider | Breakpoints | TTLs | Minimum | Prefix order |
//! | -------- | ----------- | ---- | ------- | ------------ |
//! | `Anthropic` | 4 | 1h, 5m | 1024 | tools, system, messages |
//! | Second provider | 1 (cache key) | implicit | 1024 | tools, system, messages |
//! | `Google` | implicit | implicit | 2048 | tools, system, messages |
//!
//! `Anthropic` is the fully-specified case: four explicit breakpoints with TTLs, exactly
//! T6's table. The second provider carries a single key rather than four breakpoints - the
//! prefix is still `tools, system, messages`, but there is one entry, not four. `Google`
//! documents only implicit caching with a 2048-4096 minimum and no TTL or prefix rules
//! at all, so its policy is conservative: assume the longest minimum, claim no TTL
//! control, and measure through `total_cached_tokens` rather than asserting behaviour
//! the provider never promised.
//!
//! # `Google` is unverified, and says so
//!
//! Section 11 records `Google`'s cache behaviour as unverified: TTL and prefix rules
//! undocumented, minimum stated as a 2048-4096 range. The policy below encodes that
//! honestly - `breakpoints: 1`, no explicit TTL, minimum 2048 - rather than guessing the
//! favourable end of the range. A policy that claims control the provider never promised
//! is how a silent 10x bill arrives.

use supra_types::{Breakpoint, CacheTtl, MinimumCacheable};

/// Which provider a policy describes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ProviderKind {
    /// Anthropic: four explicit breakpoints, 1h and 5m TTLs.
    Anthropic,
    /// `OpenAI`: one cache key, implicit otherwise.
    OpenAI,
    /// Google: implicit caching only, longest minimum assumed.
    Google,
}

impl ProviderKind {
    /// All three, in stage-map order.
    pub const ALL: [Self; 3] = [Self::Anthropic, Self::OpenAI, Self::Google];

    /// Short name, as used in configuration and logs.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Anthropic => "anthropic",
            Self::OpenAI => "openai",
            Self::Google => "google",
        }
    }

    /// Parse a short name. Refuses rather than guessing: an unknown provider string is a
    /// configuration defect, and defaulting it to any real provider would send the
    /// credential to the wrong endpoint.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "anthropic" => Some(Self::Anthropic),
            "openai" => Some(Self::OpenAI),
            "google" => Some(Self::Google),
            _ => None,
        }
    }
}

/// Per-provider cache behaviour.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CachePolicy {
    /// Which provider.
    pub kind: ProviderKind,
    /// How many breakpoint entries may be carried. Anthropic: 4. `OpenAI` and Google: 1 -
    /// a single key, not four breakpoints.
    pub max_breakpoints: usize,
    /// Whether explicit TTLs may be set per breakpoint. `Anthropic` only.
    pub explicit_ttl: bool,
    /// Below this length, caching fails silently with no error (I8).
    pub minimum: MinimumCacheable,
    /// Thinking-budget floor in tokens. Zero means the provider has no separate thinking
    /// parameter to floor - not that thinking is free, which it never is.
    pub min_thinking_tokens: u32,
}

impl CachePolicy {
    /// Anthropic: the fully-specified case.
    #[must_use]
    pub const fn anthropic() -> Self {
        Self {
            kind: ProviderKind::Anthropic,
            max_breakpoints: 4,
            explicit_ttl: true,
            minimum: MinimumCacheable::Tokens1024,
            min_thinking_tokens: 1024,
        }
    }

    /// `OpenAI`: one cache key, implicit otherwise.
    #[must_use]
    pub const fn openai() -> Self {
        Self {
            kind: ProviderKind::OpenAI,
            max_breakpoints: 1,
            explicit_ttl: false,
            minimum: MinimumCacheable::Tokens1024,
            min_thinking_tokens: 0,
        }
    }

    /// Google: implicit caching only, longest minimum assumed.
    ///
    /// The documented range is 2048-4096. Assuming 2048 pads to a length that may still
    /// not cache; assuming 4096 always caches at the cost of extra padding. Padding is
    /// 1.0x once, a miss is 1.0x every turn - so the higher floor wins.
    #[must_use]
    pub const fn google() -> Self {
        Self {
            kind: ProviderKind::Google,
            max_breakpoints: 1,
            explicit_ttl: false,
            minimum: MinimumCacheable::Tokens4096,
            min_thinking_tokens: 0,
        }
    }

    /// The policy for a provider kind.
    #[must_use]
    pub const fn for_kind(kind: ProviderKind) -> Self {
        match kind {
            ProviderKind::Anthropic => Self::anthropic(),
            ProviderKind::OpenAI => Self::openai(),
            ProviderKind::Google => Self::google(),
        }
    }

    /// The TTL for a breakpoint under this policy.
    ///
    /// Anthropic: T6's table verbatim. `OpenAI`/Google: no explicit TTL exists, so every
    /// breakpoint reports the five-minute TTL - the shorter one, because claiming an hour
    /// of lifetime the provider never promised is how a stale prefix gets trusted.
    #[must_use]
    pub const fn ttl_for(self, breakpoint: Breakpoint) -> CacheTtl {
        if self.explicit_ttl { breakpoint.ttl() } else { CacheTtl::FiveMinutes }
    }

    /// Whether a breakpoint sequence is transmittable under this policy.
    ///
    /// Two checks: no more entries than the provider accepts, and TTLs in non-increasing
    /// order (T6's rule, which binds even where the TTLs are implicit - a five-minute
    /// entry before a one-hour claim is still a lie about lifetimes).
    #[must_use]
    pub fn accepts_breakpoints(self, breakpoints: &[Breakpoint]) -> bool {
        breakpoints.len() <= self.max_breakpoints && supra_types::ttl_order_is_valid(breakpoints)
    }

    /// Whether the thinking budget passes this provider's floor.
    ///
    /// A zero budget disables thinking and always passes: the floor constrains reasoning,
    /// not its absence. Checked at startup, not at the first request.
    #[must_use]
    pub const fn thinking_allowed(self, budget: u32) -> bool {
        budget == 0 || budget >= self.min_thinking_tokens
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn anthropic_is_t6_verbatim() {
        // The fully-specified case: four breakpoints, T6's TTL table, 1024 minimum.
        let policy = CachePolicy::anthropic();
        assert_eq!(policy.max_breakpoints, 4);
        assert!(policy.explicit_ttl);
        assert_eq!(policy.minimum, MinimumCacheable::Tokens1024);
        assert_eq!(policy.ttl_for(Breakpoint::Bp1Tools), CacheTtl::OneHour);
        assert_eq!(policy.ttl_for(Breakpoint::Bp4PreviousTurn), CacheTtl::FiveMinutes);
        assert!(policy.accepts_breakpoints(&Breakpoint::ALL));
    }

    #[test]
    fn openai_carries_one_key_not_four_breakpoints() {
        let policy = CachePolicy::openai();
        assert_eq!(policy.max_breakpoints, 1);
        assert!(!policy.explicit_ttl);
        assert!(policy.accepts_breakpoints(&[Breakpoint::Bp1Tools]));
        assert!(!policy.accepts_breakpoints(&Breakpoint::ALL));
    }

    #[test]
    fn google_assumes_the_higher_floor() {
        // The documented range is 2048-4096. Assuming 2048 pads to a length that may
        // still not cache; assuming 4096 always caches at the cost of extra padding.
        // Padding is 1.0x once, a miss is 1.0x every turn - so the higher floor wins.
        assert_eq!(CachePolicy::google().minimum, MinimumCacheable::Tokens4096);
    }

    #[test]
    fn implicit_providers_claim_no_hour_lifetime() {
        // Claiming an hour the provider never promised is how a stale prefix gets
        // trusted. The short TTL is the honest one.
        for policy in [CachePolicy::openai(), CachePolicy::google()] {
            for breakpoint in Breakpoint::ALL {
                assert_eq!(policy.ttl_for(breakpoint), CacheTtl::FiveMinutes);
            }
        }
    }

    #[test]
    fn zero_budget_disables_thinking_on_every_provider() {
        for kind in ProviderKind::ALL {
            assert!(CachePolicy::for_kind(kind).thinking_allowed(0));
        }
    }

    #[test]
    fn the_anthropic_floor_is_checked_not_assumed() {
        let policy = CachePolicy::anthropic();
        assert!(!policy.thinking_allowed(1023));
        assert!(policy.thinking_allowed(1024));
    }

    #[test]
    fn provider_names_round_trip_and_refuse_unknowns() {
        for kind in ProviderKind::ALL {
            assert_eq!(ProviderKind::parse(kind.name()), Some(kind));
        }
        assert_eq!(ProviderKind::parse("azure"), None);
        assert_eq!(ProviderKind::parse(""), None);
        assert_eq!(ProviderKind::parse("ANTHROPIC"), None, "names are lowercase-exact");
    }
}
