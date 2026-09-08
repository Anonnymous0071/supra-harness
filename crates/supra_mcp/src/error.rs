//! What the MCP gateway refused, and who must act.
//!
//! The gateway sits between the harness and third-party servers, which
//! means every refusal has two audiences: the operator (whose
//! configuration produced the call) and the model (whose tool call
//! proxied through). The variants name both where they differ.
//!
//! | Variant | Shape | Who acts |
//! | ------- | ----- | -------- |
//! | `Transport` | the server could not be reached or answered | the operator |
//! | `Protocol` | the server speaks something that is not MCP | the operator |
//! | `ProtocolVersion` | the server's version and ours have no overlap | the operator |
//! | `ToolName` | a discovered tool's name cannot be registered | the operator |
//! | `Cache` | the manifest store refused | the operator |
//! | `TooManyTools` | the server's tool count exceeds the budget | the operator |

use thiserror::Error;

/// An MCP gateway refusal.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum McpError {
    /// The transport failed: the process did not start, the pipe broke, or
    /// the HTTP endpoint did not answer. The detail names which.
    #[error("the MCP transport failed: {0}")]
    Transport(String),

    /// The server answered something that is not a JSON-RPC response, or a
    /// response whose shape the protocol does not define.
    #[error("the MCP server violated the protocol: {0}")]
    Protocol(String),

    /// The server's protocol version and the gateway's have no overlap.
    ///
    /// Negotiated at `initialize` per the protocol; a mismatch is stated,
    /// not retried, because retrying a version the server does not speak
    /// is how corruption wears a success.
    #[error("protocol version mismatch: server wants {server}, gateway speaks {gateway}")]
    ProtocolVersion {
        /// The version the server asked for.
        server: String,
        /// The version this gateway last spoke.
        gateway: &'static str,
    },

    /// A discovered tool's name cannot live in the registry: it is not
    /// snake_case-able ASCII, or it collides with an existing tool. A
    /// name the registry cannot carry is refused at discovery, not at
    /// first call - the manifest never advertises a tool the dispatcher
    /// could not route.
    #[error("discovered tool {tool:?} cannot be registered: {reason}")]
    ToolName {
        /// The name the server offered.
        tool: String,
        /// Why it cannot be registered.
        reason: String,
    },

    /// The manifest cache (SQLite) refused.
    #[error("the manifest cache refused: {0}")]
    Cache(#[from] supra_store::StoreError),

    /// The server's tool count exceeds the discovery budget.
    ///
    /// A server that advertises thousands of tools would spend the
    /// session's manifest budget on one remote; the budget refuses at
    /// discovery, the same refusal shape T15's index budget takes.
    #[error("server {server:?} advertises {count} tools; the discovery budget is {budget}")]
    TooManyTools {
        /// The server's name.
        server: String,
        /// How many tools it offered.
        count: usize,
        /// The ceiling.
        budget: usize,
    },
}
