use supra_store::ComponentMigration;

/// This component's name in `schema_component`.
pub const COMPONENT: &str = "blackboard";

const CLAIM_TABLE: &str = "bb_claim";
const VOTE_TABLE: &str = "bb_vote";
const VALIDATOR_TABLE: &str = "bb_validator";

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

        CREATE TABLE bb_validator (
            claim_id TEXT NOT NULL
                     CHECK (length(claim_id) = 26),
            agent    TEXT NOT NULL
                     CHECK (length(agent) = 26),
            PRIMARY KEY (claim_id, agent)
        ) STRICT;
    ",
}];

/// Insert one claim row with its validator roster.
pub(super) fn insert_claim(
    tx: &rusqlite::Transaction<'_>,
    claim: supra_types::ClaimId,
    turn: supra_types::TurnId,
    proposer: supra_types::AgentId,
    body: &str,
    k: usize,
    status: &str,
    validators: &[supra_types::AgentId],
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
    let mut insert_validator =
        tx.prepare_cached(&format!("INSERT INTO {VALIDATOR_TABLE} (claim_id, agent) VALUES (?1, ?2)"))?;
    for agent in validators {
        insert_validator.execute(rusqlite::params![claim.to_string(), agent.to_string()])?;
    }
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

/// Remove a vote row, undoing an insert whose tally refused.
pub(super) fn delete_vote(
    tx: &rusqlite::Transaction<'_>,
    claim: supra_types::ClaimId,
    voter: supra_types::AgentId,
) -> Result<(), supra_store::StoreError> {
    tx.prepare_cached(&format!("DELETE FROM {VOTE_TABLE} WHERE claim_id = ?1 AND voter = ?2"))?
        .execute(rusqlite::params![claim.to_string(), voter.to_string()])?;
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

fn vote_from_text(text: &str) -> Option<supra_types::Vote> {
    match text {
        "yes" => Some(supra_types::Vote::Yes),
        "no" => Some(supra_types::Vote::No),
        "abstain" => Some(supra_types::Vote::Abstain),
        _ => None,
    }
}

/// Read every stored claim with its validators and votes, in id order.
pub(super) fn read_all(
    tx: &rusqlite::Transaction<'_>,
) -> Result<Vec<(StoredClaim, Vec<supra_types::AgentId>, Vec<StoredVote>)>, supra_store::StoreError> {
    let mut statement = tx.prepare_cached(&format!(
        "SELECT claim_id, turn_id, proposer, body, k, status FROM {CLAIM_TABLE} ORDER BY claim_id"
    ))?;
    let rows = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, i64>(4)?,
                row.get::<_, String>(5)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;

    let mut claims = Vec::with_capacity(rows.len());
    for (claim_id, turn_id, proposer, body, k, status) in rows {
        let claim = supra_types::ClaimId::try_from(claim_id.clone())
            .map_err(|_| supra_store::StoreError::Malformed { detail: format!("bad claim id {claim_id}") })?;
        let turn = supra_types::TurnId::try_from(turn_id.clone())
            .map_err(|_| supra_store::StoreError::Malformed { detail: format!("bad turn id {turn_id}") })?;
        let proposer = supra_types::AgentId::try_from(proposer.clone()).map_err(|_| {
            supra_store::StoreError::Malformed { detail: format!("bad proposer id {proposer}") }
        })?;

        let mut validators_statement = tx.prepare_cached(&format!(
            "SELECT agent FROM {VALIDATOR_TABLE} WHERE claim_id = ?1 ORDER BY agent"
        ))?;
        let validators = validators_statement
            .query_map([&claim_id], |row| row.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?
            .into_iter()
            .map(|agent| {
                supra_types::AgentId::try_from(agent.clone()).map_err(|_| {
                    supra_store::StoreError::Malformed { detail: format!("bad agent id {agent}") }
                })
            })
            .collect::<Result<Vec<_>, _>>()?;

        let mut votes_statement = tx.prepare_cached(&format!(
            "SELECT voter, vote FROM {VOTE_TABLE} WHERE claim_id = ?1 ORDER BY voter"
        ))?;
        let votes = votes_statement
            .query_map([&claim_id], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)))?
            .collect::<Result<Vec<_>, _>>()?
            .into_iter()
            .map(|(voter, vote)| {
                let voter = supra_types::AgentId::try_from(voter.clone()).map_err(|_| {
                    supra_store::StoreError::Malformed { detail: format!("bad voter id {voter}") }
                })?;
                let vote = vote_from_text(&vote).ok_or_else(|| supra_store::StoreError::Malformed {
                    detail: format!("bad vote {vote}"),
                })?;
                Ok(StoredVote { claim, voter, vote })
            })
            .collect::<Result<Vec<_>, supra_store::StoreError>>()?;

        let stored = StoredClaim { claim, turn, proposer, body, k: usize::try_from(k).unwrap_or(0), status };
        claims.push((stored, validators, votes));
    }
    Ok(claims)
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| i64::try_from(elapsed.as_millis()).unwrap_or(i64::MAX))
}
