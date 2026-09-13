#![deny(missing_docs)]

use supra_types::{Event, MicroUsd, Vote};

/// Live counters folded from one bus drain.
///
/// A frame is what the renderer draws between drains: every counter the
/// status line needs, the thinking state, and the vote tally. Folding is
/// total over the drain - a second drain starts from the previous frame,
/// so a counter never resets mid-turn.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FrameState {
    /// Streamed text characters this turn.
    pub streamed_chars: u64,
    /// Streamed reasoning characters this turn.
    pub thinking_chars: u64,
    /// Reconciled spend in micro-dollars.
    pub spend: MicroUsd,
    /// Votes seen: (yes, total).
    pub votes: (u32, u32),
    /// Quorum reached on the current claim.
    pub quorum: bool,
    /// The cache prefix broke this turn.
    pub cache_broken: bool,
    /// Events the subscriber lost before this drain.
    pub missed_events: u64,
    /// Provider calls completed this turn.
    pub completions: u32,
}

impl FrameState {
    /// Fold one drained delivery into the frame.
    pub fn apply(&mut self, event: &Event, missed_before: u64) {
        self.missed_events = self.missed_events.saturating_add(missed_before);
        match event {
            Event::StreamDelta { chars } => {
                self.streamed_chars = self.streamed_chars.saturating_add(u64::from(*chars));
            }
            Event::ThinkingDelta { chars } => {
                self.thinking_chars = self.thinking_chars.saturating_add(u64::from(*chars));
            }
            Event::UsageReported { cost, .. } => {
                self.spend = MicroUsd::from_micros(self.spend.micros().saturating_add(cost.micros()));
                self.completions = self.completions.saturating_add(1);
            }
            Event::VoteCast { vote, .. } => {
                self.votes.1 = self.votes.1.saturating_add(1);
                if *vote == Vote::Yes {
                    self.votes.0 = self.votes.0.saturating_add(1);
                }
            }
            Event::QuorumReached { .. } => {
                self.quorum = true;
            }
            Event::CacheBreak { .. } => {
                self.cache_broken = true;
            }
            Event::TurnStarted { .. } => {
                *self = Self::default();
            }
            _ => {}
        }
    }

    /// The spend as status-line mills (thousandths of a dollar), rounded up.
    #[must_use]
    pub fn spend_mills(&self) -> u64 {
        let micros = u64::try_from(self.spend.micros().max(0)).unwrap_or(u64::MAX);
        micros.saturating_add(999) / 1_000
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use supra_types::{AgentId, ClaimId, Confidence};

    fn usage(cost_micros: i64) -> Event {
        Event::UsageReported {
            agent: AgentId::generate(),
            cached_read: 0,
            cached_write: 0,
            uncached: 100,
            output: 10,
            cost: MicroUsd::from_micros(cost_micros),
        }
    }

    #[test]
    fn stream_and_thinking_deltas_accumulate() {
        let mut frame = FrameState::default();
        frame.apply(&Event::StreamDelta { chars: 24 }, 0);
        frame.apply(&Event::ThinkingDelta { chars: 96 }, 0);
        frame.apply(&Event::StreamDelta { chars: 6 }, 0);
        assert_eq!(frame.streamed_chars, 30);
        assert_eq!(frame.thinking_chars, 96);
    }

    #[test]
    fn usage_sums_spend_and_counts_completions() {
        let mut frame = FrameState::default();
        frame.apply(&usage(1_400), 0);
        frame.apply(&usage(600), 0);
        assert_eq!(frame.spend_mills(), 2, "2000 micros rounds up to 2 mills");
        assert_eq!(frame.completions, 2);
    }

    #[test]
    fn votes_tally_yes_over_total_and_quorum_latches() {
        let mut frame = FrameState::default();
        let claim = ClaimId::generate();
        frame.apply(
            &Event::VoteCast {
                claim,
                voter: AgentId::generate(),
                vote: Vote::Yes,
                confidence: Confidence::High,
            },
            0,
        );
        frame.apply(
            &Event::VoteCast {
                claim,
                voter: AgentId::generate(),
                vote: Vote::No,
                confidence: Confidence::Low,
            },
            0,
        );
        assert_eq!(frame.votes, (1, 2));
        assert!(!frame.quorum);
        frame.apply(&Event::QuorumReached { claim, yes: 2, needed: 2 }, 0);
        assert!(frame.quorum);
    }

    #[test]
    fn a_cache_break_latches_until_the_next_turn() {
        let mut frame = FrameState::default();
        frame.apply(
            &Event::CacheBreak {
                cause: supra_types::CacheBreakCause::VolatileDataInPrefix,
                detail: "probe".to_owned(),
            },
            0,
        );
        assert!(frame.cache_broken);
        frame.apply(&Event::TurnStarted { turn: supra_types::TurnId::generate() }, 0);
        assert!(!frame.cache_broken, "a new turn starts clean");
        assert_eq!(frame.streamed_chars, 0);
    }

    #[test]
    fn missed_deliveries_accumulate_across_drains() {
        let mut frame = FrameState::default();
        frame.apply(&Event::StreamDelta { chars: 1 }, 3);
        frame.apply(&Event::StreamDelta { chars: 1 }, 2);
        assert_eq!(frame.missed_events, 5);
    }
}
