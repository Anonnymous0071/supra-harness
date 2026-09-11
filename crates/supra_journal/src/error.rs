//! What the journal refused, and who must act.
//!
//! Every refusal here names an actor, because the journal sits on the one
//! boundary where "retry" and "give up" are both wrong answers to the same
//! error: a snapshot that cannot be trusted must never be written from, and a
//! file whose snapshot disagrees with its bytes has a story only the operator
//! can finish reading.
//!
//! | Variant | Shape | Who acts |
//! | ------- | ----- | -------- |
//! | `Store` | the store refused (migration, transaction) | the operator |
//! | `Io` | the file itself could not be read or written | the operator |
//! | `Corrupt` | a snapshot's bytes no longer match their digest | nobody silently - the snapshot is refused, not returned |
//! | `AlreadyUndone` | the caller asked to undo twice | the caller, in one line |
//! | `NotFound` | the snapshot id does not exist | the caller |

use thiserror::Error;

/// A journal refusal.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum JournalError {
    /// The store refused: a component migration failed, or a transaction
    /// could not be opened or committed. The store's own error names the
    /// step.
    #[error("the store refused: {0}")]
    Store(#[from] supra_store::StoreError),

    /// A query against the journal's own tables failed - the SQL below the
    /// store's vocabulary, in the journal's. Distinct from [`Self::Store`]
    /// for the reason `with_transaction`'s error bound exists: the owner of
    /// the tables owns the failure vocabulary.
    #[error("the journal's tables refused: {0}")]
    Sqlite(#[from] rusqlite::Error),

    /// The file could not be read or written. The snapshot may or may not
    /// have landed; the error names which side failed, and the caller
    /// treats the effect as unknown - not as "undone".
    #[error("the file refused: {0}")]
    Io(#[from] std::io::Error),

    /// The journal cannot preserve this path in its TEXT schema without
    /// changing its bytes. Refusing is safer than storing a lossy spelling
    /// that undo could resolve to a different file.
    #[error("the path is not valid UTF-8 and cannot be journaled losslessly: {path:?}")]
    NonUtf8Path {
        /// The path that could not be represented exactly.
        path: std::path::PathBuf,
    },

    /// The path stopped naming the descriptor opened for the snapshot.
    ///
    /// This is the fail-closed outcome for a concurrent rename or symlink
    /// swap: the bytes are not committed under an identity they did not come
    /// from.
    #[error("the path changed identity while it was being snapshotted: {path:?}")]
    PathIdentityChanged {
        /// The path whose descriptor and resolved name disagreed.
        path: std::path::PathBuf,
    },

    /// This platform cannot provide the no-follow and identity checks needed
    /// to bind snapshot bytes to one path safely.
    #[error("safe journal path handling is unsupported on this platform")]
    UnsupportedPathSafety,

    /// A stored snapshot does not hash to the digest recorded with it.
    ///
    /// The bytes are **not** offered to the caller. I4's reasoning applies
    /// verbatim: a caller that received altered content with a warning
    /// attached would have no way to tell the difference downstream, and an
    /// undo that restores *approximately* the original is worse than no
    /// undo, because it destroys the very recoverability it exists to
    /// provide.
    #[error(
        "snapshot {snapshot} does not match the digest recorded when it was taken \
         (stored {stored}, read back as {computed}); the journal is damaged and the \
         snapshot cannot be undone"
    )]
    Corrupt {
        /// Which snapshot.
        snapshot: supra_types::SnapshotId,
        /// Digest written at snapshot time.
        stored: supra_types::ContentHash,
        /// Digest of what came back.
        computed: supra_types::ContentHash,
    },

    /// The caller asked to undo a snapshot that has already been undone.
    ///
    /// Undoing twice would restore the same bytes twice - harmless when the
    /// file is untouched since, destructive when it is not: the second undo
    /// would silently revert the *first* caller's legitimate work. The row
    /// is marked at undo time, so the refusal is a state read, not a guess.
    #[error("snapshot {snapshot} has already been undone")]
    AlreadyUndone {
        /// Which snapshot.
        snapshot: supra_types::SnapshotId,
    },

    /// The snapshot id does not name a stored snapshot.
    #[error("snapshot {snapshot} does not exist")]
    NotFound {
        /// Which snapshot.
        snapshot: supra_types::SnapshotId,
    },
}
