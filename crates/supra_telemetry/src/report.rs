/// One telemetry report: what a session did, with no way to tell which
/// session did it.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Report {
    /// A random per-report id, not the session id: the session id is
    /// the identity a resume restores, and a report that carried it
    /// would let two reports be tied to one person.
    pub report: supra_types::SessionId,
    /// The cohort tier the session ran at.
    pub tier: supra_types::Tier,
    /// Turns completed.
    pub turns: u32,
    /// Tool invocations that ran.
    pub tools: u32,
    /// Cache hits among provider requests.
    pub cache_hits: u32,
}

/// The counts a session accumulates before its first report.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Counters {
    /// Turns completed.
    pub turns: u32,
    /// Tool invocations that ran.
    pub tools: u32,
    /// Cache hits among provider requests.
    pub cache_hits: u32,
}

impl Counters {
    /// Record one completed turn.
    pub fn turn(&mut self) {
        self.turns = self.turns.saturating_add(1);
    }

    /// Record one tool invocation.
    pub fn tool(&mut self) {
        self.tools = self.tools.saturating_add(1);
    }

    /// Record one cache hit.
    pub fn cache_hit(&mut self) {
        self.cache_hits = self.cache_hits.saturating_add(1);
    }

    /// Build a report at one tier, consuming the counters into a fresh
    /// report id. The report id is random per report: two reports from
    /// one session share nothing.
    #[must_use]
    pub fn report(&self, tier: supra_types::Tier) -> Report {
        Report {
            report: supra_types::SessionId::generate(),
            tier,
            turns: self.turns,
            tools: self.tools,
            cache_hits: self.cache_hits,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_report_carries_counts_but_not_identity() {
        let mut counters = Counters::default();
        counters.turn();
        counters.turn();
        counters.tool();
        counters.cache_hit();

        let report = counters.report(supra_types::Tier::E2);
        assert_eq!(report.turns, 2);
        assert_eq!(report.tools, 1);
        assert_eq!(report.cache_hits, 1);
        assert_eq!(report.tier, supra_types::Tier::E2);
    }

    #[test]
    fn two_reports_from_one_session_share_nothing() {
        let counters = Counters { turns: 5, tools: 3, cache_hits: 2 };
        let first = counters.report(supra_types::Tier::E1);
        let second = counters.report(supra_types::Tier::E1);
        assert_ne!(first.report, second.report, "the report id is random per report");
        assert_eq!(first.turns, second.turns, "the counts are the counts");
    }

    #[test]
    fn a_report_round_trips_through_json() {
        let report = Counters { turns: 9, tools: 4, cache_hits: 7 }.report(supra_types::Tier::E3);
        let text = serde_json::to_string(&report).expect("serialise");
        let back: Report = serde_json::from_str(&text).expect("parse");
        assert_eq!(back, report);
        assert!(!text.contains("path"), "{text}");
        assert!(!text.contains("user"), "{text}");
    }

    #[test]
    fn counters_saturate_rather_than_wrap() {
        let mut counters = Counters { turns: u32::MAX, tools: u32::MAX, cache_hits: u32::MAX };
        counters.turn();
        counters.tool();
        counters.cache_hit();
        assert_eq!(counters.turns, u32::MAX);
        assert_eq!(counters.tools, u32::MAX);
        assert_eq!(counters.cache_hits, u32::MAX);
    }
}
