use supra_blackboard::Blackboard;
use supra_eventbus::Bus;
use supra_types::{
    AgentId, Block, ClaimId, Event, PEER_CEILING, QuorumStatus, Role, Segment, SegmentId, SegmentKind,
    TurnId, Vote, shards_needed,
};

use crate::error::TurnError;

/// How one peer answers a fan-out.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PeerAnswer {
    /// Which peer answered.
    pub agent: AgentId,
    /// The answer body, already canonical.
    pub text: String,
}

/// Where the turn stands after each recorded vote.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    /// Still collecting votes; quorum reachable.
    Collecting,
    /// Quorum reached; remaining in-flight peers abort.
    Reached,
    /// Quorum unreachable; escalate now, do not await a timeout.
    Escalate,
}

/// One turn: the thirteen steps of section 9, driven one vote at a time.
///
/// The loop owns no LLM client: peers' answers arrive through
/// [`Turn::record`], so the choreography - publish, vote, evaluate,
/// abort, escalate - is testable without a network, and the runtime
/// (T30) supplies answers as its shards complete.
pub struct Turn {
    id: TurnId,
    session: Vec<AgentId>,
    blackboard: Blackboard,
    bus: Bus,
    claim: ClaimId,
    answer: Option<String>,
    votes_seen: usize,
    steps_seen: Vec<Step>,
}

impl Turn {
    /// Steps 1-4: anchors and tier estimation are the caller's inputs
    /// (pure functions over the digest); this starts the publish.
    ///
    /// # Panics
    ///
    /// Never: the first agent of a non-empty cohort is the proposer.
    ///
    /// # Errors
    ///
    /// [`TurnError::NoAnswer`] when the cohort is empty or exceeds the
    /// peer ceiling; [`TurnError::NoAnswer`] carrying the blackboard's
    /// reason when the publish refuses.
    pub fn start(
        blackboard: Blackboard,
        bus: Bus,
        task: &str,
        agents: Vec<AgentId>,
    ) -> Result<Self, TurnError> {
        if agents.is_empty() {
            return Err(TurnError::NoAnswer("an empty cohort cannot field a turn".to_owned()));
        }
        if agents.len() > PEER_CEILING {
            return Err(TurnError::NoAnswer(format!(
                "a cohort of {} exceeds the ceiling of {PEER_CEILING}",
                agents.len()
            )));
        }
        let turn_id = TurnId::generate();
        let Some(proposer) = agents.first() else {
            return Err(TurnError::NoAnswer("an empty cohort cannot field a turn".to_owned()));
        };
        let proposer = *proposer;
        let validators: Vec<AgentId> = agents.iter().copied().filter(|a| *a != proposer).collect();

        let mut blackboard = blackboard;
        let claim = blackboard
            .publish(turn_id, proposer, &validators, task)
            .map_err(|error| TurnError::NoAnswer(error.to_string()))?;
        bus.publish(Event::TurnStarted { turn: turn_id });
        bus.publish(Event::ClaimPublished { claim, proposer });

        Ok(Self {
            id: turn_id,
            session: agents,
            blackboard,
            bus,
            claim,
            answer: None,
            votes_seen: 0,
            steps_seen: Vec::new(),
        })
    }

    /// The claim id this turn published.
    #[must_use]
    pub fn claim(&self) -> ClaimId {
        self.claim
    }

    /// The turn's id.
    #[must_use]
    pub fn id(&self) -> TurnId {
        self.id
    }

    /// Cohort size, as published.
    #[must_use]
    pub fn k(&self) -> usize {
        self.session.len()
    }

    /// Shards the fan-out splits into, per the request budget.
    #[must_use]
    pub fn shards(&self) -> usize {
        shards_needed(self.session.len())
    }

    /// Step 6, one vote at a time: record a peer's answer and evaluate
    /// quorum incrementally.
    ///
    /// The first answer to arrive becomes the turn's working answer; a
    /// later peer votes yes exactly when it confirms the working answer
    /// - which is what a quorum over answers means.
    ///
    /// # Errors
    ///
    /// [`TurnError::NoAnswer`] carrying the blackboard's reason when the
    /// vote refuses; the turn keeps its prior state.
    pub fn record(&mut self, answer: PeerAnswer) -> Result<Step, TurnError> {
        let vote = if self.answer_is_supported(&answer) { Vote::Yes } else { Vote::No };
        let verdict = supra_types::Verdict::new(vote, supra_types::Confidence::Medium, &answer.text, None)
            .map_err(|error| TurnError::NoAnswer(error.to_string()))?;
        let outcome = self
            .blackboard
            .vote(self.claim, answer.agent, &verdict)
            .map_err(|error| TurnError::NoAnswer(error.to_string()))?;

        self.bus.publish(Event::VoteCast {
            claim: self.claim,
            voter: answer.agent,
            vote,
            confidence: supra_types::Confidence::Medium,
        });
        if self.answer.is_none() {
            self.answer = Some(answer.text);
        }
        self.votes_seen += 1;

        let step = match outcome.status {
            QuorumStatus::Open => Step::Collecting,
            QuorumStatus::Reached => {
                self.bus.publish(Event::QuorumReached {
                    claim: self.claim,
                    yes: u32::try_from(outcome.tally.yes()).unwrap_or(u32::MAX),
                    needed: u32::try_from(outcome.tally.needed()).unwrap_or(u32::MAX),
                });
                self.bus.publish(Event::CohortAborted {
                    reason: "quorum reached; in-flight peers aborted".to_owned(),
                });
                Step::Reached
            }
            QuorumStatus::Unreachable => {
                self.bus.publish(Event::QuorumUnreachable {
                    claim: self.claim,
                    reachable: u32::try_from(outcome.tally.yes() + outcome.tally.pending())
                        .unwrap_or(u32::MAX),
                    needed: u32::try_from(outcome.tally.needed()).unwrap_or(u32::MAX),
                });
                self.bus.publish(Event::CohortAborted {
                    reason: "quorum unreachable; escalating now".to_owned(),
                });
                Step::Escalate
            }
        };
        self.steps_seen.push(step);
        Ok(step)
    }

    fn answer_is_supported(&self, answer: &PeerAnswer) -> bool {
        match &self.answer {
            None => true,
            Some(working) => working == &answer.text,
        }
    }

    /// Steps 8 and 13: the winning claim's body, sealed into the ledger
    /// as the turn's answer segment.
    ///
    /// # Errors
    ///
    /// [`TurnError::EmptyClaim`] when the winning claim carries no body;
    /// [`TurnError::Ledger`] when the ledger refuses the append.
    pub fn finish(&mut self, ledger: &mut supra_prompt::PromptLedger) -> Result<SegmentId, TurnError> {
        let body = self.blackboard.claim(self.claim).map(|stored| stored.body).unwrap_or_default();
        if body.is_empty() {
            return Err(TurnError::EmptyClaim);
        }
        let id = SegmentId::generate();
        let segment = Segment::new(
            id,
            SegmentKind::Turn { turn: self.id, role: Role::Assistant },
            vec![Block::Text(body)],
        )?;
        let seq = ledger.append(segment)?;
        self.bus.publish(Event::SegmentSealed { seq, hash: ledger.prefix_hash() });
        self.bus.publish(Event::TurnCompleted { turn: self.id });
        Ok(id)
    }

    /// The turn's working answer, if any peer answered.
    #[must_use]
    pub fn answer(&self) -> Option<&str> {
        self.answer.as_deref()
    }

    /// The steps recorded so far, in order.
    #[must_use]
    pub fn steps(&self) -> &[Step] {
        &self.steps_seen
    }

    /// Votes recorded so far.
    #[must_use]
    pub fn votes_seen(&self) -> usize {
        self.votes_seen
    }

    /// The quorum status as the blackboard holds it now.
    #[must_use]
    pub fn status(&self) -> Option<QuorumStatus> {
        self.blackboard.outcome(self.claim).map(|outcome| outcome.status)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use supra_blackboard::Blackboard;
    use supra_eventbus::{Bus, TopicSet};
    use supra_store::Store;
    use supra_types::{AgentId, QuorumStatus, Topic};

    use super::*;

    fn board() -> Blackboard {
        let dir = std::env::temp_dir().join(format!(
            "supra-core-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.subsec_nanos())
        ));
        std::fs::create_dir_all(&dir).expect("dir");
        Blackboard::open(Arc::new(Store::open(dir.join("bb.db")).expect("store"))).expect("board")
    }

    fn agents(n: usize) -> Vec<AgentId> {
        (0..n).map(|_| AgentId::generate()).collect()
    }

    fn answer(agent: AgentId, text: &str) -> PeerAnswer {
        PeerAnswer { agent, text: text.to_owned() }
    }

    #[test]
    fn a_turn_reaches_quorum_and_finishes_through_the_ledger() {
        let peers = agents(3);
        let mut turn = Turn::start(board(), Bus::new(), "the fix", peers.clone()).expect("turn");
        assert_eq!(turn.k(), 3);
        assert_eq!(turn.shards(), 1);

        assert_eq!(turn.record(answer(peers[1], "fix it")).expect("v1"), Step::Collecting);
        assert_eq!(turn.answer(), Some("fix it"));
        assert_eq!(turn.record(answer(peers[2], "fix it")).expect("v2"), Step::Reached);
        assert_eq!(turn.status(), Some(QuorumStatus::Reached));
        assert_eq!(turn.steps(), &[Step::Collecting, Step::Reached]);

        let mut ledger = supra_prompt::PromptLedger::new();
        let id = turn.finish(&mut ledger).expect("finish");
        assert_eq!(ledger.len(), 1);
        assert!(turn.answer().is_some());
        assert_ne!(id, SegmentId::generate());
    }

    #[test]
    fn a_disagreeing_peer_votes_no_and_unreachable_escalates() {
        let peers = agents(4);
        let mut turn = Turn::start(board(), Bus::new(), "the fix", peers.clone()).expect("turn");
        assert_eq!(turn.record(answer(peers[1], "fix a")).expect("v1"), Step::Collecting);
        assert_eq!(
            turn.record(answer(peers[2], "fix b")).expect("v2"),
            Step::Collecting,
            "yes=1 no=1 pending=1: 1+1 >= needed 2, still reachable"
        );
        assert_eq!(
            turn.record(answer(peers[3], "fix c")).expect("v3"),
            Step::Escalate,
            "yes+pending = 1+0 < needed 2: escalate now"
        );
        assert_eq!(turn.status(), Some(QuorumStatus::Unreachable));
        assert_eq!(turn.steps(), &[Step::Collecting, Step::Collecting, Step::Escalate]);
    }

    #[test]
    fn every_vote_is_an_event_the_tui_can_stream() {
        let peers = agents(3);
        let bus = Bus::new();
        let subscription = bus.subscribe(TopicSet::of(Topic::Cohort));
        let mut turn = Turn::start(board(), bus, "the fix", peers.clone()).expect("turn");
        turn.record(answer(peers[1], "fix it")).expect("v1");
        turn.record(answer(peers[2], "fix it")).expect("v2");

        let mut saw_vote = false;
        let mut saw_reached = false;
        let mut saw_abort = false;
        while let Ok(delivery) = subscription.try_recv() {
            match delivery.event.as_ref() {
                Event::VoteCast { .. } => saw_vote = true,
                Event::QuorumReached { .. } => saw_reached = true,
                Event::CohortAborted { .. } => saw_abort = true,
                _ => {}
            }
        }
        assert!(saw_vote, "the vote is an event");
        assert!(saw_reached, "the reach is an event");
        assert!(saw_abort, "the abort is an event");
    }

    #[test]
    fn turn_events_carry_the_turn_topic() {
        let peers = agents(3);
        let bus = Bus::new();
        let subscription = bus.subscribe(TopicSet::of(Topic::Turn).union(TopicSet::of(Topic::Prompt)));
        let mut turn = Turn::start(board(), bus, "the fix", peers.clone()).expect("turn");
        turn.record(answer(peers[1], "fix it")).expect("v1");
        turn.record(answer(peers[2], "fix it")).expect("v2");
        let mut ledger = supra_prompt::PromptLedger::new();
        turn.finish(&mut ledger).expect("finish");

        let mut saw_started = false;
        let mut saw_segment = false;
        let mut saw_completed = false;
        while let Ok(delivery) = subscription.try_recv() {
            match delivery.event.as_ref() {
                Event::TurnStarted { .. } => saw_started = true,
                Event::SegmentSealed { .. } => saw_segment = true,
                Event::TurnCompleted { .. } => saw_completed = true,
                _ => {}
            }
        }
        assert!(saw_started && saw_segment && saw_completed);
    }

    #[test]
    fn an_empty_cohort_cannot_field_a_turn() {
        let result = Turn::start(board(), Bus::new(), "task", Vec::new());
        assert!(matches!(result, Err(TurnError::NoAnswer(_))));
    }

    #[test]
    fn a_cohort_over_the_ceiling_is_refused() {
        let too_many = agents(PEER_CEILING + 1);
        let result = Turn::start(board(), Bus::new(), "task", too_many);
        assert!(matches!(result, Err(TurnError::NoAnswer(_))));
    }

    #[test]
    fn the_proposer_never_votes_on_its_own_claim() {
        let peers = agents(2);
        let mut turn = Turn::start(board(), Bus::new(), "the fix", peers.clone()).expect("turn");
        let error = turn.record(answer(peers[0], "me too")).expect_err("proposer");
        assert!(matches!(error, TurnError::NoAnswer(_)));
    }

    #[test]
    fn a_late_peer_after_reach_is_refused_by_the_closed_claim() {
        let peers = agents(6);
        let mut turn = Turn::start(board(), Bus::new(), "the fix", peers.clone()).expect("turn");
        for voter in &peers[1..4] {
            assert_eq!(
                turn.record(answer(*voter, "fix")).expect("agreeing"),
                Step::Collecting,
                "quorum(6) = 4: the fourth yes is the reach"
            );
        }
        assert_eq!(turn.record(answer(peers[4], "fix")).expect("the reach"), Step::Reached);
        let result = turn.record(answer(peers[5], "fix"));
        assert!(matches!(result, Err(TurnError::NoAnswer(_))));
    }

    #[test]
    fn unreachable_emits_the_abort_event_with_the_escalation_reason() {
        let peers = agents(4);
        let bus = Bus::new();
        let subscription = bus.subscribe(TopicSet::of(Topic::Cohort));
        let mut turn = Turn::start(board(), bus, "the fix", peers.clone()).expect("turn");
        turn.record(answer(peers[1], "fix a")).expect("v1");
        turn.record(answer(peers[2], "fix b")).expect("v2");
        turn.record(answer(peers[3], "fix c")).expect("v3, unreachable");

        let mut saw_unreachable = false;
        let mut saw_abort = false;
        while let Ok(delivery) = subscription.try_recv() {
            match delivery.event.as_ref() {
                Event::QuorumUnreachable { .. } => saw_unreachable = true,
                Event::CohortAborted { reason } => {
                    assert!(reason.contains("escalating"), "{reason}");
                    saw_abort = true;
                }
                _ => {}
            }
        }
        assert!(saw_unreachable, "the unreachable flip is an event");
        assert!(saw_abort, "the escalation abort is an event");
    }

    #[test]
    fn a_cohort_over_the_ceiling_refuses_without_panicking() {
        let too_many = agents(PEER_CEILING + 1);
        let result = Turn::start(board(), Bus::new(), "task", too_many);
        let Err(TurnError::NoAnswer(reason)) = result else {
            panic!("over the ceiling must refuse");
        };
        assert!(reason.contains("ceiling"), "{reason}");
    }

    #[test]
    fn an_empty_cohort_refuses_without_panicking() {
        let result = Turn::start(board(), Bus::new(), "task", Vec::new());
        let Err(TurnError::NoAnswer(reason)) = result else {
            panic!("empty must refuse");
        };
        assert!(reason.contains("empty"), "{reason}");
    }

    #[test]
    fn an_empty_task_body_refuses_at_finish_not_at_publish() {
        let peers = agents(3);
        let mut turn = Turn::start(board(), Bus::new(), "x", peers.clone()).expect("turn");
        turn.record(answer(peers[1], "x")).expect("v1");
        turn.record(answer(peers[2], "x")).expect("v2, reached");

        let mut ledger = supra_prompt::PromptLedger::new();
        let id = turn.finish(&mut ledger).expect("a non-empty body seals");
        assert_ne!(id, SegmentId::generate());

        let empty = Turn::start(board(), Bus::new(), "x", peers.clone()).expect("turn 2");
        let bodyless = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| empty.claim()));
        assert!(bodyless.is_ok(), "querying a live claim never panics");
    }

    #[test]
    fn k_sixteen_fans_out_over_two_shards() {
        let peers = agents(16);
        let turn = Turn::start(board(), Bus::new(), "the fix", peers).expect("turn");
        assert_eq!(turn.shards(), 2, "16 peers over the 15/min budget need 2 shards");
    }
}
