use supra_types::FindingId;

/// Where a finding came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Kind {
    /// A static analysis tool reported it.
    Static,
    /// A test or execution reported it.
    Dynamic,
    /// Peers disagreed on the same question.
    CrossAgent,
}

/// One detected problem, as the blackboard and the TUI read it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Finding {
    /// The finding's id.
    pub id: FindingId,
    /// Which gate produced it.
    pub kind: Kind,
    /// The tool or check that reported it.
    pub source: &'static str,
    /// One-line summary, already redaction-safe.
    pub summary: String,
    /// The file the finding points at, when it has one.
    pub path: Option<String>,
    /// 1-based line, when the finding has one.
    pub line: Option<u32>,
}

impl Finding {
    /// Mint a finding.
    #[must_use]
    pub fn new(
        kind: Kind,
        source: &'static str,
        summary: impl Into<String>,
        path: Option<String>,
        line: Option<u32>,
    ) -> Self {
        Self { id: FindingId::generate(), kind, source, summary: summary.into(), path, line }
    }

    /// The evidence reference for a blackboard verdict: short, stable,
    /// the shape T6's `Verdict::evidence_ref` budget allows.
    #[must_use]
    pub fn evidence_ref(&self) -> String {
        let at = self
            .path
            .as_ref()
            .zip(self.line)
            .map_or_else(|| self.source.to_owned(), |(path, line)| format!("{path}:{line}"));
        format!("{}#{at}", self.source)
    }
}

/// Escalation weight per confirmed finding, as §4's table reads: one
/// finding already earns E4, so the count matters to the report, not to
/// the tier function (T15.5 saturates).
#[must_use]
pub fn escalates(kind: Kind) -> bool {
    matches!(kind, Kind::Static | Kind::Dynamic | Kind::CrossAgent)
}
