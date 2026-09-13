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

fn ordinary_tally(k: usize) -> QuorumTally {
    let mut tally = QuorumTally::new(k);
    // Publishing a candidate is the proposer's affirmative vote. It is not a
    // validator row: ordinary claims forbid the proposer from voting again.
    let recorded = tally.record(Vote::Yes);
    debug_assert_eq!(recorded, Ok(if k == 1 { QuorumStatus::Reached } else { QuorumStatus::Open }));
    tally
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
    /// Every stored claim is hydrated with its validator roster and the
    /// votes it has already collected, so a restart resumes the tally
    /// instead of forgetting the claim.
    ///
    /// # Errors
    ///
    /// [`BlackboardError::Store`] when the component migration fails or
    /// the stored rows do not parse.
    pub fn open(store: Arc<Store>) -> Result<Self, BlackboardError> {
        store.migrate_component(COMPONENT, MIGRATIONS)?;
        let mut claims = BTreeMap::new();
        store.with_transaction::<_, BlackboardError>(TransactionBehavior::Deferred, |tx| {
            for (stored, validators, votes) in schema::read_all(tx)? {
                let k = stored.k.max(1);
                let blind_rederivation =
                    k == 1 && validators.len() == 1 && validators.first() == Some(&stored.proposer);
                let mut tally = if blind_rederivation { QuorumTally::new(k) } else { ordinary_tally(k) };
                let mut state = ClaimState {
                    turn: stored.turn,
                    proposer: stored.proposer,
                    body: stored.body,
                    k,
                    tally,
                    status: QuorumStatus::Open,
                    validators: validators.iter().copied().map(|id| (id, None)).collect(),
                };
                let mut inconsistent = false;
                for vote in votes {
                    match state.validators.get_mut(&vote.voter) {
                        Some(slot) if slot.is_none() => {
                            *slot = Some(vote.vote);
                            if tally.record(vote.vote).is_err() {
                                inconsistent = true;
                            }
                        }
                        _ => inconsistent = true,
                    }
                }
                if inconsistent {
                    return Err(BlackboardError::Store(supra_store::StoreError::Malformed {
                        detail: format!(
                            "claim {} has a roster or tally the store cannot reproduce",
                            stored.claim
                        ),
                    }));
                }
                state.status = match stored.status.as_str() {
                    "open" => tally.status(),
                    "reached" => QuorumStatus::Reached,
                    "unreachable" => QuorumStatus::Unreachable,
                    other => {
                        return Err(BlackboardError::Store(supra_store::StoreError::Malformed {
                            detail: format!("claim {} has unknown status {other:?}", stored.claim),
                        }));
                    }
                };
                state.tally = tally;
                claims.insert(stored.claim, state);
            }
            Ok(())
        })?;
        Ok(Self { store, claims })
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
            schema::insert_claim(tx, claim, turn, proposer, body, k, "open", validators)?;
            Ok(())
        })?;
        self.claims.insert(
            claim,
            ClaimState {
                turn,
                proposer,
                body: body.to_owned(),
                k,
                tally: ordinary_tally(k),
                status: QuorumStatus::Open,
                validators: validators.iter().copied().map(|id| (id, None)).collect(),
            },
        );
        Ok(claim)
    }

    /// Publish the bounded placeholder used by an E0 blind re-derivation.
    ///
    /// The proposer is persisted as the sole eligible validation pass while `k`
    /// remains one, so quorum arithmetic keeps its documented `ceil(2k/3)` meaning.
    ///
    /// # Errors
    ///
    /// As [`Self::publish`].
    pub fn publish_blind(
        &mut self,
        turn: TurnId,
        proposer: AgentId,
        body: &str,
    ) -> Result<ClaimId, BlackboardError> {
        if body.len() > MAX_BODY_BYTES {
            return Err(BlackboardError::Store(supra_store::StoreError::Malformed {
                detail: format!("claim body is {} bytes, the budget is {MAX_BODY_BYTES}", body.len()),
            }));
        }
        let claim = ClaimId::generate();
        self.store.with_transaction::<_, BlackboardError>(TransactionBehavior::Immediate, |tx| {
            schema::insert_claim(tx, claim, turn, proposer, body, 1, "open", &[proposer])?;
            Ok(())
        })?;
        self.claims.insert(
            claim,
            ClaimState {
                turn,
                proposer,
                body: body.to_owned(),
                k: 1,
                tally: QuorumTally::new(1),
                status: QuorumStatus::Open,
                validators: [(proposer, None)].into_iter().collect(),
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
        Self::record_vote(&self.store, claim, voter, verdict, state)
    }

    /// Record the E0 proposer's blind, reasoning-free second derivation.
    ///
    /// This is the only path where the proposer is also an eligible validator. The
    /// roster must contain exactly that proposer, so callers cannot use it to widen
    /// voting authority for an ordinary cohort.
    ///
    /// # Errors
    ///
    /// As [`Self::vote`], plus [`BlackboardError::NotInCohort`] unless this is the
    /// single-member blind-rederivation roster.
    pub fn vote_blind(
        &mut self,
        claim: ClaimId,
        voter: AgentId,
        verdict: &Verdict,
    ) -> Result<Outcome, BlackboardError> {
        let state = self.claims.get_mut(&claim).ok_or(BlackboardError::UnknownClaim { claim })?;
        if state.status != QuorumStatus::Open {
            return Err(BlackboardError::Closed { claim });
        }
        if state.proposer != voter || state.k != 1 || state.validators.len() != 1 {
            return Err(BlackboardError::NotInCohort { agent: voter, claim });
        }
        Self::record_vote(&self.store, claim, voter, verdict, state)
    }

    fn record_vote(
        store: &Store,
        claim: ClaimId,
        voter: AgentId,
        verdict: &Verdict,
        state: &mut ClaimState,
    ) -> Result<Outcome, BlackboardError> {
        let Some(slot) = state.validators.get_mut(&voter) else {
            return Err(BlackboardError::NotInCohort { agent: voter, claim });
        };
        if slot.is_some() {
            return Err(BlackboardError::DuplicateVote { agent: voter, claim });
        }

        // Evaluate against a copy before opening the transaction. Memory changes
        // only after the vote and any terminal status transition commit together.
        let vote = verdict.vote();
        let mut next_tally = state.tally;
        let outcome = next_tally.record(vote)?;
        store.with_transaction::<_, BlackboardError>(TransactionBehavior::Immediate, |tx| {
            schema::insert_vote(tx, claim, voter, verdict)?;
            if outcome != QuorumStatus::Open && !schema::update_status(tx, claim, status_text(outcome))? {
                return Err(BlackboardError::Store(supra_store::StoreError::Malformed {
                    detail: format!("claim {claim} closed concurrently while recording its terminal vote"),
                }));
            }
            Ok(())
        })?;

        *slot = Some(vote);
        state.tally = next_tally;
        state.status = outcome;

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

    /// Every claim id the board holds, in id order.
    #[must_use]
    pub fn all_claims(&self) -> Vec<ClaimId> {
        self.claims.keys().copied().collect()
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

    fn scratch_tag() -> String {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash as _, Hasher as _};
        let mut hasher = DefaultHasher::new();
        std::process::id().hash(&mut hasher);
        std::thread::current().id().hash(&mut hasher);
        std::time::SystemTime::now().hash(&mut hasher);
        let slot = 0u8;
        std::ptr::from_ref(&slot).hash(&mut hasher);
        format!("{:016x}", hasher.finish())
    }

    fn board() -> Blackboard {
        let dir = std::env::temp_dir().join(format!("supra-bb-{}", scratch_tag()));
        if std::fs::create_dir_all(&dir).is_err() {
            let dir = std::env::temp_dir().join(format!("supra-bb-{}", scratch_tag()));
            std::fs::create_dir_all(&dir).expect("dir");
            let store = Store::open(dir.join("bb.db")).expect("store");
            return Blackboard::open(Arc::new(store)).expect("board");
        }
        let store = Store::open(dir.join("bb.db")).expect("store");
        Blackboard::open(Arc::new(store)).expect("board")
    }

    fn verdict(vote: Vote) -> Verdict {
        Verdict::new(vote, Confidence::High, "checked", Some("tests/x.rs:1".to_owned())).expect("verdict")
    }

    fn store_path() -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("supra-bb-{}", scratch_tag()));
        if std::fs::create_dir_all(&dir).is_err() {
            let dir = std::env::temp_dir().join(format!("supra-bb-{}", scratch_tag()));
            std::fs::create_dir_all(&dir).expect("dir");
            return dir.join("bb.db");
        }
        dir.join("bb.db")
    }

    #[test]
    fn a_reopened_board_hydrates_claims_validators_and_votes() {
        let path = store_path();
        let proposer = AgentId::generate();
        let validators = ids(3);

        {
            let store = Store::open(&path).expect("store");
            let mut board = Blackboard::open(Arc::new(store)).expect("board");
            let claim =
                board.publish(TurnId::generate(), proposer, &validators, "the task").expect("publish");
            board.vote(claim, validators[0], &verdict(Vote::Yes)).expect("vote");
        }

        let store = Store::open(&path).expect("reopen");
        let mut board = Blackboard::open(Arc::new(store)).expect("hydrate");
        let claims: Vec<_> = board.all_claims();
        assert_eq!(claims.len(), 1, "the claim survives the restart");
        let claim = claims[0];
        assert_eq!(board.claim(claim).expect("stored").body, "the task");
        assert_eq!(board.votes(claim).expect("votes").len(), 1, "the vote survives");
        assert!(board.reachable(claim).expect("reachable"), "an open claim with two yes votes is still live");
        board.vote(claim, validators[1], &verdict(Vote::Yes)).expect("the roster survived too");
        assert!(board.outcome(claim).expect("outcome").is_reached(), "quorum across the restart");
    }

    fn ids(n: usize) -> Vec<AgentId> {
        (0..n).map(|_| AgentId::generate()).collect()
    }

    #[test]
    fn a_blind_claim_survives_restart_with_its_single_validator() {
        let path = store_path();
        let proposer = AgentId::generate();
        let claim;

        {
            let store = Store::open(&path).expect("store");
            let mut board = Blackboard::open(Arc::new(store)).expect("board");
            claim = board.publish_blind(TurnId::generate(), proposer, "candidate").expect("blind publish");
        }

        let store = Store::open(&path).expect("reopen");
        let mut board = Blackboard::open(Arc::new(store)).expect("hydrate");
        let outcome = board.vote_blind(claim, proposer, &verdict(Vote::Yes)).expect("persisted blind roster");
        assert_eq!(outcome.status, QuorumStatus::Reached);
        assert_eq!(board.votes(claim).expect("votes").len(), 1);
    }

    #[test]
    fn an_oversized_blind_claim_leaves_no_state() {
        let mut board = board();
        let result =
            board.publish_blind(TurnId::generate(), AgentId::generate(), &"x".repeat(MAX_BODY_BYTES + 1));
        assert!(result.is_err());
        assert!(board.claims.is_empty());
    }

    #[test]
    fn quorum_reached_closes_and_persists() {
        let mut bb = board();
        let agents = ids(3);
        let turn = TurnId::generate();
        let claim = bb.publish(turn, agents[0], &agents[1..], "use exact ranges").expect("publish");

        let outcome = bb.vote(claim, agents[1], &verdict(Vote::Yes)).expect("vote");
        assert_eq!(outcome.status, QuorumStatus::Reached);
        assert!(outcome.is_reached());

        let error = bb.vote(claim, agents[2], &verdict(Vote::Yes)).expect_err("closed");
        assert!(matches!(error, BlackboardError::Closed { .. }));

        let stored = bb.claim(claim).expect("claim");
        assert_eq!(stored.status, "reached");
        assert_eq!(bb.votes(claim).expect("votes").len(), 1);
    }

    #[test]
    fn a_terminal_vote_rolls_back_when_status_persistence_fails() {
        let mut bb = board();
        let agents = ids(2);
        let claim = bb.publish(TurnId::generate(), agents[0], &agents[1..], "body").expect("publish");
        bb.store
            .with_transaction::<_, BlackboardError>(TransactionBehavior::Immediate, |tx| {
                tx.execute_batch(
                    "CREATE TRIGGER refuse_terminal_status
                     BEFORE UPDATE OF status ON bb_claim
                     WHEN NEW.status != 'open'
                     BEGIN SELECT RAISE(ABORT, 'injected status failure'); END;",
                )?;
                Ok(())
            })
            .expect("install failure injection");

        let error = bb.vote(claim, agents[1], &verdict(Vote::Yes)).expect_err("status write must fail");
        assert!(matches!(error, BlackboardError::Store(_) | BlackboardError::Sqlite(_)), "{error}");
        let outcome = bb.outcome(claim).expect("outcome");
        assert_eq!(outcome.tally.yes(), 1, "memory keeps only the proposer vote");
        assert_eq!(outcome.status, QuorumStatus::Open);
        assert!(bb.votes(claim).expect("votes").is_empty(), "the vote insert rolled back with status");

        bb.store
            .with_transaction::<_, BlackboardError>(TransactionBehavior::Immediate, |tx| {
                tx.execute_batch("DROP TRIGGER refuse_terminal_status")?;
                Ok(())
            })
            .expect("remove failure injection");
        let retried = bb.vote(claim, agents[1], &verdict(Vote::Yes)).expect("retry");
        assert_eq!(retried.status, QuorumStatus::Reached);
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
        assert_eq!(outcome.tally.yes(), 2, "publishing is the proposer's implicit yes vote");
        assert_eq!(outcome.status, QuorumStatus::Reached, "the proposer and one validator form quorum");
    }

    #[test]
    fn a_two_peer_claim_can_reach_its_two_vote_quorum() {
        let mut bb = board();
        let agents = ids(2);
        let claim = bb.publish(TurnId::generate(), agents[0], &agents[1..], "body").expect("publish");
        let before = bb.outcome(claim).expect("outcome");
        assert_eq!(before.tally.yes(), 1, "the proposer approves the candidate it published");
        assert_eq!(before.tally.pending(), 1);
        assert_eq!(before.status, QuorumStatus::Open);

        let outcome = bb.vote(claim, agents[1], &verdict(Vote::Yes)).expect("validator");
        assert_eq!(outcome.tally.yes(), 2);
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
        bb.vote(claim_a, validators[0], &verdict(Vote::Yes)).expect("vote a");
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
        assert_eq!(outcome.tally.yes(), 1, "the proposer implicitly approves its candidate");
        assert_eq!(outcome.tally.pending(), 4, "only validators remain in flight");
    }
}
