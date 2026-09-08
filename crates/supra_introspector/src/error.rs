use thiserror::Error;

/// An introspector refusal.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum IntrospectError {
    /// The gate command could not run.
    #[error("the gate refused to run: {0}")]
    Spawn(String),

    /// A cross-agent check had no peer answers to compare.
    #[error("cross-agent check has no answers: {0}")]
    NoAnswers(String),
}
