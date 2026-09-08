use supra_blackboard::{Blackboard, BlackboardError};
use supra_types::{AgentId, ClaimId, Confidence, TurnId, Verdict, Vote};

use crate::finding::{Finding, Kind};

/// Publish a finding-backed claim to the blackboard, with one validation
/// vote per confirming peer answer.
///
/// The introspector never decides: a finding is a claim, peers vote, and
/// quorum - T21 - accepts or rejects. This function wires the two layers
/// the architecture's step 9 names ("append findings; a confirmed finding
/// escalates").
///
/// # Errors
///
/// Whatever the blackboard refuses.
pub fn append_finding(
    board: &mut Blackboard,
    turn: TurnId,
    proposer: AgentId,
    validators: &[AgentId],
    finding: &Finding,
) -> Result<ClaimId, BlackboardError> {
    let claim = board.publish(turn, proposer, validators, &finding.summary)?;
    Ok(claim)
}

/// Record one validator's verdict on a finding claim, with the finding's
/// own evidence reference attached.
///
/// # Errors
///
/// Whatever the blackboard refuses.
pub fn vote_finding(
    board: &mut Blackboard,
    claim: ClaimId,
    voter: AgentId,
    vote: Vote,
    finding: &Finding,
) -> Result<supra_blackboard::Outcome, BlackboardError> {
    let verdict = Verdict::new(vote, Confidence::High, &finding.summary, Some(finding.evidence_ref()))
        .map_err(|error| {
            BlackboardError::Store(supra_store::StoreError::Malformed { detail: error.to_string() })
        })?;
    board.vote(claim, voter, &verdict)
}

/// Whether a confirmed finding of this kind escalates the tier.
#[must_use]
pub fn escalates(kind: Kind) -> bool {
    matches!(kind, Kind::Static | Kind::Dynamic | Kind::CrossAgent)
}

const _: () = {
    const fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<Finding>();
    assert_send_sync::<Kind>();
};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cross::{Answer, cross_check};
    use std::sync::Arc;

    fn board() -> (Blackboard, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!(
            "supra-intro-bb-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.subsec_nanos())
        ));
        std::fs::create_dir_all(&dir).expect("dir");
        let store = supra_store::Store::open(dir.join("bb.db")).expect("store");
        (Blackboard::open(Arc::new(store)).expect("board"), dir)
    }

    fn peers(n: usize) -> Vec<AgentId> {
        (0..n).map(|_| AgentId::generate()).collect()
    }

    #[test]
    fn a_finding_becomes_a_claim_peers_confirm() {
        let (mut board, _dir) = board();
        let agents = peers(3);
        let turn = TurnId::generate();
        let finding =
            Finding::new(Kind::Static, "clippy", "unused variable y", Some("src/lib.rs".to_owned()), Some(1));

        let claim = append_finding(&mut board, turn, agents[0], &agents[1..], &finding).expect("claim");
        let outcome = vote_finding(&mut board, claim, agents[1], Vote::Yes, &finding).expect("v1");
        assert_eq!(outcome.status, supra_types::QuorumStatus::Open);
        let outcome = vote_finding(&mut board, claim, agents[2], Vote::Yes, &finding).expect("v2");
        assert_eq!(outcome.status, supra_types::QuorumStatus::Reached);
    }

    #[test]
    fn a_bridge_vote_carries_the_findings_evidence_into_the_store() {
        let (mut board, dir) = board();
        let agents = peers(3);
        let finding = Finding::new(
            Kind::Dynamic,
            "cargo-test",
            "assertion failed",
            Some("tests/x.rs".to_owned()),
            Some(7),
        );
        let claim =
            append_finding(&mut board, TurnId::generate(), agents[0], &agents[1..], &finding).expect("claim");
        vote_finding(&mut board, claim, agents[1], Vote::Yes, &finding).expect("vote");

        let store = supra_store::Store::open(dir.join("bb.db")).expect("reopen");
        let evidence: String = store
            .with_connection::<_, supra_store::StoreError>(|connection| {
                connection
                    .query_row(
                        "SELECT evidence FROM bb_vote WHERE claim_id = ?1",
                        [claim.to_string()],
                        |row| row.get(0),
                    )
                    .map_err(|error| supra_store::StoreError::Malformed { detail: error.to_string() })
            })
            .expect("read evidence");
        assert_eq!(evidence, "cargo-test#tests/x.rs:7", "the verdict's evidence is the finding's own");
    }

    #[test]
    fn every_kind_escalates_when_confirmed() {
        assert!(escalates(Kind::Static));
        assert!(escalates(Kind::Dynamic));
        assert!(escalates(Kind::CrossAgent));
    }

    #[test]
    fn a_cross_check_finding_flows_through_the_same_path() {
        let (mut board, _dir) = board();
        let agents = peers(3);
        let answers = vec![
            Answer { agent: agents[1], text: "fix a".to_owned() },
            Answer { agent: agents[2], text: "fix b".to_owned() },
        ];
        let found = cross_check("the fix", &answers);
        assert_eq!(found.len(), 1);

        let claim = append_finding(&mut board, TurnId::generate(), agents[0], &agents[1..], &found[0])
            .expect("claim");
        let outcome = vote_finding(&mut board, claim, agents[1], Vote::Yes, &found[0]).expect("vote");
        assert_eq!(outcome.status, supra_types::QuorumStatus::Open);
        assert_eq!(outcome.tally.k(), 3);
    }
}
