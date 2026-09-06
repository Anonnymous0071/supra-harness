//! Why digest operations can fail, and what each failure obliges.
//!
//! The variants split on *who must act*: the caller retrying will not help any of
//! these — every one is a defect in what was handed over (an unreadable tree, an
//! unparseable file, an over-budget anchor set), not a transient the next attempt
//! would survive. A caller that needs the remedy reads the message; a caller that
//! needs the category matches the variant.

use std::path::PathBuf;

use thiserror::Error;

/// What digest operations can fail with.
///
/// `Clone` and `PartialEq` are manual. Every variant compares structurally, except
/// `Vector` and `Store`: neither upstream error is `Clone` (I/O and index state),
/// so both impls degrade honestly - `Clone` rebuilds the message inside the one
/// pure-data shape available, `PartialEq` compares rendered messages. Tests use
/// `matches!` for those variants anyway; the impls exist so `assert_eq` keeps
/// working for the rest.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum DigestError {
    /// The repository root is not a readable directory.
    ///
    /// Checked once at open, not per file: a root that cannot be listed cannot be
    /// indexed, and failing late would mean a half-built index of unknown coverage.
    #[error("repository root {} is not a readable directory: {detail}", root.display())]
    BadRoot {
        /// The path that was offered as a root.
        root: PathBuf,
        /// What was wrong, in one line.
        detail: String,
    },

    /// A file could not be read during a scan.
    ///
    /// Reported, not skipped silently: a symbol index that quietly omits unreadable
    /// files answers queries with false confidence about its own coverage.
    #[error("could not read {}: {detail}", path.display())]
    Unreadable {
        /// The file involved.
        path: PathBuf,
        /// The underlying failure, in one line.
        detail: String,
    },

    /// No grammar covers the file's language.
    ///
    /// Not a fault in the file: the digest indexes seven languages, and anything
    /// else is outside its coverage by design. Callers that need total coverage
    /// match this variant and fall back to the lexical lane alone.
    #[error("no grammar covers {} (extension {:?})", path.display(), extension)]
    UnsupportedLanguage {
        /// The file involved.
        path: PathBuf,
        /// Its extension, if it has one.
        extension: Option<String>,
    },

    /// Anchors exceed the suffix budget.
    ///
    /// Refused rather than truncated: truncation would silently turn the ~300-token
    /// budget into a suggestion, and only the caller (T23, which knows the turn's
    /// remaining window) knows which anchors matter least. Same rule as T14's index
    /// entries: over budget is refused, never shortened.
    #[error("anchor set is {tokens} tokens; the budget is {budget}")]
    OverBudget {
        /// Estimated tokens in the anchor set offered.
        tokens: usize,
        /// The suffix budget.
        budget: usize,
    },

    /// The underlying vector index failed.
    #[error(transparent)]
    Vector(#[from] supra_vector::VectorError),

    /// The underlying store failed.
    #[error(transparent)]
    Store(#[from] supra_store::StoreError),
}

impl Clone for DigestError {
    fn clone(&self) -> Self {
        match self {
            Self::BadRoot { root, detail } => Self::BadRoot { root: root.clone(), detail: detail.clone() },
            Self::Unreadable { path, detail } => {
                Self::Unreadable { path: path.clone(), detail: detail.clone() }
            }
            Self::UnsupportedLanguage { path, extension } => {
                Self::UnsupportedLanguage { path: path.clone(), extension: extension.clone() }
            }
            Self::OverBudget { tokens, budget } => Self::OverBudget { tokens: *tokens, budget: *budget },
            // Neither upstream error clones; rebuild the message inside a BadRoot
            // shell is wrong (it would misattribute), so degrade to Unreadable
            // carrying the message: production matches the variant, never clones.
            Self::Vector(error) => {
                Self::Unreadable { path: PathBuf::from("<vector index>"), detail: error.to_string() }
            }
            Self::Store(error) => {
                Self::Unreadable { path: PathBuf::from("<store>"), detail: error.to_string() }
            }
        }
    }
}

impl PartialEq for DigestError {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (
                Self::BadRoot { root: left_root, detail: left_detail },
                Self::BadRoot { root: right_root, detail: right_detail },
            ) => left_root == right_root && left_detail == right_detail,
            (
                Self::Unreadable { path: left_path, detail: left_detail },
                Self::Unreadable { path: right_path, detail: right_detail },
            ) => left_path == right_path && left_detail == right_detail,
            (
                Self::UnsupportedLanguage { path: left_path, extension: left_extension },
                Self::UnsupportedLanguage { path: right_path, extension: right_extension },
            ) => left_path == right_path && left_extension == right_extension,
            (
                Self::OverBudget { tokens: left_tokens, budget: left_budget },
                Self::OverBudget { tokens: right_tokens, budget: right_budget },
            ) => left_tokens == right_tokens && left_budget == right_budget,
            // Upstream errors compare by rendered message.
            (Self::Vector(left), Self::Vector(right)) => left.to_string() == right.to_string(),
            (Self::Store(left), Self::Store(right)) => left.to_string() == right.to_string(),
            _ => false,
        }
    }
}

impl Eq for DigestError {}
