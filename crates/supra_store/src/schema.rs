//! Versioned, forward-only schema migrations.
//!
//! # Two ledgers, and why there have to be two
//!
//! `user_version` is a **single 32-bit slot per file**, so it can serve exactly one owner.
//! It serves this crate's core schema - the verbatim turn store - and it is also the
//! bootstrap: a reader has to know the file is a supra store, and at which core version,
//! before it can trust that anything else in the file exists.
//!
//! Every other owner of tables in this file - T11's vector index, T16.6's journal - records
//! its own version in [`COMPONENT_TABLE`], through [`migrate_component`]. That keeps each
//! stage's table definitions in the stage that owns their meaning, while the file still has
//! one schema history. The alternative, a single list here holding every downstream stage's
//! DDL, would put T11 and T16.6 inside T10.
//!
//! Both ledgers have identical semantics: forward only, refuse a newer file, and one
//! transaction per step carrying the DDL and the version bump together.
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
//! version bump. SQLite rolls those back together, so a failed migration leaves the
//! store exactly where it was rather than half-applied at an unrecorded version.
//!
//! That was checked against the bundled SQLite (3.53) before this file was written, and
//! there is a test here that fails a migration deliberately and asserts the version did
//! not move - because "SQLite has transactional DDL" is the sort of claim that is true
//! until some pragma makes it not. A second test does the same for a `CREATE VIRTUAL TABLE
//! ... USING fts5`, because a virtual table's DDL runs the module's own constructor and
//! there is no reason to assume it inherits the guarantee.
//!
//! # Adding one
//!
//! Append to the migration list below, or to the component's own list. Never edit an entry that has
//! shipped: a store in the field is already at that version and will not re-run it, so an
//! edit changes what new stores get and leaves old ones different for ever.

use rusqlite::{Connection, OptionalExtension as _, Transaction};

use crate::error::StoreError;

/// One schema step.
struct Migration {
    /// The `user_version` this step produces.
    version: u32,
    /// What it does. Runs as a batch, inside a transaction.
    sql: &'static str,
}

/// Where each non-core owner's schema version is recorded. See the module documentation.
pub const COMPONENT_TABLE: &str = "schema_component";

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
const MIGRATIONS: &[Migration] = &[
    Migration {
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
    },
    Migration {
        version: 2,
        sql: "
        -- One row per non-core owner of tables in this file. See the module documentation
        -- for why `user_version` cannot serve them: it is one slot, and there is more than
        -- one owner.
        CREATE TABLE schema_component (
            name    TEXT    PRIMARY KEY NOT NULL
                    CHECK (length(name) > 0),
            -- 0 means registered but not yet migrated, which never persists: the row is
            -- written by the same transaction that applies a step.
            version INTEGER NOT NULL
                    CHECK (version >= 0)
        ) STRICT;
    ",
    },
];

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

/// One schema step belonging to a component rather than to the core store.
///
/// A component declares its own list in its own crate, so its table definitions live beside
/// the code that gives them meaning.
#[derive(Clone, Copy, Debug)]
pub struct ComponentMigration {
    /// The version this step produces. Must be dense from 1, as the core list is.
    pub version: u32,
    /// What it does. Runs as a batch, inside a transaction.
    pub sql: &'static str,
}

/// Read the schema version recorded for one component. Zero when it has never migrated.
///
/// # Errors
///
/// [`StoreError::Malformed`] when the recorded value is not a version, or any database
/// failure. Reading before the core schema created [`COMPONENT_TABLE`] is a database
/// failure, not zero: a missing table means the file is older than this build expects, and
/// answering "version 0" would invite a component to re-apply its first migration onto
/// tables that may already exist.
pub fn component_version(connection: &Connection, component: &str) -> Result<u32, StoreError> {
    let version: Option<i64> = connection
        .query_row(&format!("SELECT version FROM {COMPONENT_TABLE} WHERE name = ?1"), [component], |row| {
            row.get(0)
        })
        .optional()?;

    match version {
        None => Ok(0),
        Some(found) => u32::try_from(found).map_err(|_| StoreError::Malformed {
            detail: format!("component {component:?} records version {found}, which is not a version"),
        }),
    }
}

/// The version query as [`apply_component`] needs it: `rusqlite`'s own
/// error, because that function reports through it.
fn component_version_raw(connection: &Connection, component: &str) -> Result<u32, rusqlite::Error> {
    let version: Option<i64> = connection
        .query_row(&format!("SELECT version FROM {COMPONENT_TABLE} WHERE name = ?1"), [component], |row| {
            row.get(0)
        })
        .optional()?;
    match version {
        None => Ok(0),
        Some(found) => u32::try_from(found).map_err(|_| {
            rusqlite::Error::FromSqlConversionFailure(
                0,
                rusqlite::types::Type::Integer,
                format!("component {component:?} records version {found}, which is not a version").into(),
            )
        }),
    }
}

/// Bring one component's tables up to the last version in `migrations`.
///
/// Same semantics as [`migrate`]: forward only, a newer file is refused, and each step's DDL
/// and version bump share one transaction.
///
/// # Errors
///
/// [`StoreError::ComponentSchemaTooNew`] when the file is ahead of this build,
/// [`StoreError::ComponentMigrate`] when a step fails - in which case the recorded version
/// is unchanged.
///
/// # Panics
///
/// Never. `migrations` is validated for density by [`assert_dense`] in each component's own
/// test, not here, because a malformed list is a programming error rather than a file state.
pub fn migrate_component(
    connection: &Connection,
    path: &std::path::Path,
    component: &str,
    migrations: &[ComponentMigration],
) -> Result<u32, StoreError> {
    let version = component_version(connection, component)?;
    let latest = migrations.last().map_or(0, |migration| migration.version);

    if version > latest {
        return Err(StoreError::ComponentSchemaTooNew {
            path: path.to_path_buf(),
            component: component.to_owned(),
            found: version,
            supported: latest,
        });
    }

    let mut version = version;
    for migration in migrations {
        version = apply_component(connection, component, migration).map_err(|source| {
            StoreError::ComponentMigrate {
                component: component.to_owned(),
                from: version,
                to: migration.version,
                source,
            }
        })?;
    }

    Ok(version)
}

/// Run one component migration, DDL and version row together.
///
/// `new_unchecked` because the connection is reached through a shared
/// reference: the `Store` holds it behind a mutex and hands out
/// `&Connection`. The version is re-read inside the IMMEDIATE transaction
/// so a second opener that lost the race sees the winner's version and
/// skips instead of re-running applied DDL.
fn apply_component(
    connection: &Connection,
    component: &str,
    migration: &ComponentMigration,
) -> Result<u32, rusqlite::Error> {
    let transaction = Transaction::new_unchecked(connection, rusqlite::TransactionBehavior::Immediate)?;
    let recorded = component_version_raw(&transaction, component)?;
    if recorded >= migration.version {
        return Ok(recorded);
    }
    transaction.execute_batch(migration.sql)?;
    transaction.execute(
        &format!(
            "INSERT INTO {COMPONENT_TABLE} (name, version) VALUES (?1, ?2) \
             ON CONFLICT(name) DO UPDATE SET version = excluded.version"
        ),
        rusqlite::params![component, i64::from(migration.version)],
    )?;
    transaction.commit()?;
    Ok(migration.version)
}

/// Assert at compile time that a migration list is ordered and dense from 1.
///
/// A gap makes "apply everything above the current version" ambiguous, and an out-of-order
/// entry silently never runs. Both are programming errors, so they are compile errors:
/// `const { supra_store::schema::assert_dense(MIGRATIONS) }` in the component that owns the
/// list. A test would report the same fault one build step later, on a list that has already
/// compiled into something.
///
/// # Panics
///
/// At compile time when the list is empty or a version is not its 1-based index. Called from
/// a runtime path it panics there instead, which is why it is only ever used in a `const`.
pub const fn assert_dense(migrations: &[ComponentMigration]) {
    assert!(!migrations.is_empty(), "a component with no migrations should not register one");
    // Counted up rather than derived from the index, so no `usize` to `u32` cast appears: the
    // cast would be lossless in practice and the lint would still be right that nothing here
    // says so.
    let mut expected: u32 = 1;
    let mut index = 0;
    while index < migrations.len() {
        assert!(
            migrations[index].version == expected,
            "component migration versions must be dense from 1, in order"
        );
        expected += 1;
        index += 1;
    }
}

/// Bring the schema up to [`latest_version`].
///
/// # Errors
///
/// [`StoreError::SchemaTooNew`] when the file is ahead of this build,
/// [`StoreError::Migrate`] when a step fails - in which case the version is unchanged.
pub fn migrate(connection: &mut Connection, path: &std::path::Path) -> Result<u32, StoreError> {
    let version = current_version(connection)?;
    let latest = latest_version();

    if version > latest {
        return Err(StoreError::SchemaTooNew { path: path.to_path_buf(), found: version, supported: latest });
    }

    let mut version = version;
    for migration in MIGRATIONS {
        version = apply_once(connection, migration, version)?;
    }

    Ok(version)
}

/// Apply one migration, re-reading the recorded version under the write
/// lock.
///
/// Two processes can open one file and both read `user_version` before
/// either migrates. The re-read inside the IMMEDIATE transaction is what
/// keeps the loser safe: it waits for the winner's commit, sees the new
/// version, and skips - where a version read outside the transaction
/// would have it re-run applied DDL and fail.
fn apply_once(connection: &mut Connection, migration: &Migration, version: u32) -> Result<u32, StoreError> {
    let transaction = connection
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .map_err(|source| StoreError::Migrate { from: version, to: migration.version, source })?;
    let recorded = current_version(&transaction)?;
    if recorded >= migration.version {
        return Ok(recorded);
    }
    transaction
        .execute_batch(migration.sql)
        .and_then(|()| transaction.pragma_update(None, "user_version", i64::from(migration.version)))
        .map_err(|source| StoreError::Migrate { from: recorded, to: migration.version, source })?;
    transaction.commit().map_err(|source| StoreError::Migrate {
        from: recorded,
        to: migration.version,
        source,
    })?;
    Ok(migration.version)
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
        let result = apply_once(&mut connection, &broken, before);
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

    // ---------------------------------------------------------------- the component ledger

    const PROBE: &[ComponentMigration] = &[
        ComponentMigration { version: 1, sql: "CREATE TABLE probe_one (x INTEGER) STRICT;" },
        ComponentMigration { version: 2, sql: "CREATE TABLE probe_two (y TEXT) STRICT;" },
    ];

    fn migrated() -> Connection {
        let mut connection = memory();
        migrate(&mut connection, Path::new("<memory>")).expect("migrate");
        connection
    }

    fn table_exists(connection: &Connection, name: &str) -> bool {
        let count: i64 = connection
            .query_row(
                "SELECT count(*) FROM sqlite_master WHERE type IN ('table','view') AND name = ?1",
                [name],
                |row| row.get(0),
            )
            .expect("query");
        count == 1
    }

    #[test]
    fn the_core_schema_creates_the_component_table() {
        // Every component reads its version from here, so its absence is not a state a
        // component can recover from - which is why `component_version` errors rather than
        // answering zero when it is missing.
        assert!(table_exists(&migrated(), COMPONENT_TABLE));
    }

    #[test]
    fn a_component_migrates_and_records_its_version() {
        let connection = migrated();
        let reached = migrate_component(&connection, Path::new("<memory>"), "probe", PROBE).expect("migrate");
        assert_eq!(reached, 2);
        assert_eq!(component_version(&connection, "probe").expect("read"), 2);
        assert!(table_exists(&connection, "probe_one"));
        assert!(table_exists(&connection, "probe_two"));
    }

    #[test]
    fn two_concurrent_openers_migrate_one_file_without_conflict() {
        let path = std::env::temp_dir().join(format!(
            "supra-migrate-race-{}-{}.db",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |elapsed| elapsed.subsec_nanos())
        ));
        let _ = std::fs::remove_file(&path);

        let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
        let mut joins = Vec::new();
        for _ in 0..2 {
            let barrier = std::sync::Arc::clone(&barrier);
            let path = path.clone();
            joins.push(std::thread::spawn(move || {
                barrier.wait();
                let mut connection = Connection::open(&path).expect("open");
                connection.pragma_update(None, "busy_timeout", 5_000).expect("timeout");
                migrate(&mut connection, &path).expect("core");
                migrate_component(&connection, &path, "race", PROBE)
            }));
        }
        for join in joins {
            let reached = join.join().expect("thread").expect("migrate");
            assert_eq!(reached, 2, "the loser must see the winner's version, not re-run DDL");
        }

        let check = Connection::open(&path).expect("reopen");
        assert_eq!(component_version(&check, "race").expect("read"), 2);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn a_component_that_has_never_migrated_is_at_zero() {
        assert_eq!(component_version(&migrated(), "never-seen").expect("read"), 0);
    }

    #[test]
    fn migrating_a_component_twice_changes_nothing() {
        let connection = migrated();
        migrate_component(&connection, Path::new("<memory>"), "probe", PROBE).expect("first");
        let again = migrate_component(&connection, Path::new("<memory>"), "probe", PROBE).expect("second");
        assert_eq!(again, 2);
    }

    #[test]
    fn a_component_resumes_from_where_it_stopped() {
        // The upgrade path: a store created by a build that only had version 1 must get
        // version 2 and nothing else.
        let connection = migrated();
        migrate_component(&connection, Path::new("<memory>"), "probe", &PROBE[..1]).expect("first");
        assert_eq!(component_version(&connection, "probe").expect("read"), 1);
        assert!(!table_exists(&connection, "probe_two"));

        let reached = migrate_component(&connection, Path::new("<memory>"), "probe", PROBE).expect("resume");
        assert_eq!(reached, 2);
        assert!(table_exists(&connection, "probe_two"));
    }

    #[test]
    fn components_do_not_see_each_other() {
        // Two owners in one file, each with its own history. If they shared a counter, one
        // stage shipping a migration would make every other stage think it had regressed.
        let connection = migrated();
        migrate_component(&connection, Path::new("<memory>"), "probe", PROBE).expect("probe");
        assert_eq!(component_version(&connection, "probe").expect("read"), 2);
        assert_eq!(component_version(&connection, "other").expect("read"), 0);
    }

    #[test]
    fn a_component_ahead_of_this_build_is_refused() {
        let connection = migrated();
        migrate_component(&connection, Path::new("<memory>"), "probe", PROBE).expect("migrate");

        let error = migrate_component(&connection, Path::new("/tmp/supra.db"), "probe", &PROBE[..1])
            .expect_err("a newer component must be refused");
        match error {
            StoreError::ComponentSchemaTooNew { component, found, supported, .. } => {
                assert_eq!(component, "probe");
                assert_eq!(found, 2);
                assert_eq!(supported, 1);
            }
            other => panic!("expected ComponentSchemaTooNew, got {other}"),
        }
    }

    #[test]
    fn a_failing_component_migration_leaves_the_version_where_it_was() {
        // The same claim as the core path, asserted separately: this one commits through
        // `Transaction::new_unchecked` on a shared reference rather than through `&mut`, and
        // that is a different code path.
        let connection = migrated();
        let broken: &[ComponentMigration] = &[ComponentMigration {
            version: 1,
            sql: "CREATE TABLE half_applied (x INTEGER); SELECT this_function_does_not_exist();",
        }];

        let error = migrate_component(&connection, Path::new("<memory>"), "probe", broken)
            .expect_err("the migration was supposed to fail");
        assert!(matches!(error, StoreError::ComponentMigrate { from: 0, to: 1, .. }), "{error}");
        assert_eq!(component_version(&connection, "probe").expect("read"), 0);
        assert!(!table_exists(&connection, "half_applied"), "the DDL was not rolled back");
    }

    #[test]
    fn a_failing_second_step_keeps_the_first() {
        // Forward-only means partial progress is real progress: the first step stays applied
        // and recorded, so the next run resumes rather than starting over.
        let connection = migrated();
        let broken: &[ComponentMigration] =
            &[PROBE[0], ComponentMigration { version: 2, sql: "SELECT this_function_does_not_exist();" }];

        let error = migrate_component(&connection, Path::new("<memory>"), "probe", broken)
            .expect_err("step two was supposed to fail");
        assert!(matches!(error, StoreError::ComponentMigrate { from: 1, to: 2, .. }), "{error}");
        assert_eq!(component_version(&connection, "probe").expect("read"), 1);
        assert!(table_exists(&connection, "probe_one"));
    }

    #[test]
    fn creating_an_fts5_table_rolls_back_with_its_transaction() {
        // T11 puts a `CREATE VIRTUAL TABLE ... USING fts5` in a component migration. A
        // virtual table's DDL runs the module's own constructor, which writes shadow tables
        // of its own, so there is no reason to assume it inherits ordinary DDL's rollback.
        // Measured here rather than assumed, because a half-created FTS5 table would leave
        // the component at version 0 with shadow tables already present - and the next run
        // would fail on a name that already exists, for ever.
        let connection = migrated();
        let broken: &[ComponentMigration] = &[ComponentMigration {
            version: 1,
            sql: "CREATE VIRTUAL TABLE probe_fts USING fts5(body); \
                  SELECT this_function_does_not_exist();",
        }];

        let error = migrate_component(&connection, Path::new("<memory>"), "probe", broken)
            .expect_err("the migration was supposed to fail");
        assert!(matches!(error, StoreError::ComponentMigrate { .. }), "{error}");
        assert_eq!(component_version(&connection, "probe").expect("read"), 0);

        // Not only the table: FTS5 creates `probe_fts_data`, `_idx`, `_content`, `_docsize`
        // and `_config` beside it. Any survivor would block the retry.
        let leftovers: i64 = connection
            .query_row("SELECT count(*) FROM sqlite_master WHERE name LIKE 'probe_fts%'", [], |row| {
                row.get(0)
            })
            .expect("query");
        assert_eq!(leftovers, 0, "an fts5 table or one of its shadow tables survived the rollback");
    }

    #[test]
    fn a_component_migration_can_create_an_fts5_table() {
        // The other half: it has to work when nothing fails.
        let connection = migrated();
        let fts: &[ComponentMigration] =
            &[ComponentMigration { version: 1, sql: "CREATE VIRTUAL TABLE probe_fts USING fts5(body);" }];
        migrate_component(&connection, Path::new("<memory>"), "probe", fts).expect("migrate");
        connection.execute("INSERT INTO probe_fts (body) VALUES ('fn recall_turn')", []).expect("insert");

        // bm25 is negative and better matches are *more* negative, so ranking ascending is
        // correct and taking an absolute value would invert it.
        let (rowid, score): (i64, f64) = connection
            .query_row(
                "SELECT rowid, bm25(probe_fts) FROM probe_fts WHERE probe_fts MATCH 'recall' \
                 ORDER BY bm25(probe_fts)",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("query");
        assert_eq!(rowid, 1);
        assert!(score < 0.0, "bm25 was {score}, expected a negative score");
    }

    #[test]
    fn a_component_version_read_before_the_core_schema_is_an_error() {
        // Answering zero would invite a component to re-apply its first migration onto
        // tables that may already exist. A missing component table means the file is older
        // than this build expects, which is a different problem with a different remedy.
        let connection = memory();
        let error = component_version(&connection, "probe").expect_err("no component table yet");
        assert!(matches!(error, StoreError::Sqlite(_)), "{error}");
    }

    #[test]
    fn an_empty_component_name_is_refused_by_the_schema() {
        // The name is the key. An empty one would collide with the next caller that passes
        // an empty one, silently sharing a version counter between two owners.
        let connection = migrated();
        assert!(
            connection
                .execute(&format!("INSERT INTO {COMPONENT_TABLE} (name, version) VALUES ('', 1)"), [])
                .is_err(),
            "an empty component name must be refused"
        );
    }

    #[test]
    fn a_negative_component_version_is_reported_rather_than_wrapped() {
        let connection = migrated();
        // Bypass the CHECK the same way a hand-edited file would: there is no path in this
        // crate that writes a negative version, so the row is forced in with the constraint
        // suspended.
        connection.execute_batch("PRAGMA ignore_check_constraints = ON").expect("relax");
        connection
            .execute(&format!("INSERT INTO {COMPONENT_TABLE} (name, version) VALUES ('probe', -1)"), [])
            .expect("insert");
        connection.execute_batch("PRAGMA ignore_check_constraints = OFF").expect("restore");

        let error = component_version(&connection, "probe").expect_err("not a version");
        assert!(error.to_string().contains("not a version"), "{error}");
    }
}
