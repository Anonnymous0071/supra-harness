use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, PoisonError, Weak};

use serde::{Deserialize, Serialize};
use supra_types::{SessionId, TurnId};

use crate::checkpoint::{CHECKPOINT_VERSION, CheckpointTurn, SessionCheckpoint};
use crate::error::SessionError;

const FILE_PREFIX: &str = "session-";
const FILE_SUFFIX: &str = ".json";

#[derive(Serialize)]
struct CheckpointDocument<'a> {
    format: &'static str,
    version: u32,
    checkpoint: &'a SessionCheckpoint,
}

#[derive(Deserialize)]
struct OwnedCheckpointDocument {
    format: String,
    version: u32,
    checkpoint: serde_json::Value,
}

fn file_path(directory: &Path, session: SessionId) -> PathBuf {
    directory.join(format!("{FILE_PREFIX}{session}{FILE_SUFFIX}"))
}

fn save_locks() -> &'static Mutex<HashMap<PathBuf, Weak<Mutex<()>>>> {
    static LOCKS: OnceLock<Mutex<HashMap<PathBuf, Weak<Mutex<()>>>>> = OnceLock::new();
    LOCKS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn save_lock(path: &Path) -> Arc<Mutex<()>> {
    let mut locks = save_locks().lock().unwrap_or_else(PoisonError::into_inner);
    locks.retain(|_, lock| lock.strong_count() > 0);
    if let Some(lock) = locks.get(path).and_then(Weak::upgrade) {
        return lock;
    }
    let lock = Arc::new(Mutex::new(()));
    locks.insert(path.to_path_buf(), Arc::downgrade(&lock));
    lock
}

#[cfg(unix)]
fn ensure_private_directory(directory: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt as _;

    std::fs::create_dir_all(directory)?;
    std::fs::set_permissions(directory, std::fs::Permissions::from_mode(0o700))
}

#[cfg(not(unix))]
fn ensure_private_directory(directory: &Path) -> io::Result<()> {
    std::fs::create_dir_all(directory)
}

fn open_temporary(directory: &Path, session: SessionId) -> io::Result<(File, PathBuf)> {
    for _ in 0..16 {
        let nonce = SessionId::generate();
        let path = directory.join(format!(".{FILE_PREFIX}{session}-{nonce}.tmp"));
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            options.mode(0o600);
        }
        match options.open(&path) {
            Ok(file) => return Ok((file, path)),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error),
        }
    }
    Err(io::Error::new(io::ErrorKind::AlreadyExists, "could not allocate a unique session temporary file"))
}

struct TemporaryFile {
    path: PathBuf,
    published: bool,
}

impl TemporaryFile {
    fn new(path: PathBuf) -> Self {
        Self { path, published: false }
    }
}

impl Drop for TemporaryFile {
    fn drop(&mut self) {
        if !self.published {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

trait SaveOperations {
    fn rename(&self, source: &Path, destination: &Path) -> io::Result<()> {
        std::fs::rename(source, destination)
    }

    fn sync_parent(&self, directory: &Path) -> io::Result<()> {
        File::open(directory)?.sync_all()
    }
}

struct RealSaveOperations;
impl SaveOperations for RealSaveOperations {}

fn save_with_operations(
    directory: &Path,
    session: SessionId,
    turns: &[(TurnId, String)],
    operations: &impl SaveOperations,
) -> Result<(), SessionError> {
    let document =
        serde_json::to_vec(&(session, turns)).map_err(|error| SessionError::Malformed(error.to_string()))?;
    ensure_private_directory(directory)?;
    let path = file_path(directory, session);
    let lock = save_lock(&path);
    let _guard = lock.lock().unwrap_or_else(PoisonError::into_inner);
    let (mut file, temporary_path) = open_temporary(directory, session)?;
    let mut temporary = TemporaryFile::new(temporary_path);

    file.write_all(&document)?;
    file.sync_all()?;
    drop(file);
    operations.rename(&temporary.path, &path)?;
    temporary.published = true;
    operations.sync_parent(directory)?;
    Ok(())
}

/// Persist one session's checkpoint under its session directory.
///
/// # Errors
///
/// [`SessionError::Io`] when the write fails; [`SessionError::Malformed`]
/// when serialization fails, which it cannot for this shape.
pub fn save(directory: &Path, session: SessionId, turns: &[(TurnId, String)]) -> Result<(), SessionError> {
    save_with_operations(directory, session, turns, &RealSaveOperations)
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
    if let Ok((stored, turns)) = serde_json::from_slice::<(SessionId, Vec<(TurnId, String)>)>(&bytes) {
        if stored != session {
            return Err(SessionError::Malformed(format!(
                "the file names session {stored} but was asked for {session}"
            )));
        }
        return Ok(Some(turns));
    }

    let checkpoint = decode_checkpoint(&bytes, session)?;
    Ok(Some(checkpoint.turns().iter().map(|turn| (turn.turn(), turn.display_body().to_owned())).collect()))
}

/// Persist a versioned, lossless checkpoint under its session directory.
///
/// A per-session lock serialises cooperating writers. When `expected_revision`
/// is present, publication is compare-and-swap: a stale writer refuses instead
/// of replacing a newer checkpoint.
///
/// # Errors
///
/// [`SessionError::RevisionConflict`] for a stale expected revision;
/// [`SessionError::Malformed`] for invalid checkpoint state; otherwise I/O.
pub fn save_checkpoint(
    directory: &Path,
    checkpoint: &SessionCheckpoint,
    expected_revision: Option<u64>,
) -> Result<(), SessionError> {
    checkpoint.validate()?;
    ensure_private_directory(directory)?;
    let path = file_path(directory, checkpoint.session());
    let lock = save_lock(&path);
    let _guard = lock.lock().unwrap_or_else(PoisonError::into_inner);

    if let Some(expected) = expected_revision {
        let found = current_revision(&path, checkpoint.session())?;
        if found != expected {
            return Err(SessionError::RevisionConflict { expected, found });
        }
    }

    let document = CheckpointDocument { format: "supra-session", version: CHECKPOINT_VERSION, checkpoint };
    let bytes = serde_json::to_vec(&document).map_err(|error| SessionError::Malformed(error.to_string()))?;
    publish_unlocked(directory, &path, checkpoint.session(), &bytes, &RealSaveOperations)
}

/// Load a versioned checkpoint. Legacy tuple files are upgraded in memory.
///
/// # Errors
///
/// [`SessionError::Io`] when the read fails for a reason other than absence;
/// [`SessionError::UnsupportedVersion`] for a newer envelope; and
/// [`SessionError::Malformed`] for damaged state.
pub fn load_checkpoint(
    directory: &Path,
    session: SessionId,
) -> Result<Option<SessionCheckpoint>, SessionError> {
    let path = file_path(directory, session);
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    Ok(Some(decode_checkpoint(&bytes, session)?))
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
    let mut names: Vec<String> = Vec::new();
    for entry in entries {
        let name = entry?;
        let Ok(text) = name.file_name().into_string() else { continue };
        names.push(text);
    }
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

fn decode_checkpoint(bytes: &[u8], expected: SessionId) -> Result<SessionCheckpoint, SessionError> {
    if let Ok(document) = serde_json::from_slice::<OwnedCheckpointDocument>(bytes) {
        if document.format != "supra-session" {
            return Err(SessionError::Malformed(format!("unknown checkpoint format {:?}", document.format)));
        }
        if document.version != CHECKPOINT_VERSION {
            return Err(SessionError::UnsupportedVersion { found: document.version });
        }
        let checkpoint: SessionCheckpoint = serde_json::from_value(document.checkpoint)
            .map_err(|error| SessionError::Malformed(error.to_string()))?;
        if checkpoint.version() != document.version {
            return Err(SessionError::Malformed(format!(
                "envelope version {} disagrees with checkpoint version {}",
                document.version,
                checkpoint.version()
            )));
        }
        if checkpoint.session() != expected {
            return Err(SessionError::Malformed(format!(
                "the file names session {} but was asked for {expected}",
                checkpoint.session()
            )));
        }
        checkpoint.validate()?;
        return Ok(checkpoint);
    }

    let (stored, turns): (SessionId, Vec<(TurnId, String)>) =
        serde_json::from_slice(bytes).map_err(|error| SessionError::Malformed(error.to_string()))?;
    if stored != expected {
        return Err(SessionError::Malformed(format!(
            "the file names session {stored} but was asked for {expected}"
        )));
    }
    let mut checkpoint = SessionCheckpoint::new(stored);
    for (turn, body) in turns {
        checkpoint.push_turn(CheckpointTurn::new(turn, body, Vec::new(), None))?;
    }
    Ok(checkpoint)
}

fn current_revision(path: &Path, session: SessionId) -> Result<u64, SessionError> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(error) => return Err(error.into()),
    };
    Ok(decode_checkpoint(&bytes, session)?.revision())
}

fn publish_unlocked(
    directory: &Path,
    path: &Path,
    session: SessionId,
    bytes: &[u8],
    operations: &impl SaveOperations,
) -> Result<(), SessionError> {
    let (mut file, temporary_path) = open_temporary(directory, session)?;
    let mut temporary = TemporaryFile::new(temporary_path);

    file.write_all(bytes)?;
    file.sync_all()?;
    drop(file);
    operations.rename(&temporary.path, path)?;
    temporary.published = true;
    operations.sync_parent(directory)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::LifecycleStatus;
    use std::sync::atomic::{AtomicBool, Ordering};

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
    fn checkpoint_protocol_and_opaque_snapshots_round_trip() {
        let dir = scratch();
        let session = SessionId::generate();
        let turn = TurnId::generate();
        let mut checkpoint = SessionCheckpoint::new(session);
        checkpoint
            .push_turn(CheckpointTurn::new(
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
        checkpoint.set_ledger(Some(serde_json::json!({"generation": 4})));
        checkpoint.set_config(Some(serde_json::json!({"model": "frozen"})));

        save_checkpoint(&dir, &checkpoint, Some(0)).expect("save");
        let loaded = load_checkpoint(&dir, session).expect("load").expect("present");
        assert_eq!(loaded, checkpoint);
        assert_eq!(loaded.turns()[0].messages()[0].payload()["provider_field"], 7);
        assert_eq!(
            load(&dir, session).expect("legacy projection"),
            Some(vec![(turn, "accepted".to_owned())])
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn legacy_tuple_files_upgrade_without_rewriting() {
        let dir = scratch();
        let session = SessionId::generate();
        let turn = TurnId::generate();
        let legacy = serde_json::to_vec(&(session, vec![(turn, "legacy".to_owned())])).expect("encode");
        std::fs::write(file_path(&dir, session), &legacy).expect("legacy file");

        let loaded = load_checkpoint(&dir, session).expect("load").expect("present");
        assert_eq!(loaded.turns()[0].display_body(), "legacy");
        assert!(loaded.turns()[0].messages().is_empty());
        assert_eq!(std::fs::read(file_path(&dir, session)).expect("unchanged"), legacy);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn stale_checkpoint_writer_is_refused() {
        let dir = scratch();
        let session = SessionId::generate();
        let mut first = SessionCheckpoint::new(session);
        first.set_status(LifecycleStatus::Running);
        save_checkpoint(&dir, &first, Some(0)).expect("first");

        let mut second = first.clone();
        second.set_status(LifecycleStatus::Completed);
        save_checkpoint(&dir, &second, Some(first.revision())).expect("second");

        let mut stale = first;
        stale.set_status(LifecycleStatus::Interrupted);
        let error = save_checkpoint(&dir, &stale, Some(stale.revision() - 1)).expect_err("stale");
        assert!(matches!(error, SessionError::RevisionConflict { .. }), "{error}");
        assert_eq!(load_checkpoint(&dir, session).expect("load").expect("present"), second);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn damaged_and_future_checkpoints_are_refused() {
        let dir = scratch();
        let session = SessionId::generate();
        std::fs::write(file_path(&dir, session), b"{not-json").expect("garbage");
        assert!(matches!(load_checkpoint(&dir, session), Err(SessionError::Malformed(_))));

        let future = serde_json::json!({
            "format": "supra-session",
            "version": CHECKPOINT_VERSION + 1,
            "checkpoint": {
                "incompatible_future_shape": true
            }
        });
        std::fs::write(file_path(&dir, session), serde_json::to_vec(&future).expect("encode"))
            .expect("future");
        assert!(matches!(
            load_checkpoint(&dir, session),
            Err(SessionError::UnsupportedVersion { found }) if found == CHECKPOINT_VERSION + 1
        ));
        let _ = std::fs::remove_dir_all(&dir);
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

    #[cfg(unix)]
    #[test]
    fn a_predictable_temporary_symlink_is_never_followed() {
        use std::os::unix::fs::symlink;

        let dir = scratch();
        let session = SessionId::generate();
        let victim = dir.join("victim");
        std::fs::write(&victim, b"untouched").expect("victim");
        let predictable = dir.join(format!("{FILE_PREFIX}{session}{FILE_SUFFIX}.tmp"));
        symlink(&victim, &predictable).expect("hostile symlink");

        save(&dir, session, &[(TurnId::generate(), "safe".to_owned())]).expect("save");

        assert_eq!(std::fs::read(&victim).expect("victim bytes"), b"untouched");
        assert!(std::fs::symlink_metadata(&predictable).expect("symlink remains").file_type().is_symlink());
        assert!(load(&dir, session).expect("load").is_some());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn concurrent_saves_for_one_session_always_publish_valid_json() {
        let dir = scratch();
        let session = SessionId::generate();
        let bodies: Vec<String> = (0..16).map(|index| format!("body-{index}")).collect();

        std::thread::scope(|scope| {
            for body in &bodies {
                let dir = &dir;
                scope.spawn(move || {
                    save(dir, session, &[(TurnId::generate(), body.clone())]).expect("save");
                });
            }
        });

        let loaded = load(&dir, session).expect("load").expect("present");
        assert_eq!(loaded.len(), 1);
        assert!(bodies.contains(&loaded[0].1), "unexpected body: {:?}", loaded[0].1);
        let leftovers: Vec<_> = std::fs::read_dir(&dir)
            .expect("read directory")
            .filter_map(Result::ok)
            .filter(|entry| entry.file_name().to_string_lossy().ends_with(".tmp"))
            .collect();
        assert!(leftovers.is_empty(), "temporary files remain: {leftovers:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    struct FailingRename;

    impl SaveOperations for FailingRename {
        fn rename(&self, _source: &Path, _destination: &Path) -> io::Result<()> {
            Err(io::Error::other("injected rename failure"))
        }
    }

    #[test]
    fn a_failed_publish_removes_its_temporary_file() {
        let dir = scratch();
        let session = SessionId::generate();
        let error = save_with_operations(&dir, session, &[], &FailingRename).expect_err("rename must fail");
        assert!(matches!(error, SessionError::Io(_)), "{error}");
        assert!(!file_path(&dir, session).exists());
        let names: Vec<_> = std::fs::read_dir(&dir)
            .expect("read directory")
            .filter_map(Result::ok)
            .map(|entry| entry.file_name())
            .collect();
        assert!(names.is_empty(), "failed save left files: {names:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    struct ObservedParentSync<'a>(&'a AtomicBool);

    impl SaveOperations for ObservedParentSync<'_> {
        fn sync_parent(&self, directory: &Path) -> io::Result<()> {
            assert!(directory.read_dir()?.any(|entry| {
                entry.is_ok_and(|entry| entry.file_name().to_string_lossy().ends_with(FILE_SUFFIX))
            }));
            self.0.store(true, Ordering::SeqCst);
            File::open(directory)?.sync_all()
        }
    }

    #[test]
    fn parent_directory_is_synced_after_publish() {
        let dir = scratch();
        let synced = AtomicBool::new(false);
        save_with_operations(&dir, SessionId::generate(), &[], &ObservedParentSync(&synced)).expect("save");
        assert!(synced.load(Ordering::SeqCst));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn session_directory_and_file_are_private() {
        use std::os::unix::fs::PermissionsExt as _;

        let root = scratch();
        let dir = root.join("private");
        let session = SessionId::generate();
        save(&dir, session, &[]).expect("save");

        assert_eq!(std::fs::metadata(&dir).expect("directory metadata").permissions().mode() & 0o777, 0o700);
        assert_eq!(
            std::fs::metadata(file_path(&dir, session)).expect("file metadata").permissions().mode() & 0o777,
            0o600
        );
        let _ = std::fs::remove_dir_all(&root);
    }
}
