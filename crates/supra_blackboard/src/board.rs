use std::collections::BTreeMap;
use std::sync::Arc;

use rusqlite::TransactionBehavior;
use supra_store::Store;
use supra_types::{AgentId, ClaimId, QuorumTally, TurnId, Verdict, Vote, cohort::QuorumStatus};

use crate::error::BlackboardError;
use crate::schema::{self, COMPONENT, MIGRATIONS, StoredClaim, StoredVote};

/// Maximum claim body size in bytes.
pub const MAX_BODY_BYTES: usize = 2048;

/// The shared peer blackboard.
pub struct Blackboard {
    store: Arc<Store>,
    claims: BTreeMap<ClaimId, ClaimState>,
}

struct ClaimState {
    turn: TurnId,
    proposer: AgentId,
    body: String,
    k: usize,
    tally: QuorumTally,
    status: QuorumStatus,
    validators: BTreeMap<AgentId, Option<Vote>>,
}

/// The result of recording one vote.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Outcome {
    /// The claim voted on.
    pub claim: ClaimId,
    /// Where the claim now stands.
    pub status: QuorumStatus,
    /// The claim's tally after the vote.
    pub tally: QuorumTally,
}

impl Outcome {
    /// Whether quorum was reached.
    #[must_use]
    pub fn is_reached(&self) -> bool {
        self.status == QuorumStatus::Reached
    }
}

impl Blackboard {
    /// Open the board on a migrated store.
    ///
    /// # Errors
    ///
    /// [`BlackboardError::Store`] when the component migration fails.
    pub fn open(store: Arc<Store>) -> Result<Self, BlackboardError> {
        store.migrate_component(COMPONENT, MIGRATIONS)?;
        Ok(Self { store, claims: BTreeMap::new() })
    }

    /// Publish a claim and return its id.
    ///
    /// # Errors
    ///
    /// [`BlackboardError::Store`] when the body exceeds [`MAX_BODY_BYTES`]
    /// or the store refuses.
    pub fn publish(
        &mut self,
        turn: TurnId,
        proposer: AgentId,
        validators: &[AgentId],
        body: &str,
    ) -> Result<ClaimId, BlackboardError> {
        let k = validators.len() + 1;
        if validators.contains(&proposer) {
            return Err(BlackboardError::NotAProposer { agent: proposer, claim: ClaimId::generate() });
        }
        if body.len() > MAX_BODY_BYTES {
            return Err(BlackboardError::Store(supra_store::StoreError::Malformed {
                detail: format!("claim body is {} bytes, the budget is {MAX_BODY_BYTES}", body.len()),
            }));
        }
        let claim = ClaimId::generate();
        self.store.with_transaction::<_, BlackboardError>(TransactionBehavior::Immediate, |tx| {
            schema::insert_claim(tx, claim, turn, proposer, body, k, "open")?;
            Ok(())
        })?;
        self.claims.insert(
            claim,
            ClaimState {
                turn,
                proposer,
                body: body.to_owned(),
                k,
                tally: QuorumTally::new(k),
                status: QuorumStatus::Open,
                validators: validators.iter().copied().map(|id| (id, None)).collect(),
            },
        );
        Ok(claim)
    }

    /// Record one vote and report the claim's new standing.
    ///
    /// # Errors
    ///
    /// [`BlackboardError::UnknownClaim`], [`BlackboardError::Closed`],
    /// [`BlackboardError::NotAProposer`] when the proposer votes on its
    /// own claim, [`BlackboardError::DuplicateVote`],
    /// [`BlackboardError::NotInCohort`] when the voter is not one of the
    /// k-1 validators, or whatever the tally refuses.
    pub fn vote(
        &mut self,
        claim: ClaimId,
        voter: AgentId,
        verdict: &Verdict,
    ) -> Result<Outcome, BlackboardError> {
        let state = self.claims.get_mut(&claim).ok_or(BlackboardError::UnknownClaim { claim })?;
        if state.status != QuorumStatus::Open {
            return Err(BlackboardError::Closed { claim });
        }
        if voter == state.proposer {
            return Err(BlackboardError::NotAProposer { agent: voter, claim });
        }
        let Some(slot) = state.validators.get_mut(&voter) else {
            return Err(BlackboardError::NotInCohort { agent: voter, claim });
        };
        if slot.is_some() {
            return Err(BlackboardError::DuplicateVote { agent: voter, claim });
        }

        let vote = verdict.vote();
        let outcome = state.tally.record(vote).map_err(BlackboardError::Tally)?;
        *slot = Some(vote);
        state.status = outcome;

        self.store.with_transaction::<_, BlackboardError>(TransactionBehavior::Immediate, |tx| {
            schema::insert_vote(tx, claim, voter, verdict)?;
            if outcome != QuorumStatus::Open {
                let _ = schema::update_status(tx, claim, status_text(outcome))?;
            }
            Ok(())
        })?;

        let state = &self.claims[&claim];
        Ok(Outcome { claim, status: outcome, tally: state.tally })
    }

    /// The claim's current outcome, if the board holds it.
    #[must_use]
    pub fn outcome(&self, claim: ClaimId) -> Option<Outcome> {
        self.claims.get(&claim).map(|state| Outcome { claim, status: state.status, tally: state.tally })
    }

    /// The claim as stored, if the board holds it.
    #[must_use]
    pub fn claim(&self, claim: ClaimId) -> Option<StoredClaim> {
        self.claims.get(&claim).map(|state| StoredClaim {
            claim,
            turn: state.turn,
            proposer: state.proposer,
            body: state.body.clone(),
            k: state.k,
            status: status_text(state.status).to_owned(),
        })
    }

    /// The votes recorded for a claim, if the board holds it.
    #[must_use]
    pub fn votes(&self, claim: ClaimId) -> Option<Vec<StoredVote>> {
        self.claims.get(&claim).map(|state| {
            state
                .validators
                .iter()
                .filter_map(|(voter, vote)| vote.map(|vote| StoredVote { claim, voter: *voter, vote }))
                .collect()
        })
    }

    /// The candidate who has proposed least within `turn`, breaking ties
    /// by id, or `None` for an empty candidate list.
    #[must_use]
    pub fn next_proposer(&self, turn: TurnId, candidates: &[AgentId]) -> Option<AgentId> {
        let used: BTreeMap<TurnId, Vec<AgentId>> = BTreeMap::new();
        let _ = used;
        let mut counts: BTreeMap<AgentId, usize> = BTreeMap::new();
        for candidate in candidates {
            counts.entry(*candidate).or_insert(0);
        }
        for state in self.claims.values() {
            if state.turn == turn {
                if let Some(count) = counts.get_mut(&state.proposer) {
                    *count += 1;
                }
            }
        }
        counts.into_iter().min_by_key(|(agent, count)| (*count, agent.to_string())).map(|(agent, _)| agent)
    }

    /// Whether the claim's quorum is still reachable, if the board holds it.
    #[must_use]
    pub fn reachable(&self, claim: ClaimId) -> Option<bool> {
        self.claims.get(&claim).map(|state| state.status != QuorumStatus::Unreachable)
    }
}

fn status_text(status: QuorumStatus) -> &'static str {
    match status {
        QuorumStatus::Open => "open",
        QuorumStatus::Reached => "reached",
        QuorumStatus::Unreachable => "unreachable",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use supra_types::Confidence;

    fn board() -> Blackboard {
        let dir = std::env::temp_dir().join(format!(
            "supra-bb-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.subsec_nanos())
        ));
        std::fs::create_dir_all(&dir).expect("dir");
        let store = Store::open(dir.join("bb.db")).expect("store");
        Blackboard::open(Arc::new(store)).expect("board")
    }

    fn verdict(vote: Vote) -> Verdict {
        Verdict::new(vote, Confidence::High, "checked", Some("tests/x.rs:1".to_owned())).expect("verdict")
    }

    fn ids(n: usize) -> Vec<AgentId> {
        (0..n).map(|_| AgentId::generate()).collect()
    }

    #[test]
    fn quorum_reached_closes_and_persists() {
        let mut bb = board();
        let agents = ids(3);
        let turn = TurnId::generate();
        let claim = bb.publish(turn, agents[0], &agents[1..], "use exact ranges").expect("publish");

        let outcome = bb.vote(claim, agents[1], &verdict(Vote::Yes)).expect("vote");
        assert_eq!(outcome.status, QuorumStatus::Open);
        assert!(!outcome.is_reached());

        let outcome = bb.vote(claim, agents[2], &verdict(Vote::Yes)).expect("vote");
        assert_eq!(outcome.status, QuorumStatus::Reached);
        assert!(outcome.is_reached());

        let error = bb.vote(claim, agents[1], &verdict(Vote::Yes)).expect_err("closed");
        assert!(matches!(error, BlackboardError::Closed { .. }));

        let stored = bb.claim(claim).expect("claim");
        assert_eq!(stored.status, "reached");
        assert_eq!(bb.votes(claim).expect("votes").len(), 2);
    }

    #[test]
    fn proposer_cannot_vote_on_own_claim() {
        let mut bb = board();
        let agents = ids(3);
        let claim = bb.publish(TurnId::generate(), agents[0], &agents[1..], "body").expect("publish");
        let error = bb.vote(claim, agents[0], &verdict(Vote::Yes)).expect_err("own claim");
        assert!(matches!(error, BlackboardError::NotAProposer { .. }));
    }

    #[test]
    fn duplicate_vote_is_refused() {
        let mut bb = board();
        let agents = ids(4);
        let claim = bb.publish(TurnId::generate(), agents[0], &agents[1..], "body").expect("publish");
        bb.vote(claim, agents[1], &verdict(Vote::Yes)).expect("first");
        let error = bb.vote(claim, agents[1], &verdict(Vote::Yes)).expect_err("dup");
        assert!(matches!(error, BlackboardError::DuplicateVote { .. }));
    }

    #[test]
    fn unreachable_abstains_escalate_without_waiting() {
        let mut bb = board();
        let agents = ids(4);
        let claim = bb.publish(TurnId::generate(), agents[0], &agents[1..], "body").expect("publish");
        bb.vote(claim, agents[1], &verdict(Vote::Abstain)).expect("abstain 1");
        let outcome = bb.vote(claim, agents[2], &verdict(Vote::Abstain)).expect("abstain 2");
        assert_eq!(
            outcome.status,
            QuorumStatus::Unreachable,
            "two abstains leave yes+pending = 0+2 < needed 3"
        );
        assert_eq!(outcome.tally.abstain(), 2);
        assert!(!bb.reachable(claim).expect("claim"));
        let error =
            bb.vote(claim, agents[3], &verdict(Vote::Abstain)).expect_err("unreachable closes the claim");
        assert!(matches!(error, BlackboardError::Closed { .. }));
    }

    #[test]
    fn no_votes_outside_the_cohort() {
        let mut bb = board();
        let agents = ids(3);
        let stranger = AgentId::generate();
        let claim = bb.publish(TurnId::generate(), agents[0], &agents[1..], "body").expect("publish");
        let error = bb.vote(claim, stranger, &verdict(Vote::Yes)).expect_err("stranger");
        assert!(matches!(error, BlackboardError::NotInCohort { .. }));
    }

    #[test]
    fn the_tally_spans_the_cohort_not_the_validators() {
        let mut bb = board();
        let agents = ids(3);
        let claim = bb.publish(TurnId::generate(), agents[0], &agents[1..], "body").expect("publish");
        let outcome = bb.vote(claim, agents[1], &verdict(Vote::Yes)).expect("vote");
        assert_eq!(outcome.tally.k(), 3, "the tally spans proposer plus validators");
        assert_eq!(outcome.tally.needed(), supra_types::quorum(3));
        assert_eq!(outcome.status, QuorumStatus::Open, "one yes of two needed is open");
        let outcome = bb.vote(claim, agents[2], &verdict(Vote::Yes)).expect("second");
        assert_eq!(outcome.status, QuorumStatus::Reached);
    }

    #[test]
    fn roles_rotate_across_claims_in_one_turn() {
        let mut bb = board();
        let agents = ids(3);
        let turn = TurnId::generate();
        let first = bb.next_proposer(turn, &agents).expect("first");
        let validators: Vec<AgentId> = agents.iter().copied().filter(|a| *a != first).collect();
        let claim_a = bb.publish(turn, first, &validators, "a").expect("a");
        for voter in &validators {
            bb.vote(claim_a, *voter, &verdict(Vote::Yes)).expect("vote a");
        }
        let second = bb.next_proposer(turn, &agents).expect("second");
        assert_ne!(first, second, "two proposals in one turn must not repeat a proposer");
    }

    #[test]
    fn a_proposer_in_the_validator_list_is_refused_at_publish() {
        let mut bb = board();
        let agents = ids(3);
        let error = bb
            .publish(TurnId::generate(), agents[0], &agents, "body")
            .expect_err("proposer among validators");
        assert!(matches!(error, BlackboardError::NotAProposer { .. }));
    }

    #[test]
    fn an_oversized_body_is_refused_at_publish() {
        let mut bb = board();
        let agents = ids(2);
        let validators: Vec<AgentId> = agents.iter().copied().filter(|a| *a != agents[0]).collect();
        let error = bb
            .publish(TurnId::generate(), agents[0], &validators, &"x".repeat(MAX_BODY_BYTES + 1))
            .expect_err("oversized");
        assert!(matches!(error, BlackboardError::Store(_)), "{error}");
    }

    #[test]
    fn an_oversized_body_is_refused_even_before_the_store() {
        let mut bb = board();
        let agents = ids(2);
        let validators: Vec<AgentId> = agents.iter().copied().filter(|a| *a != agents[0]).collect();
        let oversized = "x".repeat(MAX_BODY_BYTES + 1);
        let claim = bb.publish(TurnId::generate(), agents[0], &validators, &oversized);
        assert!(claim.is_err());
        assert!(bb.claims.is_empty(), "a refused publish leaves no in-memory claim");
    }

    #[test]
    fn k_counts_proposer_plus_validators() {
        let mut bb = board();
        let agents = ids(5);
        let validators: Vec<AgentId> = agents[1..].to_vec();
        let claim = bb.publish(TurnId::generate(), agents[0], &validators, "body").expect("publish");
        let stored = bb.claim(claim).expect("claim");
        assert_eq!(stored.k, 5);
        let outcome = bb.outcome(claim).expect("outcome");
        assert_eq!(outcome.tally.k(), 5);
        assert_eq!(outcome.tally.pending(), 5, "no votes yet, proposer included in the span");
    }
}
