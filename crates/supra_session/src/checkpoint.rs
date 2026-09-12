// Versioned session checkpoint representation.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use supra_types::{SessionId, TurnId};

use crate::SessionError;

/// Current on-disk checkpoint format.
pub const CHECKPOINT_VERSION: u32 = 1;

/// Whether a persisted session may accept more work.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LifecycleStatus {
    /// The session may accept another turn.
    #[default]
    Active,
    /// A turn was started but has not committed an accepted answer.
    Running,
    /// The session ended normally.
    Completed,
    /// The session stopped before completing its current work.
    Interrupted,
}

/// One provider-neutral protocol item, retained without flattening its blocks.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ProtocolMessage {
    payload: Value,
}

impl ProtocolMessage {
    /// Construct a conventional role/content protocol message.
    #[must_use]
    pub fn new(role: impl Into<String>, content: Value) -> Self {
        let mut payload = serde_json::Map::new();
        payload.insert("role".to_owned(), Value::String(role.into()));
        payload.insert("content".to_owned(), content);
        Self { payload: Value::Object(payload) }
    }

    /// Retain a complete provider-neutral message object as received.
    #[must_use]
    pub const fn from_payload(payload: Value) -> Self {
        Self { payload }
    }

    /// The complete message payload, including provider-specific fields.
    #[must_use]
    pub const fn payload(&self) -> &Value {
        &self.payload
    }

    /// The conventional provider role, when the payload carries one.
    #[must_use]
    pub fn role(&self) -> Option<&str> {
        self.payload.get("role").and_then(Value::as_str)
    }

    /// The conventional provider content, when the payload carries it.
    #[must_use]
    pub fn content(&self) -> Option<&Value> {
        self.payload.get("content")
    }
}

/// One completed turn in a versioned checkpoint.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckpointTurn {
    turn: TurnId,
    display_body: String,
    messages: Vec<ProtocolMessage>,
    completion: Option<Value>,
}

impl CheckpointTurn {
    /// Construct a turn record. Protocol messages stay in provider order.
    #[must_use]
    pub fn new(
        turn: TurnId,
        display_body: impl Into<String>,
        messages: Vec<ProtocolMessage>,
        completion: Option<Value>,
    ) -> Self {
        Self { turn, display_body: display_body.into(), messages, completion }
    }

    /// The turn identity.
    #[must_use]
    pub const fn turn(&self) -> TurnId {
        self.turn
    }

    /// The accepted body rendered to the user.
    #[must_use]
    pub fn display_body(&self) -> &str {
        &self.display_body
    }

    /// Ordered provider protocol messages used by this turn.
    #[must_use]
    pub fn messages(&self) -> &[ProtocolMessage] {
        &self.messages
    }

    /// The lossless provider completion payload, when captured.
    #[must_use]
    pub const fn completion(&self) -> Option<&Value> {
        self.completion.as_ref()
    }
}

/// Complete resumable session state.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionCheckpoint {
    version: u32,
    session: SessionId,
    revision: u64,
    status: LifecycleStatus,
    parent: Option<SessionId>,
    parent_revision: Option<u64>,
    turns: Vec<CheckpointTurn>,
    ledger: Option<Value>,
    config: Option<Value>,
}

impl SessionCheckpoint {
    /// Start an empty checkpoint for `session`.
    #[must_use]
    pub fn new(session: SessionId) -> Self {
        Self {
            version: CHECKPOINT_VERSION,
            session,
            revision: 0,
            status: LifecycleStatus::Active,
            parent: None,
            parent_revision: None,
            turns: Vec::new(),
            ledger: None,
            config: None,
        }
    }

    /// Validate and construct a checkpoint from explicit state.
    ///
    /// # Errors
    ///
    /// [`SessionError::Malformed`] when the version, lineage, revision, or turn
    /// identities violate the checkpoint invariants.
    #[allow(clippy::too_many_arguments)]
    pub fn from_parts(
        version: u32,
        session: SessionId,
        revision: u64,
        status: LifecycleStatus,
        parent: Option<SessionId>,
        parent_revision: Option<u64>,
        turns: Vec<CheckpointTurn>,
        ledger: Option<Value>,
        config: Option<Value>,
    ) -> Result<Self, SessionError> {
        let checkpoint =
            Self { version, session, revision, status, parent, parent_revision, turns, ledger, config };
        checkpoint.validate()?;
        Ok(checkpoint)
    }

    /// The file-format version.
    #[must_use]
    pub const fn version(&self) -> u32 {
        self.version
    }

    /// The session named by this checkpoint.
    #[must_use]
    pub const fn session(&self) -> SessionId {
        self.session
    }

    /// Monotonic committed-state revision.
    #[must_use]
    pub const fn revision(&self) -> u64 {
        self.revision
    }

    /// The persisted lifecycle state.
    #[must_use]
    pub const fn status(&self) -> LifecycleStatus {
        self.status
    }

    /// The source session for a branch.
    #[must_use]
    pub const fn parent(&self) -> Option<SessionId> {
        self.parent
    }

    /// The exact source revision a branch copied.
    #[must_use]
    pub const fn parent_revision(&self) -> Option<u64> {
        self.parent_revision
    }

    /// Completed turns in transcript order.
    #[must_use]
    pub fn turns(&self) -> &[CheckpointTurn] {
        &self.turns
    }

    /// Opaque prompt-ledger snapshot, interpreted by `supra_prompt`.
    #[must_use]
    pub const fn ledger(&self) -> Option<&Value> {
        self.ledger.as_ref()
    }

    /// Opaque frozen configuration snapshot, interpreted by its owner.
    #[must_use]
    pub const fn config(&self) -> Option<&Value> {
        self.config.as_ref()
    }

    /// Replace the lifecycle state and advance the revision.
    pub fn set_status(&mut self, status: LifecycleStatus) {
        self.status = status;
        self.advance_revision();
    }

    /// Append a completed turn and advance the revision.
    ///
    /// # Errors
    ///
    /// [`SessionError::Malformed`] when the turn id already exists.
    pub fn push_turn(&mut self, turn: CheckpointTurn) -> Result<(), SessionError> {
        if self.turns.iter().any(|known| known.turn == turn.turn) {
            return Err(SessionError::Malformed(format!("duplicate turn {}", turn.turn)));
        }
        self.turns.push(turn);
        self.advance_revision();
        Ok(())
    }

    /// Replace the opaque ledger snapshot and advance the revision.
    pub fn set_ledger(&mut self, ledger: Option<Value>) {
        self.ledger = ledger;
        self.advance_revision();
    }

    /// Replace the opaque frozen configuration and advance the revision.
    pub fn set_config(&mut self, config: Option<Value>) {
        self.config = config;
        self.advance_revision();
    }

    /// Copy this checkpoint under a fresh session id, retaining explicit lineage.
    #[must_use]
    pub fn branch(&self, session: SessionId) -> Self {
        Self {
            version: CHECKPOINT_VERSION,
            session,
            revision: 0,
            status: LifecycleStatus::Active,
            parent: Some(self.session),
            parent_revision: Some(self.revision),
            turns: self.turns.clone(),
            ledger: self.ledger.clone(),
            config: self.config.clone(),
        }
    }

    pub(crate) fn validate(&self) -> Result<(), SessionError> {
        if self.version != CHECKPOINT_VERSION {
            return Err(SessionError::UnsupportedVersion { found: self.version });
        }
        if self.session.is_nil() {
            return Err(SessionError::Malformed("a checkpoint cannot name the nil session".to_owned()));
        }
        if self.parent.is_some() != self.parent_revision.is_some() {
            return Err(SessionError::Malformed(
                "branch parent and parent revision must be present together".to_owned(),
            ));
        }
        if self.parent == Some(self.session) {
            return Err(SessionError::Malformed("a checkpoint cannot branch from itself".to_owned()));
        }
        let mut seen = BTreeSet::new();
        for turn in &self.turns {
            if turn.turn.is_nil() {
                return Err(SessionError::Malformed("a checkpoint cannot contain a nil turn".to_owned()));
            }
            if !seen.insert(turn.turn) {
                return Err(SessionError::Malformed(format!("duplicate turn {}", turn.turn)));
            }
            for message in &turn.messages {
                if !message.payload.is_object() {
                    return Err(SessionError::Malformed(format!(
                        "turn {} contains a protocol message that is not an object",
                        turn.turn
                    )));
                }
                if message.role().is_some_and(|role| role.trim().is_empty()) {
                    return Err(SessionError::Malformed(format!(
                        "turn {} contains an empty protocol role",
                        turn.turn
                    )));
                }
            }
        }
        Ok(())
    }

    fn advance_revision(&mut self) {
        self.revision = self.revision.saturating_add(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn turn(id: TurnId) -> CheckpointTurn {
        CheckpointTurn::new(
            id,
            "answer",
            vec![ProtocolMessage::new(
                "assistant",
                serde_json::json!([
                    {"type": "thinking", "thinking": "reason", "signature": "sig"},
                    {"type": "text", "text": "answer"}
                ]),
            )],
            Some(serde_json::json!({"stop_reason": "end_turn"})),
        )
    }

    #[test]
    fn protocol_payload_round_trips_without_flattening() {
        let session = SessionId::generate();
        let mut checkpoint = SessionCheckpoint::new(session);
        checkpoint.push_turn(turn(TurnId::generate())).expect("append");
        checkpoint.set_ledger(Some(serde_json::json!({"generation": 3, "segments": [1, 2]})));

        let encoded = serde_json::to_vec(&checkpoint).expect("encode");
        let decoded: SessionCheckpoint = serde_json::from_slice(&encoded).expect("decode");
        decoded.validate().expect("valid");
        assert_eq!(decoded, checkpoint);
        assert_eq!(decoded.turns()[0].messages()[0].content().expect("content")[0]["signature"], "sig");
    }

    #[test]
    fn branch_records_the_exact_source_revision() {
        let mut source = SessionCheckpoint::new(SessionId::generate());
        source.push_turn(turn(TurnId::generate())).expect("append");
        source.set_status(LifecycleStatus::Interrupted);
        let branch = source.branch(SessionId::generate());

        assert_eq!(branch.parent(), Some(source.session()));
        assert_eq!(branch.parent_revision(), Some(source.revision()));
        assert_eq!(branch.revision(), 0);
        assert_eq!(branch.status(), LifecycleStatus::Active);
        assert_eq!(branch.turns(), source.turns());
    }

    #[test]
    fn invalid_lineage_and_duplicate_turns_are_refused() {
        let session = SessionId::generate();
        let turn_id = TurnId::generate();
        let missing_revision = SessionCheckpoint::from_parts(
            CHECKPOINT_VERSION,
            session,
            0,
            LifecycleStatus::Active,
            Some(SessionId::generate()),
            None,
            Vec::new(),
            None,
            None,
        );
        assert!(matches!(missing_revision, Err(SessionError::Malformed(_))));

        let duplicate = SessionCheckpoint::from_parts(
            CHECKPOINT_VERSION,
            session,
            2,
            LifecycleStatus::Active,
            None,
            None,
            vec![turn(turn_id), turn(turn_id)],
            None,
            None,
        );
        assert!(matches!(duplicate, Err(SessionError::Malformed(_))));
    }

    #[test]
    fn unknown_versions_are_refused() {
        let result = SessionCheckpoint::from_parts(
            CHECKPOINT_VERSION + 1,
            SessionId::generate(),
            0,
            LifecycleStatus::Active,
            None,
            None,
            Vec::new(),
            None,
            None,
        );
        assert!(matches!(result, Err(SessionError::UnsupportedVersion { .. })));
    }
}
