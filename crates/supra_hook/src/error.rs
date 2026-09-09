use thiserror::Error;

/// A hook refusal.
#[derive(Debug, Error, PartialEq, Eq)]
#[non_exhaustive]
pub enum HookError {
    /// The hook point's name is not one of the eight.
    #[error("unknown hook point {name:?}")]
    UnknownPoint {
        /// The unrecognised name.
        name: String,
    },

    /// A hook tried to attach inside the prompt prefix's lifetime. Only
    /// the four boundary points accept hooks; a user command executing
    /// mid-prefix can mutate what the provider has already cached.
    #[error("hook point {point:?} is inside the prefix; only boundary points accept hooks")]
    NotPrefixSafe {
        /// The refused point.
        point: crate::point::HookPoint,
    },

    /// A hook command could not run.
    #[error("the hook command failed: {0}")]
    Command(String),
}
