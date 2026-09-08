//! What skill loading refused, and who must act.
//!
//! Skills are user-authored content, so every refusal names an author's
//! fix: a file that is not a skill, a dependency that is not there, a
//! cycle no order can satisfy. The operator (or the author) acts; the
//! harness never guesses.
//!
//! | Variant | Shape | Who acts |
//! | ------- | ----- | -------- |
//! | `Io` | the file could not be read | the operator |
//! | `Parse` | the front matter is not valid | the skill's author |
//! | `MissingField` | a required front-matter field is absent | the author |
//! | `UnknownDependency` | a named dependency is not loaded | the author |
//! | `Cycle` | the dependency graph has a cycle | the author |
//! | `Duplicate` | two skills claim one name | the author |

use thiserror::Error;

/// A skill-loading refusal.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum SkillError {
    /// The file could not be read.
    #[error("the skill file refused: {0}")]
    Io(#[from] std::io::Error),

    /// The front matter could not be parsed. The detail names the line
    /// and what was expected there.
    #[error("the front matter of {path:?} is not valid: {detail}")]
    Parse {
        /// Which file.
        path: String,
        /// What is wrong, with the offending line quoted.
        detail: String,
    },

    /// A required front-matter field is absent. `name` and `description`
    /// are the two the loader cannot proceed without: the name is the
    /// identity every other field and the registry refer to, and the
    /// description is what the model reads to decide whether to open the
    /// body - the Kimchi lesson, kept as a hard requirement.
    #[error("the front matter of {path:?} is missing field {field:?}")]
    MissingField {
        /// Which file.
        path: String,
        /// Which field.
        field: &'static str,
    },

    /// A dependency names a skill that is not loaded. Skills resolve
    /// against their directory: a `requires` entry with no matching
    /// `SKILL.md` beside it is a typo, not a late arrival - the loader
    /// resolves once, at load, and never blocks on a file that may not
    /// exist.
    #[error("skill {skill:?} requires {missing:?}, which is not loaded")]
    UnknownDependency {
        /// The skill that declared the dependency.
        skill: String,
        /// The dependency that is not there.
        missing: String,
    },

    /// The dependency graph has a cycle. The detail names the cycle's
    /// path (`a -> b -> a`), because "cycle" alone sends the author
    /// hunting through every skill they wrote.
    #[error("the skill dependency graph has a cycle: {cycle}")]
    Cycle {
        /// The cycle, as an arrow-joined path.
        cycle: String,
    },

    /// Two skills claim one name. A duplicate would make the registry's
    /// tool names and the model's references ambiguous, and the
    /// append-only rule means the second one cannot overwrite the first.
    #[error("two skills claim the name {name:?}: {first:?} and {second:?}")]
    Duplicate {
        /// The claimed name.
        name: String,
        /// The first file that claimed it.
        first: String,
        /// The second file that claimed it.
        second: String,
    },
}
