//! Durable storage for supra-harness.
//!
//! **T10** of the stage sequence: SQLite in WAL mode, versioned migrations, and the
//! verbatim turn store that invariant I4 rests on.
//!
//! # What this stage is for
//!
//! > Old turns are **not summarised**. They are evicted verbatim to SQLite and replaced by
//! > a ~15 token index entry. The `recall` tool returns the original **byte-identical**.
//!
//! Everything here serves the last word of that sentence. Turns are stored as `BLOB`s and
//! never re-rendered; every row carries a digest of its own body; and recall recomputes that
//! digest and returns [`StoreError::Corrupt`] with **no bytes** rather than handing back
//! content that is no longer what the conversation contained.
//!
//! # Durability, and the obligation it places on T14
//!
//! `synchronous = FULL` by default. In WAL mode `NORMAL` can lose the last commits on power
//! loss, and this store holds turns that the prefix has already dropped - so a lost commit
//! is a lost conversation, not a lost cache entry.
//!
//! It is affordable because eviction is rare: it happens at a generation rewrite, not per
//! turn, so the fsync never lands on the hot path.
//!
//! **T14 must commit the eviction before dropping the turn from the prefix.** No setting
//! here can make the other order safe.
//!
//! # Concurrency
//!
//! One connection behind a `Mutex`. Eviction and recall are both rare, so the contention a
//! read pool would relieve does not exist yet - and a pool that is not measured is a guess.
//! WAL still earns its place: it keeps a reader from blocking the writer, and a checkpoint
//! from blocking either.
//!
//! Within a process the mutex serialises. Across processes the eviction path takes an
//! `IMMEDIATE` transaction, so its read-then-write pair is atomic there too.
//!
//! # Usage
//!
//! ```no_run
//! use supra_store::Store;
//! use supra_types::TurnId;
//!
//! let store = Store::open("/tmp/supra/store.db")?;
//! let turn = TurnId::generate();
//!
//! store.evict_turn(turn, b"the turn exactly as it was sent")?;
//! assert_eq!(store.recall_turn(turn)?, b"the turn exactly as it was sent");
//! # Ok::<(), supra_store::StoreError>(())
//! ```

#![deny(missing_docs)]
#![forbid(unsafe_code)]
// Tests assert with `.expect()` and `panic!`, and some reach for the connection directly to
// damage a row on purpose. Scoped to `cfg(test)` so no allow reaches a shipped path.
#![cfg_attr(test, allow(clippy::expect_used, clippy::panic, clippy::print_stderr))]

pub mod error;
pub mod schema;
pub mod turns;

use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, PoisonError};

use rusqlite::Connection;

pub use error::StoreError;
pub use turns::{EvictedTurn, body_digest};

/// The store is used from the turn loop and from a recall on another task, so this is a
/// requirement rather than an observation.
const _: () = {
    const fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<Store>();
};

/// How long to wait for another writer before giving up.
///
/// Only reachable when a second supra process holds the write lock. Five seconds is long
/// enough to outlast a generation rewrite and short enough that a wedged peer surfaces as an
/// error rather than an apparent hang.
pub const DEFAULT_BUSY_TIMEOUT_MS: u32 = 5_000;

/// Durability setting.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Synchronous {
    /// Every commit reaches the disk before it is reported as committed.
    ///
    /// The default, and the only setting under which a turn the prefix has already dropped
    /// is safe from a power loss.
    #[default]
    Full,
    /// Commits survive a process crash but may be lost on power loss.
    ///
    /// Offered for a store whose contents are reconstructible. Not for the turn store.
    Normal,
}

impl Synchronous {
    /// The value SQLite expects.
    const fn pragma(self) -> &'static str {
        match self {
            Self::Full => "FULL",
            Self::Normal => "NORMAL",
        }
    }
}

/// How a store is opened.
#[derive(Clone, Copy, Debug)]
pub struct StoreOptions {
    /// Durability. See [`Synchronous`].
    pub synchronous: Synchronous,
    /// How long to wait for another writer, in milliseconds.
    pub busy_timeout_ms: u32,
}

impl Default for StoreOptions {
    fn default() -> Self {
        Self { synchronous: Synchronous::default(), busy_timeout_ms: DEFAULT_BUSY_TIMEOUT_MS }
    }
}

impl StoreOptions {
    /// Use a different durability setting.
    #[must_use]
    pub const fn with_synchronous(mut self, synchronous: Synchronous) -> Self {
        self.synchronous = synchronous;
        self
    }

    /// Wait a different length of time for another writer.
    #[must_use]
    pub const fn with_busy_timeout_ms(mut self, milliseconds: u32) -> Self {
        self.busy_timeout_ms = milliseconds;
        self
    }
}

/// A durable store.
pub struct Store {
    connection: Mutex<Connection>,
    path: PathBuf,
}

impl Store {
    /// Open or create a store at `path`, creating its parent directory and migrating it.
    ///
    /// # Errors
    ///
    /// [`StoreError::Open`] when the file cannot be opened or prepared,
    /// [`StoreError::SchemaTooNew`] when it was written by a newer supra,
    /// [`StoreError::Migrate`] when a migration fails.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, StoreError> {
        Self::open_with(path, StoreOptions::default())
    }

    /// Open or create a store with explicit options.
    ///
    /// # Errors
    ///
    /// As [`Store::open`].
    pub fn open_with(path: impl AsRef<Path>, options: StoreOptions) -> Result<Self, StoreError> {
        let path = path.as_ref().to_path_buf();

        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent).map_err(|error| StoreError::Open {
                    path: path.clone(),
                    source: rusqlite::Error::ToSqlConversionFailure(Box::new(error)),
                })?;
            }
        }

        let mut connection =
            Connection::open(&path).map_err(|source| StoreError::Open { path: path.clone(), source })?;

        Self::prepare(&connection, options, true)
            .map_err(|source| StoreError::Open { path: path.clone(), source })?;
        schema::migrate(&mut connection, &path)?;

        Ok(Self { connection: Mutex::new(connection), path })
    }

    /// Open a store that exists only for the life of the process.
    ///
    /// For tests. Note that **WAL does not apply to an in-memory database** - SQLite reports
    /// `journal_mode = memory` - so a test asserting WAL has to use a file. That was
    /// measured rather than assumed.
    ///
    /// # Errors
    ///
    /// As [`Store::open`].
    pub fn open_in_memory() -> Result<Self, StoreError> {
        let path = PathBuf::from("<memory>");
        let mut connection =
            Connection::open_in_memory().map_err(|source| StoreError::Open { path: path.clone(), source })?;

        // WAL is skipped: it is not available in memory, and asking for it would leave a
        // misleading "we set WAL" in the code for a database that cannot have it.
        Self::prepare(&connection, StoreOptions::default(), false)
            .map_err(|source| StoreError::Open { path: path.clone(), source })?;
        schema::migrate(&mut connection, &path)?;

        Ok(Self { connection: Mutex::new(connection), path })
    }

    /// Apply the pragmas a store depends on.
    fn prepare(connection: &Connection, options: StoreOptions, wal: bool) -> rusqlite::Result<()> {
        if wal {
            connection.pragma_update(None, "journal_mode", "WAL")?;
        }
        connection.pragma_update(None, "synchronous", options.synchronous.pragma())?;
        // Set explicitly rather than relying on the driver's default. rusqlite happens to
        // enable this, which would leave the schema's referential integrity depending on a
        // dependency's choice instead of on this file.
        connection.pragma_update(None, "foreign_keys", true)?;
        connection.pragma_update(None, "busy_timeout", i64::from(options.busy_timeout_ms))?;
        Ok(())
    }

    /// Where the store lives. `<memory>` for an in-memory store.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The schema version recorded in the file.
    ///
    /// # Errors
    ///
    /// Any failure reading the pragma.
    pub fn schema_version(&self) -> Result<u32, StoreError> {
        schema::current_version(&self.connection())
    }

    /// The journal mode in force, for a diagnostic that wants to confirm WAL.
    ///
    /// # Errors
    ///
    /// Any failure reading the pragma.
    pub fn journal_mode(&self) -> Result<String, StoreError> {
        let connection = self.connection();
        connection.pragma_query_value(None, "journal_mode", |row| row.get(0)).map_err(StoreError::Sqlite)
    }

    /// The connection, for the modules that speak SQL.
    ///
    /// Crate-private on purpose: the SQL surface is an implementation detail, and a caller
    /// holding a connection could take a lock this type is responsible for.
    ///
    /// Poisoning is ignored. A panic inside a query leaves SQLite consistent - it has its own
    /// transaction boundaries - and refusing every later operation because one caller
    /// panicked would turn a recoverable fault into a dead session.
    pub(crate) fn connection(&self) -> MutexGuard<'_, Connection> {
        self.connection.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// Lock-free, and therefore partial: taking the connection mutex to report a row count
/// would let a `Debug` inside a diagnostic block on a query. The connection is omitted on
/// purpose rather than by oversight.
#[allow(
    clippy::missing_fields_in_debug,
    reason = "the connection is behind a mutex that a Debug must not take"
)]
impl core::fmt::Debug for Store {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter.debug_struct("Store").field("path", &self.path).finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use supra_types::TurnId;

    struct Scratch(PathBuf);

    impl Scratch {
        fn new(name: &str) -> Self {
            let path = std::env::temp_dir().join(format!("supra-store-{name}"));
            let _ = fs::remove_dir_all(&path);
            fs::create_dir_all(&path).expect("scratch directory");
            Self(path)
        }

        fn db(&self) -> PathBuf {
            self.0.join("store.db")
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn a_file_store_runs_in_wal_mode() {
        // Asserted on a file, not in memory: an in-memory database reports
        // `journal_mode = memory` however it is opened, so the same assertion there would
        // pass for the wrong reason.
        let scratch = Scratch::new("wal");
        let store = Store::open(scratch.db()).expect("open");
        assert_eq!(store.journal_mode().expect("mode").to_lowercase(), "wal");
    }

    #[test]
    fn an_in_memory_store_reports_memory_rather_than_wal() {
        // Recorded because it is the reason the WAL assertion above needs a file.
        let store = Store::open_in_memory().expect("open");
        assert_eq!(store.journal_mode().expect("mode").to_lowercase(), "memory");
    }

    #[test]
    fn wal_leaves_its_sidecar_files_beside_the_database() {
        // Worth knowing for anything that copies or backs up a store: the database is three
        // files while a connection is open, not one.
        let scratch = Scratch::new("sidecars");
        let store = Store::open(scratch.db()).expect("open");
        store.evict_turn(TurnId::generate(), b"force a write").expect("evict");

        let mut names: Vec<String> = fs::read_dir(&scratch.0)
            .expect("read dir")
            .filter_map(|entry| entry.ok().map(|e| e.file_name().to_string_lossy().into_owned()))
            .collect();
        names.sort();
        assert!(names.contains(&"store.db".to_owned()), "{names:?}");
        assert!(names.iter().any(|name| name.ends_with("-wal")), "{names:?}");
    }

    #[test]
    fn a_missing_parent_directory_is_created() {
        let scratch = Scratch::new("nested");
        let nested = scratch.0.join("deep").join("deeper").join("store.db");
        let store = Store::open(&nested).expect("open");
        assert!(nested.exists());
        assert_eq!(store.path(), nested.as_path());
    }

    #[test]
    fn the_defaults_are_the_durable_ones() {
        // `synchronous = FULL` is not a preference here. This store holds turns the prefix
        // has already dropped, so a commit lost to power failure is a lost conversation.
        let options = StoreOptions::default();
        assert_eq!(options.synchronous, Synchronous::Full);
        assert_eq!(options.busy_timeout_ms, DEFAULT_BUSY_TIMEOUT_MS);

        let scratch = Scratch::new("synchronous");
        let store = Store::open(scratch.db()).expect("open");
        let level: i64 =
            store.connection().pragma_query_value(None, "synchronous", |row| row.get(0)).expect("read");
        assert_eq!(level, 2, "2 is FULL");
    }

    #[test]
    fn options_are_applied() {
        let scratch = Scratch::new("options");
        let store = Store::open_with(
            scratch.db(),
            StoreOptions::default().with_synchronous(Synchronous::Normal).with_busy_timeout_ms(250),
        )
        .expect("open");

        let connection = store.connection();
        let level: i64 = connection.pragma_query_value(None, "synchronous", |row| row.get(0)).expect("read");
        assert_eq!(level, 1, "1 is NORMAL");
        let timeout: i64 =
            connection.pragma_query_value(None, "busy_timeout", |row| row.get(0)).expect("read");
        assert_eq!(timeout, 250);
    }

    #[test]
    fn foreign_keys_are_set_here_rather_than_inherited() {
        // rusqlite happens to enable them. Setting it explicitly means referential integrity
        // does not depend on a dependency's default.
        let store = Store::open_in_memory().expect("open");
        let enabled: i64 =
            store.connection().pragma_query_value(None, "foreign_keys", |row| row.get(0)).expect("read");
        assert_eq!(enabled, 1);
    }

    #[test]
    fn a_store_migrates_on_open_and_reopen() {
        let scratch = Scratch::new("reopen");
        let turn = TurnId::generate();

        {
            let store = Store::open(scratch.db()).expect("first open");
            assert_eq!(store.schema_version().expect("version"), schema::latest_version());
            store.evict_turn(turn, b"survives a restart").expect("evict");
        }

        let store = Store::open(scratch.db()).expect("second open");
        assert_eq!(store.schema_version().expect("version"), schema::latest_version());
        assert_eq!(store.recall_turn(turn).expect("recall"), b"survives a restart");
    }

    #[test]
    fn a_store_written_by_a_newer_supra_is_refused() {
        let scratch = Scratch::new("too-new");
        {
            let store = Store::open(scratch.db()).expect("open");
            let ahead = i64::from(schema::latest_version()) + 1;
            store.connection().pragma_update(None, "user_version", ahead).expect("bump the version");
        }

        let error = Store::open(scratch.db()).expect_err("must be refused");
        assert!(matches!(error, StoreError::SchemaTooNew { .. }), "{error}");
        assert!(error.to_string().contains("newer supra"), "{error}");
    }

    #[test]
    fn the_store_is_usable_from_several_threads() {
        // The turn loop evicts while a recall reads; both go through one connection behind a
        // mutex, and neither may corrupt the other.
        use std::sync::Arc;

        let store = Arc::new(Store::open_in_memory().expect("open"));
        let turns: Vec<TurnId> = (0..8).map(|_| TurnId::generate()).collect();

        std::thread::scope(|scope| {
            for (index, turn) in turns.iter().copied().enumerate() {
                let store = Arc::clone(&store);
                scope.spawn(move || {
                    let body = vec![u8::try_from(index).expect("small"); 64];
                    store.evict_turn(turn, &body).expect("evict");
                    assert_eq!(store.recall_turn(turn).expect("recall"), body);
                });
            }
        });

        assert_eq!(store.evicted_turn_count().expect("count"), 8);
    }

    #[test]
    fn a_panicking_caller_does_not_poison_the_store() {
        // A panic inside a query leaves SQLite consistent - it has its own transaction
        // boundaries - so refusing every later operation would turn a recoverable fault into
        // a dead session.
        use std::sync::Arc;

        let store = Arc::new(Store::open_in_memory().expect("open"));
        let turn = TurnId::generate();
        store.evict_turn(turn, b"before the panic").expect("evict");

        let poisoner = {
            let store = Arc::clone(&store);
            std::thread::spawn(move || {
                let _guard = store.connection();
                panic!("a caller panicked while holding the connection");
            })
        };
        assert!(poisoner.join().is_err(), "the thread was supposed to panic");

        assert_eq!(store.recall_turn(turn).expect("recall"), b"before the panic");
        store.evict_turn(TurnId::generate(), b"after the panic").expect("still usable");
    }
}
