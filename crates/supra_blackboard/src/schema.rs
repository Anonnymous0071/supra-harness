use supra_store::ComponentMigration;

/// This component's name in `schema_component`.
pub const COMPONENT: &str = "blackboard";

const CLAIM_TABLE: &str = "bb_claim";
const VOTE_TABLE: &str = "bb_vote";

/// The blackboard's schema steps, in order.
pub const MIGRATIONS: &[ComponentMigration] = &[ComponentMigration {
    version: 1,
    sql: "
        CREATE TABLE bb_claim (
            claim_id   TEXT PRIMARY KEY NOT NULL
                       CHECK (length(claim_id) = 26),
            turn_id    TEXT NOT NULL
                       CHECK (length(turn_id) = 26),
            proposer   TEXT NOT NULL
                       CHECK (length(proposer) = 26),
            body       TEXT NOT NULL
                       CHECK (length(body) > 0 AND length(body) <= 2048),
            k          INTEGER NOT NULL
                       CHECK (k >= 1 AND k <= 80),
            created_at INTEGER NOT NULL
                       CHECK (created_at >= 0),
            status     TEXT NOT NULL
                       CHECK (status IN ('open', 'reached', 'unreachable'))
        ) STRICT;

        CREATE TABLE bb_vote (
            claim_id TEXT NOT NULL
                     CHECK (length(claim_id) = 26),
            voter    TEXT NOT NULL
                     CHECK (length(voter) = 26),
            vote     TEXT NOT NULL
                     CHECK (vote IN ('yes', 'no', 'abstain')),
            reason   TEXT NOT NULL
                     CHECK (length(reason) <= 140),
            evidence TEXT
                     CHECK (evidence IS NULL OR length(evidence) <= 40),
            PRIMARY KEY (claim_id, voter)
        ) STRICT;

        CREATE INDEX bb_vote_claim ON bb_vote (claim_id);
    ",
}];

/// Insert one claim row.
pub(super) fn insert_claim(
    tx: &rusqlite::Transaction<'_>,
    claim: supra_types::ClaimId,
    turn: supra_types::TurnId,
    proposer: supra_types::AgentId,
    body: &str,
    k: usize,
    status: &str,
) -> Result<(), supra_store::StoreError> {
    tx.prepare_cached(&format!(
        "INSERT INTO {CLAIM_TABLE} (claim_id, turn_id, proposer, body, k, created_at, status) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)"
    ))?
    .execute(rusqlite::params![
        claim.to_string(),
        turn.to_string(),
        proposer.to_string(),
        body,
        i64::try_from(k).unwrap_or(i64::MAX),
        now_ms(),
        status
    ])?;
    Ok(())
}

/// Close a claim row under a terminal status. Returns whether a row changed.
pub(super) fn update_status(
    tx: &rusqlite::Transaction<'_>,
    claim: supra_types::ClaimId,
    status: &str,
) -> Result<bool, supra_store::StoreError> {
    let updated = tx
        .prepare_cached(&format!(
            "UPDATE {CLAIM_TABLE} SET status = ?2 WHERE claim_id = ?1 AND status = 'open'"
        ))?
        .execute([claim.to_string(), status.to_owned()])?;
    Ok(updated == 1)
}

/// Insert one vote row.
pub(super) fn insert_vote(
    tx: &rusqlite::Transaction<'_>,
    claim: supra_types::ClaimId,
    voter: supra_types::AgentId,
    verdict: &supra_types::Verdict,
) -> Result<(), supra_store::StoreError> {
    tx.prepare_cached(&format!(
        "INSERT INTO {VOTE_TABLE} (claim_id, voter, vote, reason, evidence) VALUES (?1, ?2, ?3, ?4, ?5)"
    ))?
    .execute(rusqlite::params![
        claim.to_string(),
        voter.to_string(),
        vote_text(verdict.vote()),
        verdict.reason(),
        verdict.evidence_ref(),
    ])?;
    Ok(())
}

/// One claim row as the board reads it back.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StoredClaim {
    /// The claim's id.
    pub claim: supra_types::ClaimId,
    /// The turn the claim belongs to.
    pub turn: supra_types::TurnId,
    /// The claim's proposer.
    pub proposer: supra_types::AgentId,
    /// The claim's body.
    pub body: String,
    /// The cohort size the claim was published with.
    pub k: usize,
    /// The stored status text.
    pub status: String,
}

/// One vote row as the board reads it back.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StoredVote {
    /// The claim voted on.
    pub claim: supra_types::ClaimId,
    /// The voter.
    pub voter: supra_types::AgentId,
    /// The vote cast.
    pub vote: supra_types::Vote,
}

fn vote_text(vote: supra_types::Vote) -> &'static str {
    match vote {
        supra_types::Vote::Yes => "yes",
        supra_types::Vote::No => "no",
        supra_types::Vote::Abstain => "abstain",
    }
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| i64::try_from(elapsed.as_millis()).unwrap_or(i64::MAX))
}
