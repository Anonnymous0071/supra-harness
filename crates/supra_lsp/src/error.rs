use thiserror::Error;

/// An LSP client refusal.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum LspError {
    /// The server process could not start, or its pipe broke.
    #[error("the language server failed: {0}")]
    Transport(String),

    /// The server answered something that is not JSON-RPC, or a response
    /// whose shape the protocol does not define.
    #[error("the language server violated the protocol: {0}")]
    Protocol(String),

    /// The requested language has no configured server.
    #[error("no language server is configured for {language}")]
    Uncovered {
        /// The language with no server.
        language: &'static str,
    },

    /// The server cannot answer this request for the file asked.
    #[error("the server refused the request: {0}")]
    Server(String),

    /// The server died mid-request and the recovery restart could not
    /// answer it either.
    #[error("the language server crashed and did not recover: {0}")]
    Crashed(String),
}
