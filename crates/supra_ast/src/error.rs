//! Why structural operations can fail, and what each failure obliges.
//!
//! The variants split on *who must act*: the caller retrying will not help any
//! of these - every one is a defect in what was handed over (a range that is
//! not a node, a replacement that does not parse, a name that collides), not a
//! transient the next attempt would survive. A caller that needs the remedy
//! reads the message; a caller that needs the category matches the variant.

use std::path::PathBuf;

use thiserror::Error;

/// What structural operations can fail with.
#[derive(Clone, Debug, PartialEq, Eq, Error)]
#[non_exhaustive]
pub enum AstError {
    /// No grammar covers the file's language.
    ///
    /// By design, not by accident: seven grammars cover the working set (T15's
    /// rule), and anything else refuses rather than guessing. The caller falls
    /// back to `edit_file`, which is exactly the riskier path the reversibility
    /// damping rewards avoiding.
    #[error("no grammar covers {} (extension {:?})", path.display(), extension)]
    UnsupportedLanguage {
        /// The file involved.
        path: PathBuf,
        /// Its extension, if it has one.
        extension: Option<String>,
    },

    /// The byte range does not address a node.
    ///
    /// Either the range is out of bounds, inverted, or - the interesting case -
    /// it names bytes that are not exactly one node's span. A splice of half a
    /// node is how a structured edit becomes string surgery wearing a node
    /// range, so the gate refuses it rather than rounding to the nearest node.
    #[error("bytes {start}..{end} in {} address no single node", path.display())]
    RangeIsNotANode {
        /// The file involved.
        path: PathBuf,
        /// Requested start byte.
        start: usize,
        /// Requested end byte (exclusive).
        end: usize,
    },

    /// The source does not parse.
    ///
    /// Error nodes anywhere - before or after the splice - refuse the
    /// operation. The ranges around an error are guesses (T15's rule), and a
    /// splice verified against guesses certifies nothing.
    #[error("source {} does not parse cleanly: {detail}", path.display())]
    HasErrors {
        /// The file involved.
        path: PathBuf,
        /// What was wrong, in one line.
        detail: String,
    },

    /// The spliced result does not parse, or parses to a different node kind.
    ///
    /// The reparse gate: the replacement must leave the file error-free *and*
    /// the spliced node the same kind it was. A function replaced by a struct
    /// parses fine and is still the wrong edit - same-kind is what makes the
    /// gate structural rather than merely syntactic. The source is untouched;
    /// atomic rollback means there is nothing to roll back.
    #[error("reparse gate refused the splice in {}: {detail}", path.display())]
    GateRefused {
        /// The file involved (unmodified).
        path: PathBuf,
        /// What the reparse found, in one line.
        detail: String,
    },

    /// A rename target already names a live symbol in scope.
    ///
    /// Refused rather than shadowed: silently creating a shadow is how a rename
    /// "succeeds" while changing which declaration every existing reference
    /// resolves to. The caller picks another name; the tree is untouched.
    #[error("rename of {old:?} to {new:?} in {} would shadow {shadowed:?}", path.display())]
    WouldShadow {
        /// The file involved (unmodified).
        path: PathBuf,
        /// The name being replaced.
        old: String,
        /// The name requested.
        new: String,
        /// The existing declaration that would be shadowed.
        shadowed: String,
    },

    /// The rename found no occurrence of the old name.
    ///
    /// Refused rather than returning an empty edit list: an empty rename that
    /// reports success is how a misspelled target passes silently. Zero edits
    /// is information, and information arrives as an error, not as silence.
    #[error("name {old:?} occurs nowhere eligible in {}", path.display())]
    NothingToRename {
        /// The file involved (unmodified).
        path: PathBuf,
        /// The name that was asked for.
        old: String,
    },
}
