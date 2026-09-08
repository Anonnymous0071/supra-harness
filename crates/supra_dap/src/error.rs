use thiserror::Error;

/// A DAP client refusal.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum DapError {
    /// The adapter process could not start, or its pipe broke.
    #[error("the debug adapter failed: {0}")]
    Transport(String),

    /// The adapter answered something that is not DAP, or a response
    /// whose shape the protocol does not define.
    #[error("the debug adapter violated the protocol: {0}")]
    Protocol(String),

    /// The requested language has no configured adapter.
    #[error("no debug adapter is configured for {language}")]
    Uncovered {
        /// The language with no adapter.
        language: &'static str,
    },

    /// The adapter stopped the debuggee and reported why.
    #[error("the debuggee stopped: {0}")]
    Stopped(String),
}
