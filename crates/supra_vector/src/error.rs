//! One variant per way retrieval can fail.
//!
//! The distinctions that matter here are between *a query cannot be answered* and *the index
//! no longer describes the corpus*. The second is not recoverable by retrying, and answering
//! anyway would return confidently wrong anchors - which is worse than returning none, because
//! nothing downstream can tell a bad anchor from a good one.

use std::path::PathBuf;

use thiserror::Error;

/// Everything this crate can fail at.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum VectorError {
    /// The index was built for a different embedding model or width.
    ///
    /// Not recoverable and not a reason to score anyway. Embeddings from two models share no
    /// space: a cosine between them is a number with no meaning, and it would rank. The
    /// caller's options are to re-embed the corpus or to run lexical-only, and only the caller
    /// knows which is acceptable - so this crate reports and does neither.
    #[error(
        "the vector index at {} was built by model {stored:?} at {stored_dims} dimensions, and \
         this session uses {expected:?} at {expected_dims}; re-embed the corpus or run the \
         lexical lane alone",
        path.display()
    )]
    ModelMismatch {
        /// The file involved.
        path: PathBuf,
        /// Model identity recorded in the index.
        stored: String,
        /// Width recorded in the index.
        stored_dims: usize,
        /// Model identity this session was opened with.
        expected: String,
        /// Width this session was opened with.
        expected_dims: usize,
    },

    /// The index was built with a different binarisation threshold.
    ///
    /// Separate from [`VectorError::ModelMismatch`] because the model can match while the
    /// threshold does not, and the consequence is narrower but no less wrong: the stored codes
    /// answer a different question from the one the caller's threshold would ask, so a query
    /// binarised with the new threshold would be compared against codes written under the old.
    ///
    /// Reported rather than silently resolved in the stored threshold's favour. Ignoring a
    /// caller's argument leaves it believing something false about how its queries are encoded.
    #[error(
        "the vector index at {} was built with a different binarisation threshold; it is frozen, \
         so re-binarising the corpus is the only way to change it",
        path.display()
    )]
    FrozenThresholdMismatch {
        /// The file involved.
        path: PathBuf,
    },

    /// A vector was offered at the wrong width.
    #[error("expected a {expected}-dimension vector, got {found}")]
    WrongWidth {
        /// The index's width.
        expected: usize,
        /// What arrived.
        found: usize,
    },

    /// A vector is not usable for cosine similarity.
    ///
    /// A zero vector has no direction, and a non-finite component poisons every comparison it
    /// takes part in: `NaN` fails every ordering predicate, so one of them silently rearranges
    /// a ranking rather than failing.
    #[error("the vector is not usable for similarity: {detail}")]
    Degenerate {
        /// What is wrong with it.
        detail: String,
    },

    /// A stored row is not the shape the schema promised.
    ///
    /// Distinct from [`VectorError::Degenerate`]: this one means the file was edited around
    /// SQLite, or written by something that is not this code.
    #[error("a stored row is malformed: {detail}")]
    Malformed {
        /// What was wrong.
        detail: String,
    },

    /// The index has no configuration row yet.
    #[error("the vector index at {} has not been initialised", path.display())]
    Uninitialised {
        /// The file involved.
        path: PathBuf,
    },

    /// A locator that was asked for is not in the index.
    #[error("no entry for locator {locator:?}")]
    NotFound {
        /// What was asked for.
        locator: String,
    },

    /// The underlying store failed.
    #[error(transparent)]
    Store(#[from] supra_store::StoreError),

    /// SQLite failed.
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
}

impl VectorError {
    /// Whether this means the index itself is untrustworthy, rather than one request being
    /// wrong.
    ///
    /// A caller uses this to decide between refusing a query and refusing the lane: a
    /// [`VectorError::WrongWidth`] is a caller's mistake and the next query may be fine, while
    /// a [`VectorError::Malformed`] row means the file cannot be trusted to rank anything.
    #[must_use]
    pub const fn is_index_damage(&self) -> bool {
        matches!(
            self,
            Self::Malformed { .. } | Self::ModelMismatch { .. } | Self::FrozenThresholdMismatch { .. }
        )
    }
}
