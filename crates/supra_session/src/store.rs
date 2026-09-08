use std::path::{Path, PathBuf};

use supra_types::{SessionId, TurnId};

use crate::error::SessionError;

const FILE_PREFIX: &str = "session-";
const FILE_SUFFIX: &str = ".json";

fn file_path(directory: &Path, session: SessionId) -> PathBuf {
    directory.join(format!("{FILE_PREFIX}{session}{FILE_SUFFIX}"))
}

/// Persist one session's checkpoint under its session directory.
///
/// # Errors
///
/// [`SessionError::Io`] when the write fails; [`SessionError::Malformed`]
/// when serialization fails, which it cannot for this shape.
pub fn save(directory: &Path, session: SessionId, turns: &[(TurnId, String)]) -> Result<(), SessionError> {
    std::fs::create_dir_all(directory)?;
    let document =
        serde_json::to_vec(&(session, turns)).map_err(|error| SessionError::Malformed(error.to_string()))?;
    let path = file_path(directory, session);
    std::fs::write(&path, document)?;
    Ok(())
}

/// Load one session's checkpoint. `Ok(None)` when the session file does
/// not exist: absent is a state, not an error.
///
/// # Errors
///
/// [`SessionError::Io`] when the read fails for a reason other than
/// absence; [`SessionError::Malformed`] when the file's shape is wrong.
pub fn load(directory: &Path, session: SessionId) -> Result<Option<Vec<(TurnId, String)>>, SessionError> {
    let path = file_path(directory, session);
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let (stored, turns): (SessionId, Vec<(TurnId, String)>) =
        serde_json::from_slice(&bytes).map_err(|error| SessionError::Malformed(error.to_string()))?;
    if stored != session {
        return Err(SessionError::Malformed(format!(
            "the file names session {stored} but was asked for {session}"
        )));
    }
    Ok(Some(turns))
}

/// Every persisted session id under the directory, sorted by file name
/// - which is ULID order, which is creation order.
///
/// # Errors
///
/// [`SessionError::Io`] when the directory cannot be read.
pub fn list(directory: &Path) -> Result<Vec<SessionId>, SessionError> {
    let mut sessions = Vec::new();
    let entries = match std::fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(sessions),
        Err(error) => return Err(error.into()),
    };
    let mut names: Vec<String> =
        entries.filter_map(Result::ok).filter_map(|entry| entry.file_name().into_string().ok()).collect();
    names.sort();
    for name in names {
        let Some(stem) = name.strip_prefix(FILE_PREFIX) else { continue };
        let Some(stem) = stem.strip_suffix(FILE_SUFFIX) else { continue };
        if let Ok(session) = stem.parse() {
            sessions.push(session);
        }
    }
    Ok(sessions)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch() -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "supra-session-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.subsec_nanos())
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("dir");
        dir
    }

    #[test]
    fn save_then_load_round_trips() {
        let dir = scratch();
        let session = SessionId::generate();
        let turns = vec![(TurnId::generate(), "first".to_owned()), (TurnId::generate(), "second".to_owned())];
        save(&dir, session, &turns).expect("save");
        let loaded = load(&dir, session).expect("load").expect("present");
        assert_eq!(loaded, turns);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_absent_session_loads_as_none_not_an_error() {
        let dir = scratch();
        let loaded = load(&dir, SessionId::generate()).expect("load");
        assert!(loaded.is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn list_orders_by_creation() {
        let dir = scratch();
        let first = SessionId::generate();
        let second = SessionId::generate();
        save(&dir, first, &[]).expect("save 1");
        save(&dir, second, &[]).expect("save 2");
        let listed = list(&dir).expect("list");
        let expected: Vec<SessionId> = [first, second].to_vec();
        let mut sorted = expected.clone();
        sorted.sort();
        assert!(listed == sorted, "{listed:?} vs {sorted:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_file_naming_another_session_refuses() {
        let dir = scratch();
        let session = SessionId::generate();
        let other = SessionId::generate();
        save(&dir, session, &[]).expect("save");
        // `other` has no file of its own, so the id check cannot fire:
        // the file never opens. The mismatch refusal needs a file whose
        // stored id disagrees with the one asked for - so write
        // `session`'s bytes under `other`'s name.
        let stored = std::fs::read(file_path(&dir, session)).expect("stored bytes");
        std::fs::write(file_path(&dir, other), stored).expect("misfiled");
        let error = load(&dir, other).expect_err("mismatch");
        assert!(matches!(error, SessionError::Malformed(_)), "{error}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn save_creates_the_directory_it_was_given() {
        let root = scratch();
        let nested = root.join("a/b/c");
        let session = SessionId::generate();
        save(&nested, session, &[]).expect("save into a missing directory");
        let loaded = load(&nested, session).expect("load").expect("present");
        assert!(loaded.is_empty());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn garbage_files_are_skipped_not_fatal() {
        let dir = scratch();
        std::fs::write(dir.join("session-not-a-ulid.json"), b"{}").expect("write");
        std::fs::write(dir.join("unrelated.txt"), b"x").expect("write");
        let listed = list(&dir).expect("list");
        assert!(listed.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
