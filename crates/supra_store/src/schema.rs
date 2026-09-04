//! Versioned, forward-only schema migrations.
//!
//! # Forward only
//!
//! There are no down migrations. A store written by a newer supra is **refused**, not
//! downgraded: a later schema may keep the same table name with different meaning, and
//! reading it with older code would not fail - it would silently misinterpret. Refusing
//! turns a confusing wrong answer into a clear message telling the reader to upgrade.
//!
//! # Atomic, verified rather than assumed
//!
//! Each migration runs in one transaction that carries both the DDL and the
//! `user_version` bump. SQLite rolls those back together, so a failed migration leaves the
//! store exactly where it was rather than half-applied at an unrecorded version.
//!
//! That was checked against the bundled SQLite (3.53) before this file was written, and
//! there is a test here that fails a migration deliberately and asserts the version did
//! not move - because "SQLite has transactional DDL" is the sort of claim that is true
//! until some pragma makes it not.
//!
//! # Adding one
//!
//! Append to [`MIGRATIONS`]. Never edit an entry that has shipped: a store in the field is
//! already at that version and will not re-run it, so an edit changes what new stores get
//! and leaves old ones different for ever.

use rusqlite::{Connection, Transaction};

use crate::error::StoreError;

/// One schema step.
struct Migration {
    /// The `user_version` this step produces.
    version: u32,
    /// What it does. Runs as a batch, inside a transaction.
    sql: &'static str,
}

/// Every schema step, in order.
///
/// Version 1 creates the verbatim turn table that invariant I4 rests on.
///
/// `STRICT` is used, and it is **not sufficient** on its own. A probe appeared to show it
/// rejecting an integer in a `TEXT` column; a test then contradicted that, and the
/// `sqlite3` shell settled it: a STRICT `TEXT` column *accepts* an integer and converts it
/// to text, while a STRICT `BLOB` column refuses one. The probe's rejection had come from
/// the blob beside the id, not from the id.
///
/// So the invariants the code depends on are written as `CHECK` constraints, where no code
/// path can bypass them. A turn id that is not 26 characters, a digest that is not 32
/// bytes, or a length column that disagrees with the blob it describes is refused at write
/// time rather than discovered at recall.
const MIGRATIONS: &[Migration] = &[Migration {
    version: 1,
    sql: "
        CREATE TABLE evicted_turn (
            -- The 26-character Crockford form rather than 16 opaque bytes: it sorts
            -- lexicographically in SQL the same way it sorts by time, and it is readable
            -- when someone opens the file with the sqlite3 shell during an incident.
            turn_id    TEXT    PRIMARY KEY NOT NULL
                       CHECK (length(turn_id) = 26),
            -- The turn exactly as it was sent. BLOB, not TEXT: TEXT invites an encoding
            -- assumption, and I4 promises these bytes back unchanged.
            body       BLOB    NOT NULL,
            -- Digest of `body` alone, so a damaged row is detected on read rather than
            -- returned. Never covers the metadata beside it. A wrong-length digest would
            -- make that verification meaningless, so the width is enforced here.
            body_hash  BLOB    NOT NULL
                       CHECK (length(body_hash) = 32),
            -- Redundant with length(body), and kept anyway: a size query that does not
            -- have to read every blob is what makes a store-size report cheap. The CHECK
            -- is what keeps the redundancy from becoming a second, disagreeing answer.
            byte_len   INTEGER NOT NULL
                       CHECK (byte_len = length(body)),
            -- Diagnostics only. Deliberately outside the digest, so the same turn evicted
            -- at two different times still verifies.
            stored_at  INTEGER NOT NULL
                       CHECK (stored_at >= 0)
        ) STRICT;

        -- Eviction order, for a report that wants the oldest first without a full scan.
        CREATE INDEX evicted_turn_stored_at ON evicted_turn (stored_at);
    ",
}];

/// The highest schema version this build understands.
#[must_use]
pub fn latest_version() -> u32 {
    MIGRATIONS.last().map_or(0, |migration| migration.version)
}

/// Read the schema version recorded in the file.
///
/// # Errors
///
/// Any failure reading the pragma.
pub fn current_version(connection: &Connection) -> Result<u32, StoreError> {
    let version: i64 = connection.pragma_query_value(None, "user_version", |row| row.get(0))?;
    u32::try_from(version).map_err(|_| StoreError::Malformed {
        detail: format!("user_version is {version}, which is not a schema version"),
    })
}

/// Bring the schema up to [`latest_version`].
///
/// # Errors
///
/// [`StoreError::SchemaTooNew`] when the file is ahead of this build,
/// [`StoreError::Migrate`] when a step fails - in which case the version is unchanged.
pub fn migrate(connection: &mut Connection, path: &std::path::Path) -> Result<u32, StoreError> {
    let mut version = current_version(connection)?;
    let latest = latest_version();

    if version > latest {
        return Err(StoreError::SchemaTooNew { path: path.to_path_buf(), found: version, supported: latest });
    }

    for migration in MIGRATIONS {
        if migration.version <= version {
            continue;
        }
        apply(connection, migration).map_err(|source| StoreError::Migrate {
            from: version,
            to: migration.version,
            source,
        })?;
        version = migration.version;
    }

    Ok(version)
}

/// Run one migration, DDL and version bump together.
fn apply(connection: &mut Connection, migration: &Migration) -> rusqlite::Result<()> {
    let transaction: Transaction<'_> = connection.transaction()?;
    transaction.execute_batch(migration.sql)?;
    transaction.pragma_update(None, "user_version", i64::from(migration.version))?;
    transaction.commit()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn memory() -> Connection {
        Connection::open_in_memory().expect("in-memory database")
    }

    #[test]
    fn a_fresh_database_migrates_to_the_latest_version() {
        let mut connection = memory();
        assert_eq!(current_version(&connection).expect("read"), 0);

        let reached = migrate(&mut connection, Path::new("<memory>")).expect("migrate");
        assert_eq!(reached, latest_version());
        assert_eq!(current_version(&connection).expect("read"), latest_version());
    }

    #[test]
    fn migrating_twice_changes_nothing() {
        // A store is opened on every run, so this is the ordinary path rather than an edge
        // case: the second open must not re-apply anything.
        let mut connection = memory();
        migrate(&mut connection, Path::new("<memory>")).expect("first");
        let again = migrate(&mut connection, Path::new("<memory>")).expect("second");
        assert_eq!(again, latest_version());
    }

    #[test]
    fn the_first_migration_creates_the_verbatim_turn_table() {
        let mut connection = memory();
        migrate(&mut connection, Path::new("<memory>")).expect("migrate");

        let exists: i64 = connection
            .query_row(
                "SELECT count(*) FROM sqlite_master WHERE type = 'table' AND name = 'evicted_turn'",
                [],
                |row| row.get(0),
            )
            .expect("query");
        assert_eq!(exists, 1);
    }

    /// Insert a row directly, bypassing every Rust-side check, so the schema is what is
    /// under test.
    fn raw_insert(connection: &Connection, values: &str) -> rusqlite::Result<usize> {
        connection.execute(
            &format!(
                "INSERT INTO evicted_turn (turn_id, body, body_hash, byte_len, stored_at) \
                 VALUES {values}"
            ),
            [],
        )
    }

    fn valid_id() -> String {
        "0".repeat(26)
    }

    #[test]
    fn strict_alone_does_not_protect_a_text_column() {
        // Recorded as a test because a probe suggested otherwise and was wrong. A STRICT
        // TEXT column accepts an integer and converts it; the probe's rejection had come
        // from the BLOB column beside the id. This is why the CHECK constraints exist.
        let connection = memory();
        connection.execute_batch("CREATE TABLE t (a TEXT NOT NULL, b BLOB NOT NULL) STRICT").expect("create");

        assert!(
            connection.execute("INSERT INTO t (a, b) VALUES (1, X'00')", []).is_ok(),
            "a STRICT TEXT column does accept an integer"
        );
        let stored: String = connection.query_row("SELECT a FROM t", [], |row| row.get(0)).expect("read");
        assert_eq!(stored, "1", "it was converted to text");

        assert!(
            connection.execute("INSERT INTO t (a, b) VALUES ('x', 2)", []).is_err(),
            "a STRICT BLOB column does refuse an integer"
        );
    }

    #[test]
    fn a_turn_id_that_is_not_a_ulid_is_refused_by_the_schema() {
        // The failure this prevents: an id stored as an integer becomes the text "1", which
        // parses as nothing, and the discovery happens at recall.
        let mut connection = memory();
        migrate(&mut connection, Path::new("<memory>")).expect("migrate");

        assert!(
            raw_insert(&connection, "(1, X'00', zeroblob(32), 1, 0)").is_err(),
            "an integer turn_id becomes a one-character string and must be refused"
        );
        assert!(
            raw_insert(&connection, "('too-short', X'00', zeroblob(32), 1, 0)").is_err(),
            "a short turn_id must be refused"
        );
        assert!(
            raw_insert(&connection, &format!("('{}', X'00', zeroblob(32), 1, 0)", valid_id())).is_ok(),
            "a 26-character id is accepted"
        );
    }

    #[test]
    fn a_wrong_width_digest_is_refused() {
        // A digest of the wrong length would make the corruption check meaningless.
        let mut connection = memory();
        migrate(&mut connection, Path::new("<memory>")).expect("migrate");

        for digest in ["zeroblob(31)", "zeroblob(33)", "X''"] {
            assert!(
                raw_insert(&connection, &format!("('{}', X'00', {digest}, 1, 0)", valid_id())).is_err(),
                "{digest} must be refused"
            );
        }
    }

    #[test]
    fn a_length_column_that_disagrees_with_its_blob_is_refused() {
        // The point of keeping a redundant column is a cheap size query. The point of the
        // CHECK is that the redundancy cannot become a second, disagreeing answer.
        let mut connection = memory();
        migrate(&mut connection, Path::new("<memory>")).expect("migrate");

        assert!(
            raw_insert(&connection, &format!("('{}', X'0011', zeroblob(32), 5, 0)", valid_id())).is_err(),
            "byte_len must match length(body)"
        );
        assert!(
            raw_insert(&connection, &format!("('{}', X'0011', zeroblob(32), 2, 0)", valid_id())).is_ok(),
            "and is accepted when it does"
        );
    }

    #[test]
    fn a_newer_schema_is_refused_rather_than_downgraded() {
        // A later schema may reuse a table name with different meaning. Reading it with
        // older code would not fail; it would misinterpret.
        let mut connection = memory();
        let ahead = i64::from(latest_version()) + 5;
        connection.pragma_update(None, "user_version", ahead).expect("set");

        let error =
            migrate(&mut connection, Path::new("/tmp/supra.db")).expect_err("a newer store must be refused");
        match error {
            StoreError::SchemaTooNew { found, supported, .. } => {
                assert_eq!(i64::from(found), ahead);
                assert_eq!(supported, latest_version());
            }
            other => panic!("expected SchemaTooNew, got {other}"),
        }
    }

    #[test]
    fn a_failing_migration_leaves_the_version_where_it_was() {
        // "SQLite has transactional DDL" is the kind of claim that holds until a pragma
        // makes it not, so it is asserted rather than assumed: a migration whose SQL fails
        // part-way must roll back both the DDL and the version bump.
        let mut connection = memory();
        let broken = Migration {
            version: 99,
            sql: "CREATE TABLE half_applied (x INTEGER); SELECT this_function_does_not_exist();",
        };

        let before = current_version(&connection).expect("read");
        let result = apply(&mut connection, &broken);
        assert!(result.is_err(), "the migration was supposed to fail");

        assert_eq!(current_version(&connection).expect("read"), before, "the version moved");
        let leftover: i64 = connection
            .query_row(
                "SELECT count(*) FROM sqlite_master WHERE type = 'table' AND name = 'half_applied'",
                [],
                |row| row.get(0),
            )
            .expect("query");
        assert_eq!(leftover, 0, "the DDL was not rolled back with the version");
    }

    #[test]
    fn versions_are_ordered_and_dense_from_one() {
        // A gap or a repeat would make "apply everything above the current version"
        // ambiguous, and an out-of-order entry would silently never run.
        for (index, migration) in MIGRATIONS.iter().enumerate() {
            let expected = u32::try_from(index + 1).expect("small");
            assert_eq!(
                migration.version, expected,
                "migration {index} declares version {} rather than {expected}",
                migration.version
            );
        }
        assert!(latest_version() >= 1, "there must be at least one migration");
    }

    #[test]
    fn a_negative_user_version_is_reported_rather_than_wrapped() {
        // `user_version` is a signed 32-bit value in SQLite, so a hand-edited file can hold
        // something that is not a schema version at all.
        let connection = memory();
        connection.pragma_update(None, "user_version", -1_i64).expect("set");
        let error = current_version(&connection).expect_err("not a version");
        assert!(error.to_string().contains("not a schema version"), "{error}");
    }
}
