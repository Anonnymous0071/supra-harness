//! The journal's tables in the shared store file.
//!
//! `user_version` is one slot per file and T10's core schema owns it, so the
//! journal records its schema history in `schema_component` under
//! [`COMPONENT`] - the second-owner pattern T11 set with the vector index.
//! Forward only, dense from 1, each step's DDL and version bump in one
//! transaction: a file whose journal component is ahead of this build is
//! refused, not downgraded.
//!
//! The table shapes follow T10's discipline: the invariants the code depends
//! on are `CHECK` constraints, where no code path can bypass them - a
//! snapshot id that is not 26 characters, a digest that is not 32 bytes, or
//! a length that disagrees with the blob it describes is refused at write
//! time rather than discovered at undo.

use rusqlite::OptionalExtension as _;
use supra_store::ComponentMigration;
use supra_types::SnapshotId;

/// This component's name in `schema_component`.
pub const COMPONENT: &str = "journal";

/// The snapshot table's name, for the queries this module runs.
const TABLE: &str = "journal_snapshot";

/// The journal's schema steps, in order.
pub const MIGRATIONS: &[ComponentMigration] = &[ComponentMigration {
    version: 1,
    sql: "
        -- One row per snapshot: the bytes a file held before an edit was
        -- allowed to touch it, taken by the write-ahead discipline T16.6
        -- owes the permission engine's R1 class.
        CREATE TABLE journal_snapshot (
            -- The 26-character Crockford form, as every other ULID-backed id
            -- in the store is, for the same reasons: sortable in SQL by time,
            -- readable in an incident shell.
            snapshot_id TEXT    PRIMARY KEY NOT NULL
                        CHECK (length(snapshot_id) = 26),
            -- The file the snapshot belongs to, verbatim as the caller named
            -- it. Two snapshots of one file are two rows; undo picks one by
            -- id, never by \"the latest for the path\", because which snapshot
            -- to undo is a decision, not a position.
            path        TEXT    NOT NULL
                        CHECK (length(path) > 0),
            -- The file's bytes before the edit. BLOB, not TEXT: TEXT invites
            -- an encoding assumption, and undo promises these bytes back
            -- unchanged.
            before      BLOB    NOT NULL,
            -- Digest of `before` alone, domain-separated through
            -- `SnapshotBody`. A damaged row is detected on read rather than
            -- restored from.
            before_hash BLOB    NOT NULL
                        CHECK (length(before_hash) = 32),
            -- Redundant with length(before), kept for the same reason T10
            -- keeps its copy: a size report that does not read every blob.
            -- The CHECK keeps the redundancy from becoming a second,
            -- disagreeing answer.
            byte_len    INTEGER NOT NULL
                        CHECK (byte_len = length(before) AND byte_len >= 0),
            -- Milliseconds since the epoch. Diagnostics only, deliberately
            -- outside the digest, so the same bytes snapshotted at two
            -- different times still verify.
            created_at  INTEGER NOT NULL
                        CHECK (created_at >= 0),
            -- Whether this snapshot has been undone. Undo marks the row in
            -- the same transaction that restores the bytes; a second undo is
            -- refused as `AlreadyUndone` because it would silently revert
            -- whichever legitimate edit landed after the first one.
            undone      INTEGER NOT NULL
                        CHECK (undone IN (0, 1))
        ) STRICT;

        -- The undo stack for one file, newest first. A report that wants to
        -- offer \"undo the last edit to this file\" reads the head of this
        -- index without scanning the table.
        CREATE INDEX journal_snapshot_path_created
            ON journal_snapshot (path, created_at DESC);
    ",
}];

/// One stored snapshot, as a report reads it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SnapshotRow {
    /// The snapshot's id.
    pub snapshot: SnapshotId,
    /// The file it belongs to.
    pub path: String,
    /// Digest of the stored `before` bytes.
    pub digest: supra_types::ContentHash,
    /// Size of the stored bytes, in bytes.
    pub byte_len: i64,
    /// When the snapshot was taken, milliseconds since the epoch.
    pub created_at: i64,
    /// Whether the snapshot has been undone.
    pub undone: bool,
}

/// Insert one snapshot row.
///
/// `created_at` is derived from the snapshot id's own timestamp rather than
/// read from the clock a second time. Two clock reads disagree whenever the
/// millisecond turns between them, and `ORDER BY created_at DESC` would then
/// rank a snapshot by a timestamp its id contradicts - the "newest" report
/// could name the *older* snapshot, which the flake this closed proved by
/// producing. One source of truth: the id.
pub(super) fn insert(
    transaction: &rusqlite::Transaction<'_>,
    snapshot: SnapshotId,
    path: &str,
    before: &[u8],
    digest: &supra_types::ContentHash,
) -> Result<(), supra_store::StoreError> {
    transaction
        .prepare_cached(&format!(
            "INSERT INTO {TABLE} (snapshot_id, path, before, before_hash, byte_len, created_at, undone) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, 0)"
        ))?
        .execute(rusqlite::params![
            snapshot.to_string(),
            path,
            before,
            digest.as_bytes().as_slice(),
            i64::try_from(before.len()).unwrap_or(i64::MAX),
            i64::try_from(snapshot.timestamp_ms()).unwrap_or(i64::MAX),
        ])?;
    Ok(())
}

/// Read one snapshot's stored bytes, digest, undo state, and path.
///
/// Returns `Ok(None)` when the id names no row. The digest is returned as
/// recorded, **not** pre-verified against the bytes: the undo path is the
/// one place verification is a decision (a mismatch is `Corrupt`, which
/// refuses rather than returns), so it happens there, where the refusal can
/// name both digests.
pub(super) fn read_row(
    transaction: &rusqlite::Transaction<'_>,
    snapshot: SnapshotId,
) -> Result<Option<(Vec<u8>, supra_types::ContentHash, bool, String)>, supra_store::StoreError> {
    let row = transaction
        .prepare_cached(&format!(
            "SELECT before, before_hash, undone, path FROM {TABLE} WHERE snapshot_id = ?1"
        ))?
        .query_row([snapshot.to_string()], |row| {
            Ok((
                row.get::<_, Vec<u8>>(0)?,
                row.get::<_, Vec<u8>>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, String>(3)?,
            ))
        })
        .optional()?;
    let Some((before, digest_bytes, undone, path)) = row else { return Ok(None) };
    let digest = decode_digest(&digest_bytes)?;
    Ok(Some((before, digest, undone != 0, path)))
}

/// Mark one snapshot undone. Returns whether a row was updated.
pub(super) fn mark_undone(
    transaction: &rusqlite::Transaction<'_>,
    snapshot: SnapshotId,
) -> Result<bool, supra_store::StoreError> {
    let updated = transaction
        .prepare_cached(&format!("UPDATE {TABLE} SET undone = 1 WHERE snapshot_id = ?1 AND undone = 0"))?
        .execute([snapshot.to_string()])?;
    Ok(updated == 1)
}

/// The newest not-yet-undone snapshot for one path, if any.
pub(super) fn newest_for_path(
    transaction: &rusqlite::Transaction<'_>,
    path: &str,
) -> Result<Option<SnapshotRow>, supra_store::StoreError> {
    let row = transaction
        .prepare_cached(&format!(
            "SELECT snapshot_id, path, before_hash, byte_len, created_at, undone \
             FROM {TABLE} WHERE path = ?1 AND undone = 0 \
             ORDER BY created_at DESC, snapshot_id DESC LIMIT 1"
        ))?
        .query_row([path], decode_row)
        .optional()?;
    Ok(row)
}

/// Count the snapshots stored for one path, undone or not.
pub(super) fn count_for_path(
    transaction: &rusqlite::Transaction<'_>,
    path: &str,
) -> Result<i64, supra_store::StoreError> {
    let count: i64 = transaction
        .prepare_cached(&format!("SELECT COUNT(*) FROM {TABLE} WHERE path = ?1"))?
        .query_row([path], |row| row.get(0))?;
    Ok(count)
}

/// The digest column, decoded and width-checked.
///
/// A stored digest of the wrong width is `Malformed` from the store's
/// vocabulary arriving in the journal's - the width is what makes the
/// corruption check meaningful, so a wrong one is a damaged file, not a
/// padding opportunity.
fn decode_digest(bytes: &[u8]) -> Result<supra_types::ContentHash, supra_store::StoreError> {
    let sized: [u8; supra_types::ContentHash::LEN] =
        bytes.try_into().map_err(|_| supra_store::StoreError::Malformed {
            detail: format!(
                "journal_snapshot records a {}-byte digest, expected {}",
                bytes.len(),
                supra_types::ContentHash::LEN
            ),
        })?;
    Ok(supra_types::ContentHash::from_bytes(sized))
}

fn decode_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<SnapshotRow> {
    let id: String = row.get(0)?;
    let path: String = row.get(1)?;
    let digest_bytes: Vec<u8> = row.get(2)?;
    let byte_len: i64 = row.get(3)?;
    let created_at: i64 = row.get(4)?;
    let undone: i64 = row.get(5)?;
    let snapshot = id.parse::<SnapshotId>().ok().ok_or_else(|| {
        rusqlite::Error::FromSqlConversionFailure(
            0,
            rusqlite::types::Type::Text,
            Box::new(std::io::Error::new(std::io::ErrorKind::InvalidData, "not a ULID")),
        )
    })?;
    let sized: [u8; supra_types::ContentHash::LEN] = digest_bytes.try_into().map_err(|_| {
        rusqlite::Error::FromSqlConversionFailure(
            2,
            rusqlite::types::Type::Blob,
            Box::new(std::io::Error::new(std::io::ErrorKind::InvalidData, "wrong digest width")),
        )
    })?;
    Ok(SnapshotRow {
        snapshot,
        path,
        digest: supra_types::ContentHash::from_bytes(sized),
        byte_len,
        created_at,
        undone: undone != 0,
    })
}

#[cfg(test)]
mod tests {
    use supra_store::Store;

    use super::*;
    use crate::journal::snapshot_digest;

    fn store() -> Store {
        let store = Store::open_in_memory().expect("store");
        store.migrate_component(COMPONENT, MIGRATIONS).expect("migrate");
        store
    }

    fn tx<F, T>(store: &Store, work: F) -> T
    where
        F: FnOnce(&rusqlite::Transaction<'_>) -> T,
        T: Default,
    {
        store
            .with_transaction::<T, supra_store::StoreError>(
                rusqlite::TransactionBehavior::Immediate,
                |transaction| Ok(work(transaction)),
            )
            .expect("transaction")
    }

    #[test]
    fn a_wrong_width_digest_is_refused_by_the_schema() {
        // The CHECK is the invariant; a code path that forgot to hash cannot
        // write a row that pretends it did. Probed through raw SQL, because
        // the API always writes the right width by construction.
        let connection = rusqlite::Connection::open_in_memory().expect("memory");
        for step in MIGRATIONS {
            connection.execute_batch(step.sql).expect("migrate");
        }
        let error = connection
            .execute(
                &format!(
                    "INSERT INTO {TABLE} (snapshot_id, path, before, before_hash, byte_len, created_at, undone) \
                     VALUES ('00000000000000000000000000', '/x', X'00', zeroblob(31), 1, 0, 0)"
                ),
                [],
            )
            .is_err();
        assert!(error, "a 31-byte digest must be refused");
    }

    #[test]
    fn byte_len_must_agree_with_the_blob() {
        let connection = rusqlite::Connection::open_in_memory().expect("memory");
        for step in MIGRATIONS {
            connection.execute_batch(step.sql).expect("migrate");
        }
        let error = connection
            .execute(
                &format!(
                    "INSERT INTO {TABLE} (snapshot_id, path, before, before_hash, byte_len, created_at, undone) \
                     VALUES ('00000000000000000000000000', '/x', X'00', zeroblob(32), 5, 0, 0)"
                ),
                [],
            )
            .is_err();
        assert!(error, "a disagreeing byte_len must be refused");
    }

    #[test]
    fn insert_read_and_mark_round_trip() {
        let store = store();
        let id = SnapshotId::generate();
        let body = b"the original bytes";

        tx(&store, |transaction| {
            insert(transaction, id, "/a.rs", body, &snapshot_digest(body)).expect("insert");
        });

        tx(&store, |transaction| {
            let (before, digest, undone, path) = read_row(transaction, id).expect("read").expect("present");
            assert_eq!(before, body);
            assert_eq!(digest, snapshot_digest(body));
            assert!(!undone, "fresh snapshots are not undone");
            assert_eq!(path, "/a.rs");
        });

        tx(&store, |transaction| {
            assert!(mark_undone(transaction, id).expect("mark"));
            assert!(!mark_undone(transaction, id).expect("mark again"), "second mark finds nothing");
        });

        tx(&store, |transaction| {
            let (_, _, undone, _) = read_row(transaction, id).expect("read").expect("present");
            assert!(undone, "the mark persisted");
        });
    }

    #[test]
    fn read_row_returns_the_digest_as_recorded() {
        // The undo path verifies the digest against the bytes; this test
        // pins that read_row hands both sides over without pre-verification,
        // so the verification the caller performs is real, not already-done.
        let store = store();
        let id = SnapshotId::generate();
        tx(&store, |transaction| {
            insert(transaction, id, "/a.rs", b"bytes", &snapshot_digest(b"bytes")).expect("insert");
        });
        tx(&store, |transaction| {
            let (before, stored, _, _) = read_row(transaction, id).expect("read").expect("present");
            assert_eq!(stored, snapshot_digest(&before), "the digest matches the bytes");
        });
    }

    #[test]
    fn newest_for_path_orders_by_creation_then_id() {
        let store = store();
        let early = SnapshotId::generate();
        let late = SnapshotId::generate();
        // Same millisecond is common in tests; the id is the tiebreak, so
        // force the ordering through it rather than sleeping.
        let (first, second) = if early < late { (early, late) } else { (late, early) };
        tx(&store, |transaction| {
            insert(transaction, first, "/a.rs", b"one", &snapshot_digest(b"one")).expect("insert");
        });
        tx(&store, |transaction| {
            insert(transaction, second, "/a.rs", b"two", &snapshot_digest(b"two")).expect("insert");
        });
        tx(&store, |transaction| {
            let newest = newest_for_path(transaction, "/a.rs").expect("read").expect("present");
            assert_eq!(newest.snapshot, second, "the later id wins the tiebreak");
            assert_eq!(count_for_path(transaction, "/a.rs").expect("count"), 2);
        });
    }

    #[test]
    fn a_missing_id_reads_as_none() {
        let store = store();
        tx(&store, |transaction| {
            assert!(read_row(transaction, SnapshotId::generate()).expect("read").is_none());
            assert!(newest_for_path(transaction, "/nope").expect("read").is_none());
        });
    }
}
