use supra_types::Event;

/// The eight lifecycle points a hook may attach to. Ordered as the turn
/// loop meets them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum HookPoint {
    /// Before the session starts.
    SessionStart,
    /// After the session ends.
    SessionEnd,
    /// Before a turn begins, before any prefix bytes move.
    TurnStart,
    /// After a turn completes, at the boundary.
    TurnEnd,
    /// Before a tool invocation runs.
    BeforeTool,
    /// After a tool invocation returns.
    AfterTool,
    /// Before the ledger evicts old turns.
    BeforeEvict,
    /// After a cache break is attributed.
    AfterCacheBreak,
}

impl HookPoint {
    /// All eight, in lifecycle order.
    pub const ALL: [Self; 8] = [
        Self::SessionStart,
        Self::SessionEnd,
        Self::TurnStart,
        Self::TurnEnd,
        Self::BeforeTool,
        Self::AfterTool,
        Self::BeforeEvict,
        Self::AfterCacheBreak,
    ];

    /// Whether the hook runs inside the prompt prefix's lifetime - that
    /// is, between BP1 and the suffix. Hooks that would run there are
    /// refused by [`crate::registry::Registry::register`]: a user
    /// command executing mid-prefix can mutate what the provider has
    /// already cached, and a cache break is the tax nobody asked for.
    /// Only boundary points - before the session, after the session,
    /// before a turn, after a turn - are prefix-safe.
    #[must_use]
    pub const fn is_prefix_safe(self) -> bool {
        matches!(self, Self::SessionStart | Self::SessionEnd | Self::TurnStart | Self::TurnEnd)
    }

    /// The name a configuration file uses for this point.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::SessionStart => "session-start",
            Self::SessionEnd => "session-end",
            Self::TurnStart => "turn-start",
            Self::TurnEnd => "turn-end",
            Self::BeforeTool => "before-tool",
            Self::AfterTool => "after-tool",
            Self::BeforeEvict => "before-evict",
            Self::AfterCacheBreak => "after-cache-break",
        }
    }

    /// Parse a configuration name back. Refuses unknown names rather
    /// than guessing - a typo'd hook point that silently never fires is
    /// invisible by construction.
    ///
    /// # Errors
    ///
    /// [`crate::error::HookError::UnknownPoint`] for a name that is
    /// not one of the eight.
    pub fn parse(text: &str) -> Result<Self, crate::error::HookError> {
        for point in Self::ALL {
            if point.name() == text {
                return Ok(point);
            }
        }
        Err(crate::error::HookError::UnknownPoint { name: text.to_owned() })
    }
}

/// What a hook sees: the event that fired it, plus nothing the hook
/// could use to mutate the prefix.
#[derive(Clone, Debug)]
pub struct HookContext {
    /// The triggering event.
    pub event: Event,
    /// The turn count so far, for hooks that want a cadence.
    pub turn_count: u64,
}

/// A hook's outcome.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum HookOutcome {
    /// Continue: nothing happens.
    #[default]
    Continue,
    /// Stop the current operation (a tool, a turn). Already-sealed
    /// segments stay sealed - a hook cannot unwrite the ledger.
    Stop,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exactly_the_four_boundaries_are_prefix_safe() {
        for point in HookPoint::ALL {
            assert_eq!(
                point.is_prefix_safe(),
                matches!(
                    point,
                    HookPoint::SessionStart
                        | HookPoint::SessionEnd
                        | HookPoint::TurnStart
                        | HookPoint::TurnEnd
                ),
                "{point:?}"
            );
        }
    }

    #[test]
    fn names_round_trip_and_unknown_refuse() {
        for point in HookPoint::ALL {
            assert_eq!(HookPoint::parse(point.name()), Ok(point));
        }
        assert!(HookPoint::parse("turn-start").is_ok());
        assert!(HookPoint::parse("mid-prefix").is_err());
    }
}
