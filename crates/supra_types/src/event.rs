//! The event taxonomy.
//!
//! One enum, because the event bus (T9) is a filtered pub/sub over a single stream:
//! the TUI subscribes to a set of topics, the telemetry sink to a different one, and
//! both get values of one type that they can match exhaustively. When a stage later
//! in the map adds an event, `match` sites that have not been updated fail to
//! compile, which is how the taxonomy stays coherent without a registry.
//!
//! Topics partition the variants. A topic is what a subscriber filters on; an event
//! is what it receives. The partition is enforced by a test that walks every variant
//! and asserts it reports the topic it is documented under.

use serde::{Deserialize, Serialize};

use crate::cache::Breakpoint;
use crate::cohort::{Confidence, Tier, Vote};
use crate::hash::ContentHash;
use crate::id::{AgentId, ClaimId, FindingId, SessionId, TurnId};
use crate::money::MicroUsd;
use crate::permission::{Decision, Invoker, Reversibility};
use crate::sealed::SeqNo;

/// What a subscriber filters on.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum Topic {
    /// Session lifecycle: start, resume, end.
    Session,
    /// The turn loop.
    Turn,
    /// The prompt ledger: seals, breakpoints, generations.
    Prompt,
    /// Prompt-cache behaviour, including breaks.
    Cache,
    /// Provider transport and usage.
    Provider,
    /// Cohorts, peers, claims, votes.
    Cohort,
    /// Tool invocation and the permission gate.
    Tool,
    /// Deterministic gates and findings.
    Gate,
    /// The anti-self-spawn guard.
    Guard,
    /// Write-ahead snapshots and undo.
    Journal,
    /// Repo digest and retrieval.
    Digest,
}

impl Topic {
    /// Every topic, for a subscriber that wants to enumerate what it can filter on.
    pub const ALL: [Self; 11] = [
        Self::Session,
        Self::Turn,
        Self::Prompt,
        Self::Cache,
        Self::Provider,
        Self::Cohort,
        Self::Tool,
        Self::Gate,
        Self::Guard,
        Self::Journal,
        Self::Digest,
    ];
}

/// Everything the harness can report.
///
/// Roughly forty variants is a floor, not a ceiling: each one is here because some
/// documented behaviour names it - `Event::CacheBreak` (I7), `Event::UsageDrift`
/// (section 7), quorum reached and unreachable (the turn loop, section 9), findings
/// that escalate a tier (section 4). An event the TUI does not render and no
/// subscriber filters for is not here.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Event {
    // -- Session ------------------------------------------------------------
    /// The harness started a new session.
    SessionStarted {
        /// Identity of the new session.
        session: SessionId,
    },
    /// A previous session was restored from disk.
    SessionResumed {
        /// Identity of the restored session.
        session: SessionId,
    },
    /// The session ended, cleanly or not.
    SessionEnded {
        /// Which session.
        session: SessionId,
    },

    // -- Turn ---------------------------------------------------------------
    /// A turn began: a user prompt arrived.
    TurnStarted {
        /// Identity of the turn.
        turn: TurnId,
    },
    /// The turn's winning claim was executed and its response delivered.
    TurnCompleted {
        /// Which turn.
        turn: TurnId,
    },
    /// The turn failed without a usable response.
    TurnFailed {
        /// Which turn.
        turn: TurnId,
        /// Why, in one line.
        reason: String,
    },

    // -- Prompt -------------------------------------------------------------
    /// A segment entered the ledger. Invariant I1's append, made observable.
    SegmentSealed {
        /// Position in the ledger.
        seq: SeqNo,
        /// Content digest of the sealed segment.
        hash: ContentHash,
    },
    /// A cache breakpoint was written into the request plan.
    BreakpointPlaced {
        /// Which one.
        breakpoint: Breakpoint,
    },
    /// The prefix was padded up to the model's minimum cacheable length.
    ///
    /// Emitted rather than silent because I8's alternative - caching failing with
    /// no error - is exactly the kind of invisibility this taxonomy exists to
    /// remove. A reader can compare padding against context length and notice a
    /// threshold that is being paid for constantly.
    PrefixPadded {
        /// Tokens the prefix had.
        before: u32,
        /// Tokens added.
        added: u32,
    },
    /// Old turns were evicted verbatim to the store.
    ///
    /// The event that makes invariant I4 visible: the evicted turns remain
    /// byte-identical in SQLite, and the index that replaces them grows
    /// append-only.
    TurnsEvicted {
        /// How many turns left the live prefix.
        count: u32,
        /// Byte count moved to the store, which a `recall` must reproduce exactly.
        bytes: u64,
    },
    /// A `recall` served a byte-identical original.
    RecallServed {
        /// The turn returned.
        turn: TurnId,
    },
    /// The prefix was rewritten into a new generation.
    ///
    /// I4's one-time rewrite, executed while idle and paid at the one-hour TTL.
    /// Rare by design - the 92-95% threshold exists so this stays rare.
    GenerationRewritten {
        /// Which generation this is, counting from 1.
        generation: u32,
        /// Cost of the rewrite, for the economics meter.
        cost: MicroUsd,
    },

    // -- Cache --------------------------------------------------------------
    /// A cache read: the prefix was served from cache.
    CacheHit,
    /// A cache write completed.
    CacheWrite {
        /// TTL the entry was written with.
        hours: bool,
    },
    /// The prefix hash changed unexpectedly.
    ///
    /// I7's headline event: an invisible cost leak becomes a debuggable defect.
    /// `cause` names the mechanism and `detail` carries the causing diff, so the
    /// meter's dip and the log line answer the same question.
    CacheBreak {
        /// What invalidated the prefix.
        cause: CacheBreakCause,
        /// The causing diff, or a one-line description of it.
        detail: String,
    },

    // -- Provider -----------------------------------------------------------
    /// A request went out.
    RequestSent {
        /// Which peer sent it.
        agent: AgentId,
    },
    /// The first response byte arrived.
    ///
    /// The moment the prompt becomes cacheable. I6's choreography - warm up, await
    /// first byte, then fan out - is sequenced on this event.
    FirstByte {
        /// Which peer.
        agent: AgentId,
    },
    /// The warm-up request reached its first byte; the cohort may fan out.
    WarmupComplete {
        /// Which shard warmed up.
        shard: u32,
    },
    /// A shard fanned out to its peers.
    FanOut {
        /// Which shard.
        shard: u32,
        /// How many peers it carries.
        peers: u32,
    },
    /// Output tokens streamed, for the live view.
    StreamDelta {
        /// How many text characters arrived in this delta.
        chars: u32,
    },
    /// Reasoning tokens streamed, for the collapsed-by-default thinking view.
    ThinkingDelta {
        /// How many reasoning characters arrived in this delta.
        chars: u32,
    },
    /// The provider refused or dropped the request.
    RequestFailed {
        /// Which peer.
        agent: AgentId,
        /// Whether the harness will retry.
        will_retry: bool,
    },
    /// The provider asked the client to slow down.
    RateLimited {
        /// Backoff the provider requested, in milliseconds.
        retry_after_ms: u64,
    },
    /// Final usage for one response, reconciled against the local estimate.
    UsageReported {
        /// Which peer.
        agent: AgentId,
        /// Tokens served from cache.
        cached_read: u32,
        /// Tokens written to cache.
        cached_write: u32,
        /// Tokens billed uncached.
        uncached: u32,
        /// Output tokens, including reasoning.
        output: u32,
        /// Cost, computed by T13 from its per-provider rates.
        cost: MicroUsd,
    },
    /// The live estimate diverged from the reconciled figure by more than 10%.
    ///
    /// The token counter needs calibration. Carrying both sides turns a vague
    /// "costs feel wrong" into a number and a turn.
    UsageDrift {
        /// What the local counter estimated.
        estimated: MicroUsd,
        /// What the provider reported.
        actual: MicroUsd,
        /// Divergence as a percentage of the reconciled figure.
        percent: u32,
    },

    // -- Cohort -------------------------------------------------------------
    /// Evidence was scored into a cohort tier. Zero LLM calls were spent.
    CohortTierEstimated {
        /// The tier selected.
        tier: Tier,
        /// How many peers it will field.
        peers: u32,
        /// How many shards, bounded by the per-minute rate limit.
        shards: u32,
    },
    /// A peer was admitted to the cohort.
    PeerSpawned {
        /// The peer.
        agent: AgentId,
        /// Its lineage, which the guard re-checks on admission.
        depth: u32,
    },
    /// The guard refused a spawn.
    SpawnRefused {
        /// Who tried to spawn.
        parent: AgentId,
        /// What would have been spawned.
        child: AgentId,
    },
    /// A peer published a claim to the blackboard.
    ClaimPublished {
        /// The claim.
        claim: ClaimId,
        /// Who proposed it.
        proposer: AgentId,
    },
    /// A peer's role rotated: proposer for one claim, validator for the next.
    RoleRotated {
        /// The peer whose role moved.
        agent: AgentId,
    },
    /// A validator voted.
    VoteCast {
        /// The claim being voted on.
        claim: ClaimId,
        /// The voter.
        voter: AgentId,
        /// The vote.
        vote: Vote,
        /// How sure the validator was.
        confidence: Confidence,
    },
    /// Enough peers agreed; in-flight work on this claim was aborted.
    QuorumReached {
        /// The claim.
        claim: ClaimId,
        /// Votes in favour.
        yes: u32,
        /// Peers needed, for checking the arithmetic at a glance.
        needed: u32,
    },
    /// Quorum became impossible.
    ///
    /// The turn loop escalates immediately on this rather than waiting for the
    /// remaining peers or for a timeout.
    QuorumUnreachable {
        /// The claim.
        claim: ClaimId,
        /// `yes + pending`, against the needed count.
        reachable: u32,
        /// Votes in favour needed.
        needed: u32,
    },
    /// The cohort aborted its remaining work.
    CohortAborted {
        /// Why: unreachable quorum, a user interrupt, or an escalation replacing
        /// the cohort.
        reason: String,
    },

    // -- Tool ---------------------------------------------------------------
    /// A tool invocation was requested.
    ToolRequested {
        /// Registered tool name.
        name: String,
        /// Who is asking.
        invoker: Invoker,
    },
    /// The permission gate decided.
    PermissionDecision {
        /// The tool.
        tool: String,
        /// What it decided.
        decision: Decision,
        /// Reversibility of the resolved effect, classified by T16.7.
        reversibility: Reversibility,
    },
    /// A tool call began.
    ToolStarted {
        /// The tool.
        tool: String,
    },
    /// A tool call completed.
    ToolCompleted {
        /// The tool.
        tool: String,
        /// How long it ran, in milliseconds.
        elapsed_ms: u64,
    },
    /// A tool call failed. Its error still becomes a `ToolResult` block: dropping
    /// it would leave a `tool_use` unanswered.
    ToolFailed {
        /// The tool.
        tool: String,
        /// One-line failure description.
        error: String,
    },

    // -- Gate ---------------------------------------------------------------
    /// A deterministic gate ran.
    GateCompleted {
        /// Which gate: compile, affected tests, introspector, or LSP.
        gate: String,
        /// Whether it passed.
        passed: bool,
    },
    /// A gate or a peer raised a finding.
    FindingRaised {
        /// The finding.
        finding: FindingId,
        /// Where, as `path:line`.
        location: String,
        /// Severity, as an integer from 1 (nit) to 5 (critical). A free string
        /// severity would drift across gates; the meaning per level is T22's
        /// contract.
        severity: u8,
    },
    /// A peer confirmed a finding.
    ///
    /// The escalation trigger: a confirmed finding raises the tier, which is how
    /// "self-improving" reaches the cohort size without an LLM call.
    FindingConfirmed {
        /// The finding.
        finding: FindingId,
        /// Who confirmed it.
        confirmer: AgentId,
    },

    // -- Guard --------------------------------------------------------------
    /// A guard layer fired, short of refusing a spawn.
    ///
    /// Layer numbering follows T12.5's seven layers. `SpawnRefused` is the
    /// terminal case; this is everything on the way there.
    GuardLayerTriggered {
        /// Which layer, 1 to 7.
        layer: u8,
        /// What it observed, in one line.
        detail: String,
    },

    // -- Journal ------------------------------------------------------------
    /// A write-ahead snapshot was recorded before a mutation.
    SnapshotTaken {
        /// How many journal entries exist, for the undo affordance.
        depth: u32,
    },
    /// A snapshot was restored.
    Undone {
        /// Remaining undo depth.
        depth: u32,
    },

    // -- Digest -------------------------------------------------------------
    /// The incremental digest noticed a workspace change.
    DigestUpdated {
        /// Files whose symbols, graph, or churn changed.
        files_changed: u32,
    },
    /// Anchors were appended to the request suffix.
    AnchorsServed {
        /// How many, and therefore roughly how many tokens of orientation were
        /// bought for zero LLM calls.
        count: u32,
    },
}

impl Event {
    /// The topic a subscriber filters on.
    #[must_use]
    pub const fn topic(&self) -> Topic {
        match self {
            Self::SessionStarted { .. } | Self::SessionResumed { .. } | Self::SessionEnded { .. } => {
                Topic::Session
            }
            Self::TurnStarted { .. } | Self::TurnCompleted { .. } | Self::TurnFailed { .. } => Topic::Turn,
            Self::SegmentSealed { .. }
            | Self::BreakpointPlaced { .. }
            | Self::PrefixPadded { .. }
            | Self::TurnsEvicted { .. }
            | Self::RecallServed { .. }
            | Self::GenerationRewritten { .. } => Topic::Prompt,
            Self::CacheHit | Self::CacheWrite { .. } | Self::CacheBreak { .. } => Topic::Cache,
            Self::RequestSent { .. }
            | Self::FirstByte { .. }
            | Self::WarmupComplete { .. }
            | Self::FanOut { .. }
            | Self::StreamDelta { .. }
            | Self::ThinkingDelta { .. }
            | Self::RequestFailed { .. }
            | Self::RateLimited { .. }
            | Self::UsageReported { .. }
            | Self::UsageDrift { .. } => Topic::Provider,
            Self::CohortTierEstimated { .. }
            | Self::PeerSpawned { .. }
            | Self::SpawnRefused { .. }
            | Self::ClaimPublished { .. }
            | Self::RoleRotated { .. }
            | Self::VoteCast { .. }
            | Self::QuorumReached { .. }
            | Self::QuorumUnreachable { .. }
            | Self::CohortAborted { .. } => Topic::Cohort,
            Self::ToolRequested { .. }
            | Self::PermissionDecision { .. }
            | Self::ToolStarted { .. }
            | Self::ToolCompleted { .. }
            | Self::ToolFailed { .. } => Topic::Tool,
            Self::GateCompleted { .. } | Self::FindingRaised { .. } | Self::FindingConfirmed { .. } => {
                Topic::Gate
            }
            Self::GuardLayerTriggered { .. } => Topic::Guard,
            Self::SnapshotTaken { .. } | Self::Undone { .. } => Topic::Journal,
            Self::DigestUpdated { .. } | Self::AnchorsServed { .. } => Topic::Digest,
        }
    }

    /// Every variant, constructed. Exhaustiveness is the point: adding a variant
    /// breaks this function, which breaks the tests that walk it, which is how the
    /// topic partition and the round-trip property stay true without maintenance.
    #[cfg(test)]
    fn samples() -> Vec<Self> {
        let agent = AgentId::generate();
        let turn = TurnId::generate();
        let claim = ClaimId::generate();
        let finding = FindingId::generate();
        let session = SessionId::generate();
        let hash = ContentHash::from_hex(&"ab".repeat(32)).expect("valid hex");

        vec![
            Self::SessionStarted { session },
            Self::SessionResumed { session },
            Self::SessionEnded { session },
            Self::TurnStarted { turn },
            Self::TurnCompleted { turn },
            Self::TurnFailed { turn, reason: "provider down".to_owned() },
            Self::SegmentSealed { seq: SeqNo::ZERO, hash },
            Self::BreakpointPlaced { breakpoint: Breakpoint::Bp1Tools },
            Self::PrefixPadded { before: 400, added: 112 },
            Self::TurnsEvicted { count: 3, bytes: 12_000 },
            Self::RecallServed { turn },
            Self::GenerationRewritten { generation: 2, cost: MicroUsd::from_micros(32_400) },
            Self::CacheHit,
            Self::CacheWrite { hours: true },
            Self::CacheBreak {
                cause: CacheBreakCause::VolatileDataInPrefix,
                detail: "system: timestamp line removed".to_owned(),
            },
            Self::RequestSent { agent },
            Self::FirstByte { agent },
            Self::WarmupComplete { shard: 0 },
            Self::FanOut { shard: 0, peers: 15 },
            Self::StreamDelta { chars: 24 },
            Self::ThinkingDelta { chars: 96 },
            Self::RequestFailed { agent, will_retry: true },
            Self::RateLimited { retry_after_ms: 30_000 },
            Self::UsageReported {
                agent,
                cached_read: 5_400,
                cached_write: 900,
                uncached: 200,
                output: 480,
                cost: MicroUsd::from_micros(14_000),
            },
            Self::UsageDrift {
                estimated: MicroUsd::from_micros(14_000),
                actual: MicroUsd::from_micros(20_000),
                percent: 30,
            },
            Self::CohortTierEstimated { tier: Tier::E2, peers: 3, shards: 1 },
            Self::PeerSpawned { agent, depth: 1 },
            Self::SpawnRefused { parent: agent, child: AgentId::generate() },
            Self::ClaimPublished { claim, proposer: agent },
            Self::RoleRotated { agent },
            Self::VoteCast { claim, voter: agent, vote: Vote::Yes, confidence: Confidence::High },
            Self::QuorumReached { claim, yes: 2, needed: 2 },
            Self::QuorumUnreachable { claim, reachable: 1, needed: 2 },
            Self::CohortAborted { reason: "quorum unreachable".to_owned() },
            Self::ToolRequested { name: "edit_file".to_owned(), invoker: Invoker::Agent },
            Self::PermissionDecision {
                tool: "edit_file".to_owned(),
                decision: crate::permission::Decision::Run,
                reversibility: crate::permission::Reversibility::R1,
            },
            Self::ToolStarted { tool: "edit_file".to_owned() },
            Self::ToolCompleted { tool: "edit_file".to_owned(), elapsed_ms: 4 },
            Self::ToolFailed { tool: "edit_file".to_owned(), error: "not read this session".to_owned() },
            Self::GateCompleted { gate: "compile".to_owned(), passed: true },
            Self::FindingRaised { finding, location: "src/lib.rs:17".to_owned(), severity: 3 },
            Self::FindingConfirmed { finding, confirmer: agent },
            Self::GuardLayerTriggered { layer: 4, detail: "binary identity mismatch".to_owned() },
            Self::SnapshotTaken { depth: 3 },
            Self::Undone { depth: 2 },
            Self::DigestUpdated { files_changed: 5 },
            Self::AnchorsServed { count: 10 },
        ]
    }
}

/// Why the prefix hash changed unexpectedly.
///
/// Enumerated from the four documented cache-break mechanisms, plus the two supra
///specific ones. The closed set matters: an unknown cause would be a bug in the
/// attribution logic itself, which is I7's whole premise, so a new mechanism must
/// be named here rather than smuggled through a string.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum CacheBreakCause {
    /// The tool manifest changed after startup. Invariant I3 forbids this.
    ToolManifestChanged,
    /// The system contract changed mid-session.
    SystemContractChanged,
    /// The memory index region changed without a new generation.
    MemoryIndexChanged,
    /// History was summarised, compacted, or truncated in place.
    HistoryRewritten,
    /// Volatile state reached the prefix. Invariant I2 forbids this.
    VolatileDataInPrefix,
    /// The thinking budget changed mid-session, which rewrites the prompt. It is
    /// frozen at startup precisely so this cannot happen.
    ThinkingBudgetChanged,
    /// Serialisation was not byte-stable, for instance unstable `tool_use` key
    /// ordering.
    NonDeterministicSerialisation,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every variant must report the topic its section documents. Walked through
    /// [`Event::samples`], which fails to compile whenever a variant is added
    /// without a sample.
    #[test]
    fn every_variant_reports_the_topic_its_section_documents() {
        let expected = [
            (Topic::Session, 3),
            (Topic::Turn, 3),
            (Topic::Prompt, 6),
            (Topic::Cache, 3),
            (Topic::Provider, 10),
            (Topic::Cohort, 9),
            (Topic::Tool, 5),
            (Topic::Gate, 3),
            (Topic::Guard, 1),
            (Topic::Journal, 2),
            (Topic::Digest, 2),
        ];

        let samples = Event::samples();
        assert_eq!(samples.len(), 47, "the sample list must cover every variant");

        for topic in Topic::ALL {
            let want = expected
                .iter()
                .find(|(name, _)| *name == topic)
                .unwrap_or_else(|| panic!("the expected table must list every topic, including {topic:?}"));
            let got = samples.iter().filter(|event| event.topic() == topic).count();
            assert_eq!(got, want.1, "{topic:?}");
        }
    }

    #[test]
    fn topics_are_covered() {
        assert_eq!(Topic::ALL.len(), 11);
        for topic in Topic::ALL {
            assert!(Event::samples().iter().any(|event| event.topic() == topic), "{topic:?} has no events");
        }
    }

    #[test]
    fn events_round_trip_through_serde() {
        // T9 persists its bus and T26 resumes sessions from files; an event that
        // does not survive a round trip breaks both.
        for event in Event::samples() {
            let json = serde_json::to_string(&event).expect("serialise");
            let restored: Event = serde_json::from_str(&json).expect("deserialise");
            assert_eq!(restored, event, "{event:?}");
        }
    }

    #[test]
    fn every_break_cause_is_named() {
        // The causes are the documented mechanisms, enumerated. Adding a mechanism
        // without naming it here would be an unattributable break - I7's premise.
        let causes = [
            CacheBreakCause::ToolManifestChanged,
            CacheBreakCause::SystemContractChanged,
            CacheBreakCause::MemoryIndexChanged,
            CacheBreakCause::HistoryRewritten,
            CacheBreakCause::VolatileDataInPrefix,
            CacheBreakCause::ThinkingBudgetChanged,
            CacheBreakCause::NonDeterministicSerialisation,
        ];
        for cause in causes {
            // Constructibility is the assertion; the match arm is exhaustiveness.
            let event = Event::CacheBreak { cause, detail: "probe".to_owned() };
            assert_eq!(event.topic(), Topic::Cache);
        }
    }

    #[test]
    fn usage_drift_carries_both_sides_of_the_divergence() {
        // Section 7: a divergence above 10% is a calibration defect worth knowing
        // about, so the event must say what was estimated and what arrived.
        let event = Event::UsageDrift {
            estimated: MicroUsd::from_micros(14_000),
            actual: MicroUsd::from_micros(20_000),
            percent: 30,
        };
        let Event::UsageDrift { estimated, actual, percent } = event else {
            unreachable!("constructed as UsageDrift");
        };
        assert_eq!(estimated.micros(), 14_000);
        assert_eq!(actual.micros(), 20_000);
        assert_eq!(percent, 30);
    }
}
