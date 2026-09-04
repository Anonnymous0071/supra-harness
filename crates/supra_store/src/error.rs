//! Errors, each naming what a reader can do about it.
//!
//! One variant carries more weight than the others. [`StoreError::Corrupt`] is how
//! invariant I4's promise - that a recalled turn comes back **byte-identical** - fails
//! loudly instead of quietly. Returning the bytes anyway with a warning would be worse
//! than returning nothing: the model would carry on with content that is no longer what
//! the conversation contained, and nothing downstream could tell.

use std::path::PathBuf;

use supra_types::{ContentHash, TurnId};

/// Why a store operation could not complete.
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    /// The database file could not be opened or prepared.
    #[error("cannot open the store at {}: {source}", path.display())]
    Open {
        /// The file involved.
        path: PathBuf,
        /// The underlying failure.
        source: rusqlite::Error,
    },

    /// The file was written by a newer version of supra.
    ///
    /// Refused rather than opened. A newer schema may store the same table with
    /// different meaning, and reading it with older code would not fail - it would
    /// silently misinterpret. Migrations run forward only.
    #[error(
        "the store at {} is at schema version {found}, and this build understands up to \
         {supported}; it was written by a newer supra",
        path.display()
    )]
    SchemaTooNew {
        /// The file involved.
        path: PathBuf,
        /// Version recorded in the file.
        found: u32,
        /// Highest version this build can apply.
        supported: u32,
    },

    /// A component's tables in this file were written by a newer version of supra.
    ///
    /// Separate from [`StoreError::SchemaTooNew`] because the remedy is reported against a
    /// component rather than the file: the core schema can be current while one owner's
    /// tables are ahead, which is exactly what happens when a build is downgraded.
    #[error(
        "component {component:?} in the store at {} is at schema version {found}, and this \
         build understands up to {supported}; it was written by a newer supra",
        path.display()
    )]
    ComponentSchemaTooNew {
        /// The file involved.
        path: PathBuf,
        /// Which component owns the tables.
        component: String,
        /// Version recorded for that component.
        found: u32,
        /// Highest version this build can apply.
        supported: u32,
    },

    /// A migration failed.
    ///
    /// The schema version is unchanged: each migration runs inside a transaction, and
    /// SQLite rolls back the DDL and the version together.
    #[error("migration to schema version {to} failed, store left at {from}: {source}")]
    Migrate {
        /// Version the store was at.
        from: u32,
        /// Version being applied.
        to: u32,
        /// The underlying failure.
        source: rusqlite::Error,
    },

    /// A component's migration failed.
    ///
    /// As [`StoreError::Migrate`]: the recorded version is unchanged, because the DDL and
    /// the version bump share one transaction.
    #[error(
        "migration of component {component:?} to schema version {to} failed, left at \
         {from}: {source}"
    )]
    ComponentMigrate {
        /// Which component owns the tables.
        component: String,
        /// Version the component was at.
        from: u32,
        /// Version being applied.
        to: u32,
        /// The underlying failure.
        source: rusqlite::Error,
    },

    /// A recalled turn does not hash to what was stored.
    ///
    /// The bytes are **not** returned. Invariant I4 promises byte-identical recall, and a
    /// caller that received altered content with a warning attached would have no way to
    /// tell the difference downstream.
    #[error(
        "turn {turn} does not match the digest recorded when it was evicted \
         (stored {stored}, read back as {computed}); the store is damaged and the turn \
         cannot be recalled"
    )]
    Corrupt {
        /// Which turn.
        turn: TurnId,
        /// Digest written at eviction.
        stored: ContentHash,
        /// Digest of what came back.
        computed: ContentHash,
    },

    /// The same turn was evicted twice with different content.
    ///
    /// A retry after a partial failure is fine and is a no-op. *Different* bytes under
    /// the same identity is a defect upstream: a turn has one body, and accepting the
    /// second would silently discard whichever version some other component still
    /// believes in.
    #[error(
        "turn {turn} was already evicted with different content (stored {stored}, \
         offered {offered}); a turn has one body"
    )]
    Conflict {
        /// Which turn.
        turn: TurnId,
        /// Digest already in the store.
        stored: ContentHash,
        /// Digest of the body being offered.
        offered: ContentHash,
    },

    /// The turn is not in the store.
    #[error("turn {turn} has not been evicted")]
    NotFound {
        /// Which turn.
        turn: TurnId,
    },

    /// A stored row could not be read back as the type it was written as.
    #[error("a stored row is malformed: {detail}")]
    Malformed {
        /// What was wrong.
        detail: String,
    },

    /// Any other database failure.
    #[error("store operation failed: {0}")]
    Sqlite(#[from] rusqlite::Error),
}

impl StoreError {
    /// Whether this indicates the store's contents can no longer be trusted.
    ///
    /// Distinguished because the response differs in kind. A missing turn is a question
    /// answered; damage means the file needs attention and a caller should stop relying on
    /// what it holds rather than retrying.
    #[must_use]
    pub const fn is_damage(&self) -> bool {
        matches!(self, Self::Corrupt { .. } | Self::Malformed { .. })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use supra_types::CanonicalWriter;

    fn hash_of(byte: u8) -> ContentHash {
        let mut writer = CanonicalWriter::for_kind(0xF5);
        writer.bytes(1, &[byte]);
        writer.finish()
    }

    #[test]
    fn a_corruption_message_names_both_digests_and_refuses_the_bytes() {
        // The message has to make clear that nothing was returned, or a reader will assume
        // the content was recovered and only the warning is new.
        let error =
            StoreError::Corrupt { turn: TurnId::generate(), stored: hash_of(1), computed: hash_of(2) };
        let text = error.to_string();
        assert!(text.contains("does not match the digest"), "{text}");
        assert!(text.contains("cannot be recalled"), "{text}");
        assert!(error.is_damage());
    }

    #[test]
    fn a_conflict_says_why_a_second_body_is_refused() {
        let error =
            StoreError::Conflict { turn: TurnId::generate(), stored: hash_of(1), offered: hash_of(2) };
        let text = error.to_string();
        assert!(text.contains("a turn has one body"), "{text}");
        assert!(!error.is_damage(), "a caller defect is not store damage");
    }

    #[test]
    fn a_too_new_schema_says_what_wrote_it() {
        let error = StoreError::SchemaTooNew { path: PathBuf::from("/tmp/supra.db"), found: 9, supported: 3 };
        let text = error.to_string();
        assert!(text.contains("version 9"), "{text}");
        assert!(text.contains("up to 3"), "{text}");
        assert!(text.contains("newer supra"), "the remedy is to upgrade: {text}");
        assert!(!error.is_damage());
    }

    #[test]
    fn a_missing_turn_is_not_damage() {
        let error = StoreError::NotFound { turn: TurnId::generate() };
        assert!(!error.is_damage(), "an answered question is not a broken file");
    }

    #[test]
    fn a_failed_migration_says_the_version_did_not_move() {
        let error = StoreError::Migrate { from: 1, to: 2, source: rusqlite::Error::InvalidQuery };
        let text = error.to_string();
        assert!(text.contains("left at 1"), "{text}");
    }
}
