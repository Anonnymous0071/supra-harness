use supra_blackboard::Blackboard;
use supra_eventbus::Bus;
use supra_types::{
    AgentId, Block, ClaimId, Event, PEER_CEILING, QuorumStatus, Role, Segment, SegmentId, SegmentKind,
    TurnId, Verdict, shards_needed,
};

use crate::error::TurnError;

/// How one peer answers a fan-out.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PeerAnswer {
    /// Which peer answered.
    pub agent: AgentId,
    /// Full proposer output. Validators leave this absent.
    pub text: Option<String>,
    /// Bounded structured validation. The proposer leaves this absent.
    pub verdict: Option<Verdict>,
}

impl PeerAnswer {
    /// Construct a full candidate answer from the proposer.
    #[must_use]
    pub fn proposal(agent: AgentId, text: impl Into<String>) -> Self {
        Self { agent, text: Some(text.into()), verdict: None }
    }

    /// Construct a validator response.
    #[must_use]
    pub const fn validation(agent: AgentId, verdict: Verdict) -> Self {
        Self { agent, text: None, verdict: Some(verdict) }
    }
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
    finished: bool,
    blind_rederivation: bool,
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
        if task.is_empty() {
            return Err(TurnError::NoAnswer("an empty task cannot field a turn".to_owned()));
        }
        let turn_id = TurnId::generate();
        let Some(proposer) = agents.first() else {
            return Err(TurnError::NoAnswer("an empty cohort cannot field a turn".to_owned()));
        };
        let proposer = *proposer;
        let validators: Vec<AgentId> = agents.iter().copied().filter(|agent| *agent != proposer).collect();
        let blind_rederivation = agents.len() == 1;

        let mut blackboard = blackboard;
        let claim = if blind_rederivation {
            blackboard.publish_blind(turn_id, proposer, "turn candidate")
        } else {
            blackboard.publish(turn_id, proposer, &validators, "turn candidate")
        }
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
            finished: false,
            blind_rederivation,
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

    /// Step 6, one response at a time: store the proposal or record a structured verdict.
    ///
    /// Only the first cohort member may submit full answer text. Validators submit
    /// [`Verdict`] values, so semantically equivalent approvals do not require byte-
    /// identical prose and answer length is independent of the verdict budget.
    ///
    /// # Errors
    ///
    /// [`TurnError::NoAnswer`] for a malformed response or a refused blackboard vote;
    /// the turn keeps its prior durable state.
    pub fn record(&mut self, answer: PeerAnswer) -> Result<Step, TurnError> {
        if let Some(text) = answer.text {
            if answer.verdict.is_some() {
                return Err(TurnError::NoAnswer(
                    "a peer response cannot be both a proposal and a verdict".to_owned(),
                ));
            }
            if answer.agent != self.session[0] {
                return Err(TurnError::NoAnswer("only the proposer may submit a full answer".to_owned()));
            }
            if text.is_empty() {
                return Err(TurnError::EmptyClaim);
            }
            if self.answer.replace(text).is_some() {
                return Err(TurnError::NoAnswer("the proposer already submitted an answer".to_owned()));
            }
            return Ok(Step::Collecting);
        }

        if self.answer.is_none() {
            return Err(TurnError::NoAnswer("a validator answered before the proposal".to_owned()));
        }
        let verdict = answer.verdict.ok_or_else(|| {
            TurnError::NoAnswer("a peer response contains neither answer nor verdict".to_owned())
        })?;
        let outcome = if self.blind_rederivation && answer.agent == self.session[0] {
            self.blackboard.vote_blind(self.claim, answer.agent, &verdict)
        } else {
            self.blackboard.vote(self.claim, answer.agent, &verdict)
        }
        .map_err(|error| TurnError::NoAnswer(error.to_string()))?;
        let vote = verdict.vote();

        self.bus.publish(Event::VoteCast {
            claim: self.claim,
            voter: answer.agent,
            vote,
            confidence: verdict.confidence(),
        });
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

    /// Steps 8 and 13: the agreed answer, sealed into the ledger as the
    /// turn's answer segment.
    ///
    /// The peers validated the working answer, so that is what the
    /// ledger receives - the claim's body on the blackboard is the task
    /// the cohort fanned out about, not the answer.
    ///
    /// # Errors
    ///
    /// [`TurnError::NoAnswer`] before quorum, after an escalation, or on
    /// a second finish; [`TurnError::EmptyClaim`] when the working
    /// answer is empty; [`TurnError::Ledger`] when the ledger refuses.
    pub fn finish(&mut self, ledger: &mut supra_prompt::PromptLedger) -> Result<SegmentId, TurnError> {
        if self.finished {
            return Err(TurnError::NoAnswer("the turn is already finished".to_owned()));
        }
        match self.status() {
            Some(QuorumStatus::Reached) => {}
            Some(QuorumStatus::Open) => {
                return Err(TurnError::NoAnswer("quorum has not been reached".to_owned()));
            }
            Some(QuorumStatus::Unreachable) | None => {
                return Err(TurnError::NoAnswer(
                    "the turn cannot finish once quorum is unreachable".to_owned(),
                ));
            }
        }
        let Some(answer) = &self.answer else {
            return Err(TurnError::NoAnswer("quorum reached without a working answer".to_owned()));
        };
        if answer.is_empty() {
            return Err(TurnError::EmptyClaim);
        }
        let id = SegmentId::generate();
        let segment = Segment::new(
            id,
            SegmentKind::Turn { turn: self.id, role: Role::Assistant },
            vec![Block::Text(answer.clone())],
        )?;
        let seq = ledger.append(segment)?;
        self.bus.publish(Event::SegmentSealed { seq, hash: ledger.prefix_hash() });
        self.bus.publish(Event::TurnCompleted { turn: self.id });
        self.finished = true;
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
    use supra_types::{AgentId, Confidence, QuorumStatus, Topic, Vote};

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
        PeerAnswer::proposal(agent, text)
    }

    fn validation(agent: AgentId, vote: Vote) -> PeerAnswer {
        PeerAnswer::validation(
            agent,
            Verdict::new(vote, Confidence::Medium, "independently checked", None).expect("verdict"),
        )
    }

    #[test]
    fn a_turn_reaches_quorum_and_finishes_through_the_ledger() {
        let peers = agents(3);
        let mut turn = Turn::start(board(), Bus::new(), "the fix", peers.clone()).expect("turn");
        assert_eq!(turn.k(), 3);
        assert_eq!(turn.shards(), 1);

        assert_eq!(turn.record(answer(peers[0], "fix it")).expect("proposal"), Step::Collecting);
        assert_eq!(turn.record(validation(peers[1], Vote::Yes)).expect("v1"), Step::Reached);
        assert_eq!(turn.answer(), Some("fix it"));
        assert_eq!(turn.status(), Some(QuorumStatus::Reached));
        assert_eq!(turn.steps(), &[Step::Reached]);

        let mut ledger = supra_prompt::PromptLedger::new();
        let id = turn.finish(&mut ledger).expect("finish");
        assert_eq!(ledger.len(), 1);
        assert!(turn.answer().is_some());
        assert_ne!(id, SegmentId::generate());
    }

    #[test]
    fn the_ledger_seals_the_agreed_answer_not_the_task() {
        let peers = agents(3);
        let mut turn = Turn::start(board(), Bus::new(), "the task text", peers.clone()).expect("turn");
        turn.record(answer(peers[0], "the agreed answer")).expect("proposal");
        turn.record(validation(peers[1], Vote::Yes)).expect("v1");

        let mut ledger = supra_prompt::PromptLedger::new();
        turn.finish(&mut ledger).expect("finish");
        let sealed: Vec<String> = ledger
            .segments()
            .iter()
            .flat_map(|entry| entry.blocks().iter())
            .filter_map(|block| match block {
                Block::Text(text) => Some(text.clone()),
                _ => None,
            })
            .collect();
        assert!(sealed.contains(&"the agreed answer".to_owned()), "{sealed:?}");
        assert!(!sealed.contains(&"the task text".to_owned()), "{sealed:?}");

        let error = turn.finish(&mut ledger).expect_err("second finish");
        assert!(matches!(error, TurnError::NoAnswer(_)));
    }

    #[test]
    fn finish_is_refused_before_quorum_and_after_escalation() {
        let peers = agents(3);
        let mut turn = Turn::start(board(), Bus::new(), "task", peers.clone()).expect("turn");
        turn.record(answer(peers[0], "an answer")).expect("proposal");
        let mut ledger = supra_prompt::PromptLedger::new();
        assert!(turn.finish(&mut ledger).is_err(), "open quorum cannot finish");

        let cohort = agents(4);
        let mut escalated = Turn::start(board(), Bus::new(), "task", cohort.clone()).expect("turn");
        escalated.record(answer(cohort[0], "candidate")).expect("proposal");
        for peer in &cohort[1..] {
            let _ = escalated.record(validation(*peer, Vote::No));
        }
        assert_eq!(escalated.steps().last(), Some(&Step::Escalate));
        assert!(escalated.finish(&mut ledger).is_err(), "escalated cannot finish");
    }

    #[test]
    fn a_disagreeing_peer_votes_no_and_unreachable_escalates() {
        let peers = agents(4);
        let mut turn = Turn::start(board(), Bus::new(), "the fix", peers.clone()).expect("turn");
        assert_eq!(turn.record(answer(peers[0], "fix a")).expect("proposal"), Step::Collecting);
        assert_eq!(turn.record(validation(peers[1], Vote::Yes)).expect("v1"), Step::Collecting);
        assert_eq!(
            turn.record(validation(peers[2], Vote::No)).expect("v2"),
            Step::Collecting,
            "yes=1 no=1 pending=1: 1+1 >= needed 2, still reachable"
        );
        assert_eq!(
            turn.record(validation(peers[3], Vote::No)).expect("v3"),
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
        turn.record(answer(peers[0], "fix it")).expect("proposal");
        turn.record(validation(peers[1], Vote::Yes)).expect("v1");

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
        turn.record(answer(peers[0], "fix it")).expect("proposal");
        turn.record(validation(peers[1], Vote::Yes)).expect("v1");
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
        turn.record(answer(peers[0], "candidate")).expect("proposal");
        let error = turn.record(answer(peers[0], "me too")).expect_err("second proposal");
        assert!(matches!(error, TurnError::NoAnswer(_)));
    }

    #[test]
    fn a_late_peer_after_reach_is_refused_by_the_closed_claim() {
        let peers = agents(6);
        let mut turn = Turn::start(board(), Bus::new(), "the fix", peers.clone()).expect("turn");
        turn.record(answer(peers[0], "fix")).expect("proposal");
        for voter in &peers[1..3] {
            assert_eq!(
                turn.record(validation(*voter, Vote::Yes)).expect("agreeing"),
                Step::Collecting,
                "quorum(6) = 4: proposer plus three validators reach"
            );
        }
        assert_eq!(turn.record(validation(peers[3], Vote::Yes)).expect("the reach"), Step::Reached);
        let result = turn.record(validation(peers[4], Vote::Yes));
        assert!(matches!(result, Err(TurnError::NoAnswer(_))));
    }

    #[test]
    fn unreachable_emits_the_abort_event_with_the_escalation_reason() {
        let peers = agents(4);
        let bus = Bus::new();
        let subscription = bus.subscribe(TopicSet::of(Topic::Cohort));
        let mut turn = Turn::start(board(), bus, "the fix", peers.clone()).expect("turn");
        turn.record(answer(peers[0], "fix a")).expect("proposal");
        turn.record(validation(peers[1], Vote::Yes)).expect("v1");
        turn.record(validation(peers[2], Vote::No)).expect("v2");
        turn.record(validation(peers[3], Vote::No)).expect("v3, unreachable");

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
    fn an_empty_task_body_is_refused_at_start() {
        let peers = agents(3);
        let result = Turn::start(board(), Bus::new(), "", peers);
        assert!(matches!(result, Err(TurnError::NoAnswer(_))));
    }

    #[test]
    fn a_single_peer_uses_a_blind_validation_pass() {
        let peers = agents(1);
        let mut turn = Turn::start(board(), Bus::new(), "task", peers.clone()).expect("turn");
        turn.record(answer(peers[0], "candidate")).expect("proposal");
        assert_eq!(turn.record(validation(peers[0], Vote::Yes)).expect("blind validation"), Step::Reached);
        assert_eq!(turn.status(), Some(QuorumStatus::Reached));
    }

    #[test]
    fn validators_cannot_submit_full_proposals() {
        let peers = agents(2);
        let mut turn = Turn::start(board(), Bus::new(), "task", peers.clone()).expect("turn");
        let error = turn.record(answer(peers[1], "candidate")).expect_err("validator proposal");
        assert!(matches!(error, TurnError::NoAnswer(_)));
        assert!(turn.answer().is_none());
    }

    #[test]
    fn k_sixteen_fans_out_over_two_shards() {
        let peers = agents(16);
        let turn = Turn::start(board(), Bus::new(), "the fix", peers).expect("turn");
        assert_eq!(turn.shards(), 2, "16 peers over the 15/min budget need 2 shards");
    }
}
