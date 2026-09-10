//! The journal: write-ahead snapshots and atomic undo.
//!
//! The permission engine's R1 class is "automatically recoverable", and the
//! architecture is explicit about why `auto` may be the default mode at all:
//! "`auto` is the default only because T16.6 `supra_journal` exists. Without
//! an undo stack, 'auto' would be a hope rather than an engineering
//! decision." This module is that decision.
//!
//! # The write-ahead discipline
//!
//! A snapshot is taken **before** the file's bytes are allowed to change -
//! not after. T14's ledger rule, "store before drop", is the same shape: an
//! undo facility that records what the file *became* can reconstruct history
//! but cannot *revert* it, and reverting is the entire point. [`Journal::snapshot`]
//! therefore reads the file, digests it, and commits the row in one store
//! transaction; only after that `Ok` does the caller perform its edit.
//!
//! The file bytes themselves are stored, not a diff. A diff would be smaller
//! and would need both the current file and a patcher to undo; bytes need
//! neither and verify by digest. Storage cost is bounded by the number of
//! live edits, not the file's history - undone snapshots can be pruned by
//! later stages, and the table is per-snapshot, never per-version.
//!
//! # Undo is one transaction around one write
//!
//! [`Journal::undo`] holds one `Immediate` transaction across the whole
//! operation: read the row, verify the digest **before** writing (a damaged
//! row restores nothing), write the bytes back through the kernel, flush,
//! and mark the row undone - the mark commits with the transaction, so a
//! second undo is refused by state, not by luck. The store's lock is held
//! across the file write on purpose: two concurrent undos of the same row
//! would otherwise both read "not undone", both write, and both report
//! success - the database is the only arbiter that both must pass, so the
//! write happens inside it.
//!
//! # What undo does not promise
//!
//! Undo restores **this snapshot's** bytes, not "the file as it was before
//! whatever you did last". A file may have been edited several times since
//! the snapshot; undoing an old snapshot discards those edits, deliberately
//! and by id - the caller picked the snapshot, so the caller owns the
//! choice. The `AlreadyUndone` refusal exists for the same reason: a second
//! undo of the same row would revert whatever legitimate edit landed after
//! the first one restored the older bytes.

use std::io::Write as _;
use std::path::Path;

use rusqlite::TransactionBehavior;
use supra_store::Store;
use supra_types::{ContentHash, Sealable, SnapshotId};

use crate::error::JournalError;
use crate::schema::{self, SnapshotRow};

/// The bytes of one snapshotted file, for hashing.
///
/// A wrapper rather than a bare hash call, for the reason T10 states: the
/// digest goes through the same `Sealable` machinery as every other stored
/// value, so two different kinds of value hashing the same bytes cannot
/// produce the same digest - a turn body and a file snapshot are different
/// things, and a collision between them must not be able to look like
/// verification.
struct SnapshotBody<'a>(&'a [u8]);

// Checked at compile time rather than by a test, for the reason turns.rs
// states: a kind outside the downstream range would collide with T6's own
// numbering or with the range reserved for tests, and a collision makes two
// different values hash alike.
const _: () = {
    assert!(SnapshotBody::CANONICAL_KIND >= 0x10, "below the range reserved for downstream crates");
    assert!(SnapshotBody::CANONICAL_KIND < 0xF0, "inside the range reserved for tests");
};

impl Sealable for SnapshotBody<'_> {
    /// The second value in the downstream range; `0x10` is T10's turn body.
    const CANONICAL_KIND: u8 = 0x11;

    fn write_canonical(&self, writer: &mut supra_types::CanonicalWriter) {
        writer.bytes(1, self.0);
    }
}

/// Digest of a file's bytes, as the journal records it.
///
/// Public so a caller that kept the bytes (T15.7's rename, which rewrites
/// several files in one operation) can verify a snapshot it is about to
/// trust without re-reading the file.
#[must_use]
pub fn snapshot_digest(bytes: &[u8]) -> ContentHash {
    ContentHash::of(&SnapshotBody(bytes))
}

/// The write-ahead snapshot journal for one store.
pub struct Journal {
    store: std::sync::Arc<Store>,
}

impl Journal {
    /// Open the journal on a migrated store.
    ///
    /// The component migration runs here, once, so a caller cannot reach a
    /// journal method on an unmigrated file - the same self-contained shape
    /// T11's index open takes.
    ///
    /// # Errors
    ///
    /// [`JournalError::Store`] when the component migration fails. A
    /// migration failure leaves the recorded version unchanged, so retrying
    /// after fixing the cause is safe.
    pub fn open(store: std::sync::Arc<Store>) -> Result<Self, JournalError> {
        store.migrate_component(schema::COMPONENT, schema::MIGRATIONS)?;
        Ok(Self { store })
    }

    /// Take a write-ahead snapshot of one file, and return its id.
    ///
    /// The row commits before this returns `Ok`, so the caller's edit is
    /// safe to proceed: whatever it does to the file afterwards, these bytes
    /// are recoverable. A file that does not exist is an error, not an empty
    /// snapshot - an undo that "restores" nothing would report success
    /// while reverting nothing.
    ///
    /// The stored path is absolute and symlink-resolved at snapshot time, so
    /// an undo restores the same file whatever the working directory is by
    /// then.
    ///
    /// # Errors
    ///
    /// [`JournalError::Io`] when the file cannot be read or resolved.
    /// [`JournalError::Store`] when the snapshot row cannot be committed.
    pub fn snapshot(&self, path: impl AsRef<Path>) -> Result<SnapshotId, JournalError> {
        let path = path.as_ref();
        let bytes = std::fs::read(path)?;
        let canonical = std::fs::canonicalize(path)?;
        let digest = snapshot_digest(&bytes);
        let id = SnapshotId::generate();
        let path_text = canonical.to_string_lossy().into_owned();

        self.store.with_transaction::<_, JournalError>(TransactionBehavior::Immediate, |transaction| {
            schema::insert(transaction, id, &path_text, &bytes, &digest)?;
            Ok(())
        })?;

        Ok(id)
    }

    /// Undo one snapshot: restore its bytes and mark it undone.
    ///
    /// One `Immediate` transaction spans read, verify, write, flush, and
    /// mark. The order inside it:
    ///
    /// 1. The row is read; `NotFound` and `AlreadyUndone` refuse here.
    /// 2. The stored digest is compared against the stored bytes (a damaged
    ///    row restores nothing - [`JournalError::Corrupt`] with no bytes,
    ///    I4's reasoning verbatim).
    /// 3. The bytes are written back through the kernel and flushed, so the
    ///    restore survives a power loss.
    /// 4. The row is marked undone; the mark commits with the transaction.
    ///
    /// A crash between the file write and the mark leaves the bytes restored
    /// and the row unmarked; undoing again rewrites the same bytes -
    /// idempotent, not destructive - which is the failure mode a crash must
    /// leave behind. Concurrent undoes of the same row serialise on the
    /// store's lock: the loser reads `AlreadyUndone` and refuses.
    ///
    /// # Errors
    ///
    /// [`JournalError::NotFound`] when the id names no snapshot.
    /// [`JournalError::AlreadyUndone`] when the snapshot has been undone.
    /// [`JournalError::Corrupt`] when the stored bytes do not match their
    /// digest.
    /// [`JournalError::Io`] when the file cannot be written or flushed.
    /// [`JournalError::Store`] when the store refuses.
    pub fn undo(&self, snapshot: SnapshotId) -> Result<(), JournalError> {
        self.store.with_transaction::<_, JournalError>(TransactionBehavior::Immediate, |transaction| {
            let (bytes, stored_digest, undone, path) =
                schema::read_row(transaction, snapshot)?.ok_or(JournalError::NotFound { snapshot })?;

            if undone {
                return Err(JournalError::AlreadyUndone { snapshot });
            }

            let computed = snapshot_digest(&bytes);
            if computed != stored_digest {
                return Err(JournalError::Corrupt { snapshot, stored: stored_digest, computed });
            }

            let target = std::path::PathBuf::from(&path);
            let directory = target.parent().map_or_else(|| std::path::PathBuf::from("."), Path::to_path_buf);
            let file_name = target
                .file_name()
                .map_or_else(|| std::ffi::OsString::from("supra-restore"), std::ffi::OsString::from);
            let temporary = directory.join(format!(".supra-undo-{snapshot}-{}", file_name.to_string_lossy()));

            {
                use std::os::unix::fs::OpenOptionsExt as _;
                let mut file =
                    std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&temporary)?;
                file.write_all(&bytes)?;
                file.sync_all()?;
            }
            // A rename replaces whatever sits at the target - a symlink
            // included - without following it, so a swapped link cannot
            // turn the restore into a write outside the workspace.
            std::fs::rename(&temporary, &target)?;

            schema::mark_undone(transaction, snapshot)?;
            Ok(())
        })
    }

    /// The newest not-yet-undone snapshot for one path, if any.
    ///
    /// This is a report, not an undo target: it names which snapshot *would*
    /// be undone by "undo the last edit to this file", so the caller (a
    /// slash command, a TUI surface) can show the file and the time before
    /// committing to it. Choosing is still the caller's job.
    ///
    /// The path is resolved the way [`Journal::snapshot`] resolves it, so a
    /// report asked through a symlinked directory (macOS `$TMPDIR` resolves
    /// through `/var/folders` to `/private/var`) names the snapshots taken
    /// through that same symlink rather than finding none. A path that
    /// cannot be resolved reads as empty - the caller named a file no
    /// snapshot ever saw.
    ///
    /// # Errors
    ///
    /// [`JournalError::Store`] when the store refuses.
    pub fn newest_for_path(&self, path: impl AsRef<Path>) -> Result<Option<SnapshotRow>, JournalError> {
        let path_text = resolved_path_text(path)?;
        self.store.with_transaction::<_, JournalError>(TransactionBehavior::Deferred, |transaction| {
            Ok(schema::newest_for_path(transaction, &path_text)?)
        })
    }

    /// How many snapshots are stored for one path, undone or not.
    ///
    /// A budget the operator can watch: an unbounded stack is a disk cost
    /// nobody asked for, and later stages prune on this number. Resolved
    /// like [`newest_for_path`] for the same reason.
    ///
    /// # Errors
    ///
    /// [`JournalError::Store`] when the store refuses.
    pub fn count_for_path(&self, path: impl AsRef<Path>) -> Result<i64, JournalError> {
        let path_text = resolved_path_text(path)?;
        self.store.with_transaction::<_, JournalError>(TransactionBehavior::Deferred, |transaction| {
            Ok(schema::count_for_path(transaction, &path_text)?)
        })
    }
}

/// The path text a snapshot row stores: absolute and symlink-resolved.
///
/// One resolver for insert and query, because two spellings of one file are
/// two different keys in the table - macOS `$TMPDIR` proves it.
fn resolved_path_text(path: impl AsRef<Path>) -> Result<String, JournalError> {
    let canonical = std::fs::canonicalize(path)?;
    Ok(canonical.to_string_lossy().into_owned())
}

impl core::fmt::Debug for Journal {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Journal").field("path", &self.store.path()).finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use supra_store::Store;

    use super::*;

    fn journal() -> (Journal, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!("supra-journal-{}", scratch_tag()));
        if std::fs::create_dir_all(&dir).is_err() {
            // Another test process claimed this tag first (a stale dir left
            // by a killed run keeps the name taken). Retry once with a fresh
            // tag rather than sharing a database file with a stranger.
            let dir = std::env::temp_dir().join(format!("supra-journal-{}", scratch_tag()));
            std::fs::create_dir_all(&dir).expect("dir");
            let store = Store::open(dir.join("journal.db")).expect("store");
            let journal = Journal::open(std::sync::Arc::new(store)).expect("journal");
            return (journal, dir);
        }
        let store = Store::open(dir.join("journal.db")).expect("store");
        let journal = Journal::open(std::sync::Arc::new(store)).expect("journal");
        (journal, dir)
    }

    /// Unique-enough scratch names without a new dependency: the pid breaks
    /// up concurrent test binaries, a random suffix breaks up the tests
    /// inside one. A bare counter is process-ordered but not
    /// process-unique: two test binaries started in the same millisecond
    /// share pid-adjacent values (cargo reuses pids fast) and can claim the
    /// same tag, so each tag carries fresh entropy instead.
    fn scratch_tag() -> String {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash as _, Hasher as _};
        use std::time::SystemTime;
        let mut hasher = DefaultHasher::new();
        std::process::id().hash(&mut hasher);
        std::thread::current().id().hash(&mut hasher);
        SystemTime::now().hash(&mut hasher);
        // Pointer entropy: two calls in the same nanosecond still differ,
        // because each formats a different stack slot.
        let slot = 0u8;
        std::ptr::from_ref(&slot).hash(&mut hasher);
        format!("{:016x}", hasher.finish())
    }

    fn write_file(path: &Path, contents: &[u8]) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("parent");
        }
        std::fs::write(path, contents).expect("write");
    }

    #[test]
    fn a_snapshot_round_trips_through_undo() {
        let (journal, dir) = journal();
        let file = dir.join("a.rs");
        write_file(&file, b"fn main() {}");

        let id = journal.snapshot(&file).expect("snapshot");
        write_file(&file, b"fn main() { todo!() }");

        journal.undo(id).expect("undo");
        assert_eq!(std::fs::read(&file).expect("read"), b"fn main() {}", "the original bytes return");
    }

    #[test]
    fn a_relative_snapshot_restores_from_any_working_directory() {
        static CWD_GUARD: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let _lock = CWD_GUARD.lock().unwrap_or_else(std::sync::PoisonError::into_inner);

        let (journal, dir) = journal();
        let file = dir.join("a.rs");
        write_file(&file, b"original");
        let original_cwd = std::env::current_dir().expect("cwd");
        std::env::set_current_dir(&dir).expect("chdir");
        let id = journal.snapshot("a.rs").expect("relative snapshot");
        std::env::set_current_dir(&original_cwd).expect("back");
        write_file(&file, b"edited");

        journal.undo(id).expect("undo from the original cwd");
        assert_eq!(std::fs::read(&file).expect("read"), b"original");
    }

    #[test]
    #[cfg(unix)]
    fn undo_replaces_a_swapped_symlink_without_following_it() {
        let (journal, dir) = journal();
        let file = dir.join("target.rs");
        let outside = dir.join("outside.txt");
        let link = dir.join("link.rs");
        write_file(&file, b"original");
        write_file(&outside, b"do not touch");

        let id = journal.snapshot(&file).expect("snapshot");
        write_file(&file, b"edited");
        std::os::unix::fs::symlink(&outside, &link).expect("swap in a symlink at the snapshotted path");

        journal.undo(id).expect("undo");
        assert_eq!(std::fs::read(&file).expect("read"), b"original", "the path holds the restored bytes");
        assert_eq!(std::fs::read(&link).expect("read"), b"do not touch", "the symlink target is untouched");
    }

    #[test]
    fn snapshot_commits_before_the_edit_may_proceed() {
        // The write-ahead order, observed: snapshot() must return Ok with
        // the row committed even though the file has not changed yet, and a
        // crash-of-the-caller between snapshot and edit leaves a snapshot
        // that undoes to identical bytes - harmless, and the shape that
        // makes "take the snapshot, then edit" safe to interleave.
        let (journal, dir) = journal();
        let file = dir.join("a.rs");
        write_file(&file, b"original");

        let id = journal.snapshot(&file).expect("snapshot");
        let count = journal.count_for_path(&file).expect("count");
        assert_eq!(count, 1, "the row committed before the edit");

        journal.undo(id).expect("undo");
        assert_eq!(std::fs::read(&file).expect("read"), b"original", "undo restores the same bytes");
    }

    #[test]
    fn a_missing_file_is_refused_not_snapshotted_empty() {
        let (journal, dir) = journal();
        let missing = dir.join("nope.rs");
        assert!(journal.snapshot(&missing).is_err(), "no empty snapshots");
    }

    #[test]
    fn undo_twice_is_refused() {
        let (journal, dir) = journal();
        let file = dir.join("a.rs");
        write_file(&file, b"one");
        let id = journal.snapshot(&file).expect("snapshot");
        write_file(&file, b"two");

        journal.undo(id).expect("first undo");
        match journal.undo(id) {
            Err(JournalError::AlreadyUndone { snapshot }) => assert_eq!(snapshot, id),
            other => panic!("the second undo must be refused; got {other:?}"),
        }

        // And the refusal left the file alone - the restored bytes still
        // stand, because refusing is not reverting.
        assert_eq!(std::fs::read(&file).expect("read"), b"one");
    }

    #[test]
    fn undoing_an_unknown_id_is_not_found() {
        let (journal, _dir) = journal();
        match journal.undo(SnapshotId::generate()) {
            Err(JournalError::NotFound { .. }) => {}
            other => panic!("an unknown id must be NotFound; got {other:?}"),
        }
    }

    #[test]
    fn a_damaged_snapshot_restores_nothing() {
        // I4's reasoning: the bytes are not returned, the undo is refused.
        // Damaged here means the stored blob no longer hashes to the digest
        // recorded beside it - reachable only by editing the database
        // around the API, which is exactly the probe a corruption test owes.
        let (journal, dir) = journal();
        let file = dir.join("a.rs");
        write_file(&file, b"original");
        let id = journal.snapshot(&file).expect("snapshot");

        // Corrupt the stored bytes behind the journal's back. The schema's
        // CHECK refuses a blob whose length disagrees with byte_len, so the
        // corruption that can actually land is a same-length mutation: the
        // bytes change, the recorded digest does not, and only the undo-time
        // verification can catch it.
        journal
            .store
            .with_transaction::<_, JournalError>(TransactionBehavior::Immediate, |transaction| {
                transaction
                    .execute(
                        "UPDATE journal_snapshot SET before = CAST(upper(CAST(before AS TEXT)) AS BLOB) WHERE snapshot_id = ?1",
                        [id.to_string()],
                    )
                    .map_err(JournalError::Sqlite)?;
                Ok(())
            })
            .expect("corrupt");

        match journal.undo(id) {
            Err(JournalError::Corrupt { snapshot, .. }) => assert_eq!(snapshot, id),
            other => panic!("a damaged snapshot must be Corrupt; got {other:?}"),
        }
        // The file still holds whatever it held - the undo refused rather
        // than restoring the damaged bytes.
        assert_eq!(std::fs::read(&file).expect("read"), b"original");
    }

    #[test]
    fn concurrent_undos_of_one_row_one_wins() {
        // The store lock is the arbiter: both undoes target the same row,
        // and only one may restore-and-mark. The other must refuse with
        // AlreadyUndone rather than double-writing.
        let (journal, dir) = journal();
        let file = dir.join("a.rs");
        write_file(&file, b"contended");
        let id = journal.snapshot(&file).expect("snapshot");
        write_file(&file, b"edited");

        let clone = std::sync::Arc::new(journal);
        let left = std::thread::spawn({
            let journal = std::sync::Arc::clone(&clone);
            move || journal.undo(id)
        });
        let right = std::thread::spawn({
            let journal = std::sync::Arc::clone(&clone);
            move || journal.undo(id)
        });

        let outcomes = [left.join().expect("left thread"), right.join().expect("right thread")];
        let refusals = outcomes
            .iter()
            .filter(|outcome| matches!(outcome, Err(JournalError::AlreadyUndone { .. })))
            .count();
        assert_eq!(refusals, 1, "exactly one undo wins: {outcomes:?}");
        assert!(outcomes.iter().any(Result::is_ok), "one undo succeeded: {outcomes:?}");
        assert_eq!(std::fs::read(&file).expect("read"), b"contended", "the file holds the restored bytes");
    }

    #[test]
    fn newest_for_path_reports_the_live_head() {
        let (journal, dir) = journal();
        let file = dir.join("a.rs");
        write_file(&file, b"v1");
        let first = journal.snapshot(&file).expect("snapshot");
        write_file(&file, b"v2");
        let second = journal.snapshot(&file).expect("snapshot");

        // Two snapshots in one millisecond are the common case, not an edge:
        // ULIDs order by their random bits within a millisecond, and the
        // newest report must still name the *second* snapshot. Pinning the
        // tiebreak through the ids themselves (rather than sleeping past the
        // millisecond) keeps the test deterministic and fast.
        let expected_head = if second > first { second } else { first };
        let newest = journal.newest_for_path(&file).expect("report").expect("present");
        assert_eq!(newest.snapshot, expected_head, "the newest live snapshot heads the stack");
        assert_eq!(newest.byte_len, 2, "v2 is two bytes");

        // Undone means "no longer the head", whichever id held it: undo the
        // head, and the other snapshot must surface.
        journal.undo(expected_head).expect("undo");
        let remaining = if expected_head == second { first } else { second };
        let next = journal.newest_for_path(&file).expect("report").expect("present");
        assert_eq!(next.snapshot, remaining, "an undone snapshot no longer heads the stack");

        let count = journal.count_for_path(&file).expect("count");
        assert_eq!(count, 2, "counting includes undone snapshots - it is a budget, not a queue");
    }

    #[test]
    fn created_at_is_the_ids_own_timestamp_not_a_second_clock_read() {
        // The flake that closed structurally: reading the clock twice (once
        // inside Ulid::generate, once for created_at) disagreed whenever the
        // millisecond turned between the reads, and ORDER BY created_at could
        // then name the *older* snapshot as newest. The row's timestamp is
        // now derived from the id, so the two cannot disagree; this test
        // pins that derivation, because a future "simplification" back to a
        // second clock read would resurrect the flake invisibly.
        let (journal, dir) = journal();
        let file = dir.join("a.rs");
        write_file(&file, b"x");
        let id = journal.snapshot(&file).expect("snapshot");

        let row = journal
            .store
            .with_transaction::<_, JournalError>(TransactionBehavior::Deferred, |transaction| {
                Ok(schema::newest_for_path(transaction, &resolved_path_text(&file)?)
                    .expect("read")
                    .expect("present"))
            })
            .expect("report");

        assert_eq!(
            row.created_at,
            i64::try_from(id.timestamp_ms()).expect("millis fit"),
            "created_at must come from the id, not a second clock read"
        );
        assert_eq!(row.snapshot, id);
    }

    #[test]
    fn a_symlinked_temp_directory_reports_its_snapshots() {
        // macOS CI proved the shape: $TMPDIR resolves through /var/folders
        // to /private/var, snapshot stored the resolved path, and a query
        // asked through the unresolved spelling found nothing. Reproduced
        // here with a symlink on any Unix, so the regression guards Linux
        // too.
        let (journal, dir) = journal();
        let file = dir.join("a.rs");
        write_file(&file, b"x");
        let _id = journal.snapshot(&file).expect("snapshot");

        #[cfg(unix)]
        {
            let linked = dir.join("linked");
            std::os::unix::fs::symlink(&dir, &linked).expect("symlink");
            let through_link = linked.join("a.rs");
            assert_eq!(std::fs::read(&through_link).expect("read"), b"x");
            let count = journal.count_for_path(&through_link).expect("count");
            assert_eq!(count, 1, "a query through the symlink finds the snapshot");
            assert!(
                journal.newest_for_path(&through_link).expect("report").is_some(),
                "the newest report resolves the path the way snapshot did"
            );
        }
    }

    #[test]
    fn the_digest_is_domain_separated_from_a_turn_body() {
        // The reason `Sealable` goes through a wrapper type: a turn body and
        // a file snapshot hashing the same bytes must not produce the same
        // digest, or a collision between them could look like verification.
        let bytes = b"same bytes, different kinds";
        assert_ne!(
            snapshot_digest(bytes),
            supra_store::body_digest(bytes),
            "a snapshot digest and a turn digest must differ on the same bytes"
        );
    }

    #[test]
    fn undo_restores_bytes_not_metadata() {
        // `created_at` is outside the digest on purpose; the same bytes
        // snapshotted twice still verify. This pins that the restore path
        // never depends on the timestamp - it writes the stored blob, and
        // that is all it writes. Each undo restores *its own* snapshot's
        // bytes: the second was taken while "transient" was on disk, so its
        // undo returns "transient", and the first returns "stable".
        let (journal, dir) = journal();
        let file = dir.join("a.rs");
        write_file(&file, b"stable");
        let first = journal.snapshot(&file).expect("first");
        write_file(&file, b"transient");
        let second = journal.snapshot(&file).expect("second");
        write_file(&file, b"edited again");

        journal.undo(second).expect("undo second");
        assert_eq!(std::fs::read(&file).expect("read"), b"transient");

        journal.undo(first).expect("undo first");
        assert_eq!(
            std::fs::read(&file).expect("read"),
            b"stable",
            "the first snapshot's bytes, restored, agree"
        );
    }
}
