//! JSON-RPC 2.0 over MCP: the request/response envelope.
//!
//! MCP is JSON-RPC 2.0 framed; the envelope is small enough that writing
//! it by hand is both cheaper and safer than pulling an MCP SDK whose
//! version pins would fight the workspace's (the T15.7 lesson about two
//! implementations of one grammar applies to SDKs too - here the grammar
//! is ~40 lines and owned).
//!
//! Only what the gateway needs: `initialize` handshake,
//! `tools/list`, and `tools/call`. Notifications and server-initiated
//! requests are deliberately absent - a client that never advertises
//! capabilities receives none, and the gateway advertises none.

use serde::{Deserialize, Serialize};

/// The protocol version this gateway speaks.
///
/// MCP negotiated versions at `initialize` from the start; `2025-06-18`
/// is the revision whose `tools/list` shape this module reads. A server
/// offering an older revision still negotiates: the handshake accepts any
/// version the server reports (the protocol made the client follow), and
/// the shapes this module reads are the ones every revision since
/// 2024-11-05 shares.
pub const PROTOCOL_VERSION: &str = "2025-06-18";

/// One JSON-RPC request. `id` is always a number here: the gateway issues
/// one request at a time per connection, so a monotonic counter is enough
/// correlation and numbers serialise smaller than UUIDs.
#[derive(Debug, Serialize)]
pub struct Request<'a> {
    /// `"2.0"`, always.
    pub jsonrpc: &'static str,
    /// The correlation id.
    pub id: u64,
    /// The method: `initialize`, `tools/list`, `tools/call`.
    pub method: &'a str,
    /// The method's parameters.
    pub params: serde_json::Value,
}

impl Request<'_> {
    /// A request with `params` already a JSON value.
    #[must_use]
    pub fn new(id: u64, method: &str, params: serde_json::Value) -> Request<'_> {
        Request { jsonrpc: "2.0", id, method, params }
    }
}

/// A JSON-RPC response: either a result or an error object, never both.
#[derive(Debug, Deserialize)]
pub struct Response {
    /// Correlates with the request's `id`.
    #[serde(default)]
    pub id: Option<u64>,
    /// The result object, on success.
    #[serde(default)]
    pub result: Option<serde_json::Value>,
    /// The error object, on failure.
    #[serde(default)]
    pub error: Option<RpcError>,
}

/// The JSON-RPC error object.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct RpcError {
    /// The error code.
    pub code: i64,
    /// The message.
    pub message: String,
}

impl Response {
    /// Whether this response carries an error.
    #[must_use]
    pub const fn is_error(&self) -> bool {
        self.error.is_some()
    }
}

/// The `initialize` result the server sends back.
///
/// Only the fields the gateway reads: the negotiated version and the
/// server's identity for the log. Everything else (capabilities,
/// instructions) is ignored - the gateway advertises no capabilities, so
/// nothing the server offers changes its behaviour.
#[derive(Debug, Deserialize)]
pub struct InitializeResult {
    /// The protocol version the server wants to speak.
    #[serde(rename = "protocolVersion")]
    pub protocol_version: String,
}

/// One tool as `tools/list` reports it.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RemoteTool {
    /// The tool's name, as the server reports it.
    pub name: String,
    /// The tool's description, as the server reports it.
    #[serde(default)]
    pub description: String,
    /// The tool's input schema, as a JSON value. The gateway does not
    /// validate remote schemas with the built-in language - the server's
    /// schema is the contract the server enforces, and re-implementing
    /// validation for a schema this crate did not write would be the
    /// second grammar again. The manifest renders it verbatim.
    #[serde(rename = "inputSchema")]
    pub input_schema: serde_json::Value,
}

/// The `tools/list` result.
#[derive(Debug, Deserialize)]
pub struct ToolsListResult {
    /// The server's tools.
    pub tools: Vec<RemoteTool>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_request_serialises_as_json_rpc_2_0() {
        let request = Request::new(1, "tools/list", serde_json::json!({ "cursor": null }));
        let text = serde_json::to_string(&request).expect("serialise");
        assert!(text.contains(r#""jsonrpc":"2.0""#), "{text}");
        assert!(text.contains(r#""id":1"#), "{text}");
        assert!(text.contains(r#""method":"tools/list""#), "{text}");
    }

    #[test]
    fn a_response_parses_result_and_error_shapes() {
        let ok: Response =
            serde_json::from_str(r#"{"jsonrpc":"2.0","id":7,"result":{"tools":[]}}"#).expect("parse");
        assert!(!ok.is_error());
        assert!(ok.result.is_some());

        let err: Response = serde_json::from_str(
            r#"{"jsonrpc":"2.0","id":7,"error":{"code":-32601,"message":"method not found"}}"#,
        )
        .expect("parse");
        assert!(err.is_error());
        assert_eq!(err.error.expect("error").code, -32601);
    }

    #[test]
    fn initialize_reads_the_negotiated_version() {
        let result: InitializeResult = serde_json::from_value(serde_json::json!({
            "protocolVersion": "2025-06-18",
            "capabilities": {},
            "serverInfo": {"name": "demo", "version": "1.0"}
        }))
        .expect("parse");
        assert_eq!(result.protocol_version, "2025-06-18");
    }

    #[test]
    fn tools_list_parses_the_shared_shape() {
        let result: ToolsListResult = serde_json::from_value(serde_json::json!({
            "tools": [
                {
                    "name": "echo",
                    "description": "Echo a message.",
                    "inputSchema": {"type": "object", "properties": {}}
                }
            ]
        }))
        .expect("parse");
        assert_eq!(result.tools.len(), 1);
        assert_eq!(result.tools[0].name, "echo");
        // A description-less tool is legal; the manifest renders what is
        // there rather than refusing what is not.
        let result: ToolsListResult =
            serde_json::from_value(serde_json::json!({"tools": [{"name": "bare", "inputSchema": {}}]}))
                .expect("parse");
        assert_eq!(result.tools[0].description, "");
    }
}
