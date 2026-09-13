use std::path::Path;

use supra_types::SessionId;

use crate::error::SessionError;
use crate::store;

/// A session's exported shape: everything a resume needs and nothing a
/// prefix would hash differently for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Session {
    id: SessionId,
    turns: Vec<supra_types::TurnId>,
    bodies: Vec<String>,
}

impl Session {
    /// Start a fresh session.
    #[must_use]
    pub fn new() -> Self {
        Self { id: SessionId::generate(), turns: Vec::new(), bodies: Vec::new() }
    }

    /// The session's id.
    #[must_use]
    pub const fn id(&self) -> SessionId {
        self.id
    }

    /// Record one completed turn.
    ///
    /// Returns `false` without changing the session when `turn` is nil or was
    /// already recorded. Existing callers may continue to ignore the return
    /// value; callers that ingest external ids can use it to reject duplicates.
    pub fn record_turn(&mut self, turn: supra_types::TurnId) -> bool {
        if turn.is_nil() || self.turns.contains(&turn) {
            return false;
        }
        self.turns.push(turn);
        self.bodies.push(String::new());
        true
    }

    /// Record one completed turn with the body its ledger segment
    /// sealed, so a later resume-export round trip keeps it.
    ///
    /// Returns `false` without changing the session when `turn` is nil or was
    /// already recorded.
    pub fn record_turn_with_body(&mut self, turn: supra_types::TurnId, body: impl Into<String>) -> bool {
        if turn.is_nil() || self.turns.contains(&turn) {
            return false;
        }
        self.turns.push(turn);
        self.bodies.push(body.into());
        true
    }

    /// The completed turns, in order.
    #[must_use]
    pub fn turns(&self) -> &[supra_types::TurnId] {
        &self.turns
    }

    /// The stored bodies, in turn order, for
    /// [`Session::export_markdown`].
    #[must_use]
    pub fn bodies(&self) -> &[String] {
        &self.bodies
    }

    /// Persist under `directory`, publishing the resume event.
    ///
    /// The bodies a resume read back are written out again, so a
    /// save-after-resume is a round trip rather than an erasure; a
    /// turn recorded without one persists an empty body, the shape the
    /// checkpoint has always held. When the file is already a lossless
    /// checkpoint, structured fields are retained and only compatible appended
    /// legacy turns are merged.
    ///
    /// # Errors
    ///
    /// Whatever the store refuses.
    pub fn save(&self, directory: &Path) -> Result<(), SessionError> {
        let turns: Vec<(supra_types::TurnId, String)> =
            self.turns.iter().zip(&self.bodies).map(|(turn, body)| (*turn, body.clone())).collect();
        store::save(directory, self.id, &turns)
    }

    /// Build an additive, lossless checkpoint from this legacy session view.
    ///
    /// Existing turns retain their accepted display bodies. Callers that own
    /// provider protocol blocks can replace these records with
    /// [`CheckpointTurn`](crate::CheckpointTurn)s before publication.
    ///
    /// # Errors
    ///
    /// Returns the checkpoint validation error rather than dropping a turn if
    /// an invalid legacy `Session` is ever produced by a future constructor.
    pub fn checkpoint(&self) -> Result<crate::SessionCheckpoint, SessionError> {
        let mut checkpoint = crate::SessionCheckpoint::new(self.id);
        for (turn, body) in self.turns.iter().zip(&self.bodies) {
            checkpoint.push_turn(crate::CheckpointTurn::new(*turn, body.clone(), Vec::new(), None))?;
        }
        Ok(checkpoint)
    }

    /// Persist a caller-supplied lossless checkpoint for this session.
    ///
    /// # Errors
    ///
    /// [`SessionError::Malformed`] when the checkpoint names another session;
    /// otherwise whatever the checkpoint store refuses.
    pub fn save_checkpoint(
        &self,
        directory: &Path,
        checkpoint: &crate::SessionCheckpoint,
        expected_revision: Option<u64>,
    ) -> Result<(), SessionError> {
        if checkpoint.session() != self.id {
            return Err(SessionError::Malformed(format!(
                "checkpoint names session {} but this session is {}",
                checkpoint.session(),
                self.id
            )));
        }
        store::save_checkpoint(directory, checkpoint, expected_revision)
    }

    /// Resume a persisted session, or `None` when it was never saved.
    ///
    /// # Errors
    ///
    /// Whatever the store refuses.
    pub fn resume(directory: &Path, id: SessionId) -> Result<Option<Self>, SessionError> {
        let Some(turns) = store::load(directory, id)? else { return Ok(None) };
        let mut ids = Vec::with_capacity(turns.len());
        let mut bodies = Vec::with_capacity(turns.len());
        for (turn, body) in turns {
            ids.push(turn);
            bodies.push(body);
        }
        Ok(Some(Self { id, turns: ids, bodies }))
    }

    /// Resume the lossless checkpoint and its legacy `Session` projection.
    ///
    /// Legacy tuple files are upgraded in memory. Structured provider payloads
    /// remain available in the returned checkpoint while the `Session` keeps
    /// the accepted display bodies used by existing call sites.
    ///
    /// # Errors
    ///
    /// Whatever the checkpoint store refuses.
    pub fn resume_checkpoint(
        directory: &Path,
        id: SessionId,
    ) -> Result<Option<(Self, crate::SessionCheckpoint)>, SessionError> {
        let Some(checkpoint) = store::load_checkpoint(directory, id)? else { return Ok(None) };
        let mut turns = Vec::with_capacity(checkpoint.turns().len());
        let mut bodies = Vec::with_capacity(checkpoint.turns().len());
        for turn in checkpoint.turns() {
            turns.push(turn.turn());
            bodies.push(turn.display_body().to_owned());
        }
        Ok(Some((Self { id, turns, bodies }, checkpoint)))
    }

    /// Branch: a new session id carrying this session's turns. The
    /// original file is untouched - a branch is a copy, not a move.
    #[must_use]
    pub fn branch(&self) -> Self {
        Self { id: SessionId::generate(), turns: self.turns.clone(), bodies: self.bodies.clone() }
    }

    /// Export the transcript as Markdown: one heading per turn.
    #[must_use]
    pub fn export_markdown(&self) -> String {
        let mut out = String::from("# Session\n");
        for (turn, body) in self.turns.iter().zip(&self.bodies) {
            out.push_str("\n## Turn ");
            out.push_str(&turn.to_string());
            out.push_str("\n\n");
            out.push_str(body);
            out.push('\n');
        }
        out
    }

    /// Export the transcript against externally supplied bodies, for a
    /// caller that held them outside the session.
    #[must_use]
    pub fn export_markdown_with(&self, bodies: &[(supra_types::TurnId, String)]) -> String {
        let mut out = String::from("# Session\n");
        for turn in &self.turns {
            out.push_str("\n## Turn ");
            out.push_str(&turn.to_string());
            out.push_str("\n\n");
            if let Some((_, body)) = bodies.iter().find(|(known, _)| known == turn) {
                out.push_str(body);
            }
            out.push('\n');
        }
        out
    }
}

impl Default for Session {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch() -> std::path::PathBuf {
        let mut dir = std::env::temp_dir();
        dir.push(format!("supra-session-l2-{}", std::process::id()));
        dir.push(supra_types::SessionId::generate().to_string());
        std::fs::create_dir_all(&dir).expect("dir");
        dir
    }

    #[test]
    fn a_session_round_trips_through_save_and_resume() {
        let dir = scratch();
        let mut session = Session::new();
        let turn = supra_types::TurnId::generate();
        session.record_turn(turn);
        session.save(&dir).expect("save");

        let resumed = Session::resume(&dir, session.id()).expect("resume").expect("present");
        assert_eq!(resumed.id(), session.id());
        assert_eq!(resumed.turns(), &[turn]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_branch_carries_the_turns_but_not_the_id() {
        let mut session = Session::new();
        let turn = supra_types::TurnId::generate();
        session.record_turn(turn);
        let branched = session.branch();
        assert_ne!(branched.id(), session.id());
        assert_eq!(branched.turns(), &[turn]);
    }

    #[test]
    fn a_branch_does_not_touch_the_original_file() {
        let dir = scratch();
        let mut session = Session::new();
        session.record_turn(supra_types::TurnId::generate());
        session.save(&dir).expect("save");
        let branched = session.branch();
        branched.save(&dir).expect("branch save");

        let original = Session::resume(&dir, session.id()).expect("resume").expect("present");
        assert_eq!(original.turns().len(), 1);
        let listed = store::list(&dir).expect("list");
        assert_eq!(listed.len(), 2, "two files: the original and the branch");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_unknown_session_resumes_as_none() {
        let dir = scratch();
        let resumed = Session::resume(&dir, SessionId::generate()).expect("resume");
        assert!(resumed.is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn export_renders_one_heading_per_turn() {
        let mut session = Session::new();
        let turn = supra_types::TurnId::generate();
        session.record_turn_with_body(turn, "the answer");
        let markdown = session.export_markdown();
        assert!(markdown.starts_with("# Session"));
        assert!(markdown.contains(&format!("## Turn {turn}")));
        assert!(markdown.contains("the answer"));
    }

    #[test]
    fn bodies_survive_a_resume_and_a_save_after_it() {
        let dir = scratch();
        let turn = supra_types::TurnId::generate();
        let id = {
            let mut session = Session::new();
            session.record_turn_with_body(turn, "the sealed body");
            let id = session.id();
            session.save(&dir).expect("first save");
            id
        };

        let resumed = Session::resume(&dir, id).expect("resume").expect("present");
        assert_eq!(resumed.turns(), &[turn]);
        assert_eq!(resumed.bodies(), &["the sealed body".to_owned()]);

        resumed.save(&dir).expect("save after resume");
        let again = store::load(&dir, id).expect("reload").expect("present");
        assert_eq!(
            again,
            vec![(turn, "the sealed body".to_owned())],
            "a save after resume keeps the bodies rather than erasing them"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_rich_checkpoint_survives_legacy_resume_and_save() {
        let dir = scratch();
        let session = Session::new();
        let turn = supra_types::TurnId::generate();
        let mut checkpoint = crate::SessionCheckpoint::new(session.id());
        checkpoint
            .push_turn(crate::CheckpointTurn::new(
                turn,
                "accepted",
                vec![crate::ProtocolMessage::from_payload(serde_json::json!({
                    "role": "assistant",
                    "content": [{"type": "thinking", "signature": "opaque"}],
                    "provider_field": 7
                }))],
                Some(serde_json::json!({"usage": {"input": 2}})),
            ))
            .expect("turn");
        checkpoint.set_ledger(Some(serde_json::json!({"generation": 4}))).expect("ledger");
        checkpoint.set_config(Some(serde_json::json!({"model": "frozen"}))).expect("config");
        session.save_checkpoint(&dir, &checkpoint, Some(0)).expect("rich save");

        let resumed = Session::resume(&dir, session.id()).expect("resume").expect("present");
        resumed.save(&dir).expect("legacy-compatible save");

        let restored = store::load_checkpoint(&dir, session.id()).expect("load").expect("present");
        assert_eq!(restored, checkpoint);
        assert_eq!(restored.turns()[0].messages()[0].payload()["provider_field"], 7);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn duplicate_turns_are_rejected_without_changing_the_first_record() {
        let mut session = Session::new();
        let turn = supra_types::TurnId::generate();

        assert!(session.record_turn_with_body(turn, "first"));
        assert!(!session.record_turn_with_body(turn, "replacement"));
        assert!(!session.record_turn(turn));
        assert_eq!(session.turns(), &[turn]);
        assert_eq!(session.bodies(), &["first".to_owned()]);

        let checkpoint = session.checkpoint().expect("unique session converts");
        assert_eq!(checkpoint.turns().len(), 1);
        assert_eq!(checkpoint.turns()[0].turn(), turn);
        assert_eq!(checkpoint.turns()[0].display_body(), "first");
    }
}
