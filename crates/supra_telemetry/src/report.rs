/// One telemetry report: what a session did, with no way to tell which
/// session did it.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct Report {
    /// A fresh, uncorrelated identity: not the session id, and not
    /// anything two reports could be joined on. Constructed only by
    /// [`Counters::report`], so a caller cannot smuggle a session id in
    /// where a report id belongs.
    identity: ReportId,
    tier: supra_types::Tier,
    turns: u32,
    tools: u32,
    cache_hits: u32,
}

/// The opaque identity of one report.
///
/// Wraps a ULID the same shape as the other identities in the system,
/// but the wrapping type is the point: a `ReportId` cannot be built
/// from a [`supra_types::SessionId`], so the linkable field cannot hold
/// linkable data by construction.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
pub struct ReportId(supra_types::SessionId);

impl ReportId {
    /// The textual spelling, for a log line or a header.
    #[must_use]
    pub fn as_str(&self) -> String {
        self.0.to_string()
    }
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
            identity: ReportId(supra_types::SessionId::generate()),
            tier,
            turns: self.turns,
            tools: self.tools,
            cache_hits: self.cache_hits,
        }
    }
}

impl Report {
    /// The report's own identity.
    #[must_use]
    pub const fn id(&self) -> ReportId {
        self.identity
    }

    /// The cohort tier the session ran at.
    #[must_use]
    pub const fn tier(&self) -> supra_types::Tier {
        self.tier
    }

    /// Turns completed.
    #[must_use]
    pub const fn turns(&self) -> u32 {
        self.turns
    }

    /// Tool invocations that ran.
    #[must_use]
    pub const fn tools(&self) -> u32 {
        self.tools
    }

    /// Cache hits among provider requests.
    #[must_use]
    pub const fn cache_hits(&self) -> u32 {
        self.cache_hits
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
        assert_eq!(report.turns(), 2);
        assert_eq!(report.tools(), 1);
        assert_eq!(report.cache_hits(), 1);
        assert_eq!(report.tier(), supra_types::Tier::E2);
    }

    #[test]
    fn two_reports_from_one_session_share_nothing() {
        let counters = Counters { turns: 5, tools: 3, cache_hits: 2 };
        let first = counters.report(supra_types::Tier::E1);
        let second = counters.report(supra_types::Tier::E1);
        assert_ne!(first.id(), second.id(), "the report id is random per report");
        assert_eq!(first.turns(), second.turns(), "the counts are the counts");
    }

    #[test]
    fn a_report_serialises_without_linkable_fields() {
        let report = Counters { turns: 9, tools: 4, cache_hits: 7 }.report(supra_types::Tier::E3);
        let text = serde_json::to_string(&report).expect("serialise");
        assert!(text.contains("\"identity\":"), "{text}");
        assert!(!text.contains("session"), "{text}");
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
