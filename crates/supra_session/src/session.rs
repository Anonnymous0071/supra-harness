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
    pub fn record_turn(&mut self, turn: supra_types::TurnId) {
        self.turns.push(turn);
        self.bodies.push(String::new());
    }

    /// Record one completed turn with the body its ledger segment
    /// sealed, so a later resume-export round trip keeps it.
    pub fn record_turn_with_body(&mut self, turn: supra_types::TurnId, body: impl Into<String>) {
        self.turns.push(turn);
        self.bodies.push(body.into());
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
    /// checkpoint has always held.
    ///
    /// # Errors
    ///
    /// Whatever the store refuses.
    pub fn save(&self, directory: &Path) -> Result<(), SessionError> {
        let turns: Vec<(supra_types::TurnId, String)> =
            self.turns.iter().zip(&self.bodies).map(|(turn, body)| (*turn, body.clone())).collect();
        store::save(directory, self.id, &turns)
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
}
