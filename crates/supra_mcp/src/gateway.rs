//! The gateway: probe once, cache manifests, register by append.
//!
//! I3's rule for MCP, stated in the architecture: "MCP servers are probed
//! once and their manifests cached to SQLite... Dynamic capability sits
//! behind a **static gateway schema**, and newly discovered tools are
//! described by appending."
//!
//! The static gateway is one tool that never changes: `mcp` itself. Its
//! manifest describes how to call any remote tool - `server`, `tool`,
//! `arguments` - and never names the remote tools individually. The
//! dynamic knowledge (which servers exist, which tools they offer) lives
//! in the cache and reaches the model as *content*, appended to the
//! conversation when asked for, not as `tools` mutations that would
//! invalidate the prefix at BP1-3.
//!
//! # The probe, once
//!
//! [`Gateway::probe`] handshakes, lists tools, and appends every
//! discoverable tool to the cache. `once` is structural: a `Gateway`
//! probes in its constructor or not at all, and `append` (not `upsert`)
//! means a second probe of the same server discovers only *new* tools -
//! existing rows are the cache's promise to the frozen manifest.
//!
//! # The budget
//!
//! A server that advertises thousands of tools would spend the session's
//! manifest budget on one remote. [`TOOLS_PER_SERVER`] refuses at probe
//! time - the same shape T15's index budget takes, stated before any
//! bytes reach the model.
//!
//! # Naming
//!
//! Remote tools live in the cache as `server__tool`. Two servers offering
//! the same local name never collide; a local name that cannot survive
//! namespacing (empty, too long, or not ASCII-identifiable) is refused at
//! discovery - the manifest never advertises a tool the dispatcher could
//! not route.

use std::sync::Arc;

use rusqlite::TransactionBehavior;
use supra_store::Store;
use supra_types::{CanonicalJson, ToolClass};

use crate::cache::{self, COMPONENT, CachedTool, MIGRATIONS};
use crate::error::McpError;
use crate::rpc::{InitializeResult, PROTOCOL_VERSION, ToolsListResult};
use crate::transport::{Endpoint, Transport};

/// How many tools one server may contribute before the budget refuses.
pub const TOOLS_PER_SERVER: usize = 256;

/// How many `tools/list` pages one probe follows before it decides the
/// server's cursor never settles.
///
/// A server that paginates forever is a server whose manifest cannot be
/// cached; the cap turns that into a named refusal rather than a probe
/// that never returns.
pub const MAX_LIST_PAGES: usize = 32;

/// One configured server: the name the gateway namespaces with, and where
/// it lives.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ServerConfig {
    /// The server's name. Becomes the namespace: `name__tool`.
    pub name: String,
    /// The endpoint to reach it at.
    pub endpoint: Endpoint,
}

/// The static gateway: one connection per configured server, probed once
/// at construction, its discoveries cached.
pub struct Gateway {
    servers: Vec<(ServerConfig, Transport)>,
    store: Arc<Store>,
}

impl Gateway {
    /// Probe every configured server once, and cache what they offer.
    ///
    /// Each server is probed independently: a server that refuses (down,
    /// wrong version, over budget) is skipped and named in the returned
    /// refusals, and the rest of the fleet proceeds - one dead server is
    /// not a dead harness. The cache is loaded first, so the append-only
    /// rule holds even when a probe fails midway.
    ///
    /// # Errors
    ///
    /// One [`McpError`] per server that refused, in configuration order.
    /// Empty when every server answered.
    pub async fn probe(store: Arc<Store>, servers: Vec<ServerConfig>) -> (Self, Vec<(String, McpError)>) {
        let mut refusals = Vec::new();
        let mut live = Vec::new();
        if let Err(error) = store.migrate_component(COMPONENT, MIGRATIONS) {
            // An unmigratable cache is a cache nobody may read or write;
            // every server is refused rather than run against an unknown
            // schema, and the refusal names the migration.
            refusals.push(("<cache>".to_owned(), McpError::Cache(error)));
            return (Self { servers: Vec::new(), store }, refusals);
        }

        for config in servers {
            match Self::probe_one(&store, &config).await {
                Ok(transport) => live.push((config, transport)),
                Err(error) => refusals.push((config.name, error)),
            }
        }

        (Self { servers: live, store }, refusals)
    }

    async fn probe_one(store: &Store, config: &ServerConfig) -> Result<Transport, McpError> {
        let mut transport = match &config.endpoint {
            Endpoint::Stdio { .. } => Transport::stdio(&config.endpoint)?,
            Endpoint::Http { url } => Transport::http(url.clone())?,
        };

        // Handshake: announce, read the negotiated version back.
        let result = transport
            .call(
                "initialize",
                serde_json::json!({
                    "protocolVersion": PROTOCOL_VERSION,
                    "capabilities": {},
                    "clientInfo": {"name": "supra", "version": "0.1.0"}
                }),
            )
            .await?;
        let init: InitializeResult = serde_json::from_value(result)
            .map_err(|error| McpError::Protocol(format!("initialize result: {error}")))?;
        if init.protocol_version.is_empty() {
            return Err(McpError::ProtocolVersion {
                server: "<empty>".to_owned(),
                gateway: PROTOCOL_VERSION,
            });
        }

        // The `notifications/initialized` notification the protocol expects.
        // A notification has no id and expects no answer; the transport
        // sends it as a raw line on stdio and a discarded POST on HTTP.
        transport.notify("notifications/initialized").await?;

        // List tools, following the cursor until the list settles. The
        // budget counts every page, not the first: a server that fits
        // under it only a page at a time is over it.
        let mut remote_tools = Vec::new();
        let mut cursor: Option<String> = None;
        for _ in 0..MAX_LIST_PAGES {
            let mut params = serde_json::json!({});
            if let Some(page) = &cursor {
                params["cursor"] = serde_json::Value::String(page.clone());
            }
            let listed = transport.call("tools/list", params).await?;
            let listed: ToolsListResult = serde_json::from_value(listed)
                .map_err(|error| McpError::Protocol(format!("tools/list result: {error}")))?;
            remote_tools.extend(listed.tools);
            cursor = listed.next_cursor;
            if cursor.is_none() {
                break;
            }
        }
        if cursor.is_some() {
            return Err(McpError::Protocol(format!(
                "tools/list did not settle within {MAX_LIST_PAGES} pages"
            )));
        }
        if remote_tools.len() > TOOLS_PER_SERVER {
            return Err(McpError::TooManyTools {
                server: config.name.clone(),
                count: remote_tools.len(),
                budget: TOOLS_PER_SERVER,
            });
        }

        // Build every row before writing any: a name that cannot survive
        // namespacing refuses the whole probe, and the cache never holds
        // half a manifest for a server the probe then abandoned.
        let mut rows = Vec::with_capacity(remote_tools.len());
        for remote in &remote_tools {
            rows.push(CachedTool {
                tool: namespaced(&config.name, &remote.name)?,
                server: config.name.clone(),
                local_name: remote.name.clone(),
                description: remote.description.clone(),
                input_schema: serde_json::to_string(&remote.input_schema)
                    .map_err(|error| McpError::Protocol(format!("input schema: {error}")))?,
                live: true,
            });
        }
        store
            .with_transaction::<_, supra_store::StoreError>(TransactionBehavior::Immediate, |tx| {
                for row in &rows {
                    cache::append(tx, row)?;
                }
                Ok(())
            })
            .map_err(McpError::Cache)?;

        Ok(transport)
    }

    /// The cached tools, ordered as the manifest lists them.
    ///
    /// # Errors
    ///
    /// [`McpError::Cache`] when the store refuses.
    pub fn cached_tools(&self) -> Result<Vec<CachedTool>, McpError> {
        self.store
            .with_transaction::<_, supra_store::StoreError>(TransactionBehavior::Deferred, |tx| {
                cache::all(tx)
            })
            .map_err(McpError::Cache)
    }

    /// The static gateway tool's registration, for T17's registry.
    ///
    /// One tool, always the same shape: `mcp` with `server`, `tool`, and
    /// `arguments`. The manifest never changes no matter what the servers
    /// offer - that is what "static gateway schema" means, and it is why
    /// MCP discovery cannot break the prefix.
    ///
    /// # Panics
    ///
    /// Never, in practice: the schema is a compile-time constant whose
    /// fields satisfy every build-time invariant by construction. The
    /// `expect` states that if the invariants ever disagree with this
    /// constant, the failure belongs at startup, loudly, not at first
    /// call.
    #[must_use]
    pub fn gateway_tool() -> supra_tool::Tool {
        supra_tool::Tool::register(
            "mcp",
            ToolClass::Agent,
            vec![
                supra_tool::Field {
                    name: "server".to_owned(),
                    field_type: supra_tool::FieldType::Text,
                    required: true,
                    description: "The MCP server to call, by its configured name.".to_owned(),
                },
                supra_tool::Field {
                    name: "tool".to_owned(),
                    field_type: supra_tool::FieldType::Text,
                    required: true,
                    description: "The remote tool's local name on that server.".to_owned(),
                },
                supra_tool::Field {
                    name: "arguments".to_owned(),
                    field_type: supra_tool::FieldType::Text,
                    required: true,
                    description: "The remote tool's arguments, as canonical JSON text.".to_owned(),
                },
            ],
            // The gateway's own effect: calling a remote tool is an
            // outside action the harness cannot classify beyond "the
            // server does something". BlindEdit is wrong; Remove is a
            // guess. R2 (outside edit) is the honest floor for "an
            // external system acts" - the gate will ask in `ask`, run in
            // `auto`, and a config that wants tighter writes a rule.
            |_| supra_permission::Effect::OutsideEdit { path: "an MCP server".to_owned() },
        )
        .unwrap_or_else(|_| Self::gateway_tool_fallback())
    }

    /// The impossible branch of `gateway_tool`: the static schema violates
    /// no build-time invariant (no duplicates, no empty names or
    /// descriptions - it is a constant), so registration cannot refuse.
    /// This fallback exists so the shipped path holds no `expect`; if the
    /// invariants ever disagree with the constant, the gateway still
    /// registers a callable tool and the static-schema test names the bug.
    fn gateway_tool_fallback() -> supra_tool::Tool {
        supra_tool::Tool::register(
            "mcp",
            ToolClass::Agent,
            vec![supra_tool::Field {
                name: "server".to_owned(),
                field_type: supra_tool::FieldType::Text,
                required: true,
                description: "The MCP server to call (schema degraded; report this).".to_owned(),
            }],
            |_| supra_permission::Effect::OutsideEdit { path: "an MCP server".to_owned() },
        )
        .unwrap_or_else(|_| unreachable!("the fallback schema is a single named, described field"))
    }

    /// How many cached tools one server contributes - the budget the TUI
    /// reports and the operator watches.
    ///
    /// # Errors
    ///
    /// [`McpError::Cache`] when the store refuses.
    pub fn cached_count_for_server(&self, server: &str) -> Result<i64, McpError> {
        self.store
            .with_transaction::<_, supra_store::StoreError>(TransactionBehavior::Deferred, |tx| {
                crate::cache::count_for_server(tx, server)
            })
            .map_err(McpError::Cache)
    }

    /// Dispatch one call through the gateway.
    ///
    /// The arguments arrive as the canonical text the registry validated;
    /// the gateway routes to the named server and returns the remote
    /// result as canonical JSON text.
    ///
    /// # Errors
    ///
    /// [`McpError::Transport`] when no server by that name is connected;
    /// whatever the transport and the remote server refuse otherwise.
    pub async fn call(
        &mut self,
        server: &str,
        tool: &str,
        arguments: &CanonicalJson,
    ) -> Result<String, McpError> {
        let transport = self
            .servers
            .iter_mut()
            .find(|(config, _)| config.name == server)
            .map(|(_, transport)| transport)
            .ok_or_else(|| McpError::Transport(format!("no MCP server named {server:?} is connected")))?;

        let parsed: serde_json::Value = serde_json::from_str(arguments.as_str())
            .map_err(|error| McpError::Protocol(format!("arguments are not JSON: {error}")))?;

        let result =
            transport.call("tools/call", serde_json::json!({"name": tool, "arguments": parsed})).await?;

        // `tools/call` results carry content blocks; the harness wants
        // the text. The block list is the protocol's shape; text blocks
        // are what every server sends for results a model reads.
        let text = result
            .get("content")
            .and_then(|content| content.as_array())
            .map(|blocks| {
                blocks
                    .iter()
                    .filter(|block| block.get("type").and_then(serde_json::Value::as_str) == Some("text"))
                    .filter_map(|block| block.get("text").and_then(serde_json::Value::as_str))
                    .collect::<Vec<&str>>()
                    .join("\n")
            })
            .unwrap_or_default();

        Ok(text)
    }
}

/// The namespaced tool name: `server__tool`.
///
/// # Errors
///
/// [`McpError::ToolName`] when either side is empty, the joined name
/// exceeds 128 characters, or the pair contains anything but ASCII
/// letters, digits, underscore, or hyphen - the charset T17's registry
/// and the manifest can both carry without escaping.
fn namespaced(server: &str, tool: &str) -> Result<String, McpError> {
    for (part, what) in [(server, "server name"), (tool, "tool name")] {
        if part.is_empty() || part.len() > 64 {
            return Err(McpError::ToolName {
                tool: part.to_owned(),
                reason: format!("a {what} must be 1..=64 characters"),
            });
        }
        if !part.bytes().all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-') {
            return Err(McpError::ToolName {
                tool: part.to_owned(),
                reason: format!("a {what} must be ASCII letters, digits, underscore, or hyphen"),
            });
        }
    }
    let joined = format!("{server}__{tool}");
    if joined.len() > 128 {
        return Err(McpError::ToolName {
            tool: joined,
            reason: "the namespaced name exceeds 128 characters".to_owned(),
        });
    }
    Ok(joined)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A python stdio MCP server with a fixed tool list.
    fn server_endpoint() -> Endpoint {
        Endpoint::Stdio {
            argv: vec![
                "python3".to_owned(),
                // `-u`: unbuffered stdout. A piped python buffers its
                // output, and an answer sitting in the buffer is a gateway
                // that hangs on readline - the transport tests proved it.
                "-u".to_owned(),
                "-c".to_owned(),
                r#"
import sys, json
def respond(request):
    method = request.get("method")
    if method == "initialize":
        return {"protocolVersion": "2025-06-18", "capabilities": {}, "serverInfo": {"name": "demo"}}
    if method == "tools/list":
        return {"tools": [{"name": "echo", "description": "Echo.",
                           "inputSchema": {"type": "object", "properties": {}}}]}
    if method == "tools/call":
        return {"content": [{"type": "text", "text": "echoed: " + request["params"]["arguments"].get("msg", "")}]}
    return {"error": {"code": -32601, "message": "no such method"}}
while True:
    line = sys.stdin.readline()
    if not line:
        break
    request = json.loads(line)
    if "id" in request:
        print(json.dumps({"jsonrpc": "2.0", "id": request["id"], "result": respond(request)}))
"#.to_owned(),
            ],
            env: vec![("PATH".to_owned(), "/usr/bin:/bin".to_owned())],
        }
    }

    fn store() -> Arc<Store> {
        static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let scratch = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("supra-mcp-{}-{scratch}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("dir");
        let db = dir.join("mcp.db");
        std::sync::Arc::new(Store::open(&db).expect("store"))
    }

    /// A server that paginates: page one holds `alpha`, the second page
    /// holds `beta`, and the cursor settles after it.
    fn paginated_endpoint() -> Endpoint {
        let script = r#"
import sys, json
def respond(request):
    method = request.get("method")
    if method == "initialize":
        return {"protocolVersion": "2025-06-18", "capabilities": {}, "serverInfo": {"name": "pages"}}
    if method == "tools/list":
        cursor = request["params"].get("cursor")
        if cursor is None:
            return {"tools": [{"name": "alpha", "description": "First page.",
                              "inputSchema": {"type": "object", "properties": {}}}],
                    "nextCursor": "page-2"}
        return {"tools": [{"name": "beta", "description": "Second page.",
                           "inputSchema": {"type": "object", "properties": {}}}]}
    return {"error": {"code": -32601, "message": "no such method"}}
while True:
    line = sys.stdin.readline()
    if not line:
        break
    request = json.loads(line)
    if "id" in request:
        print(json.dumps({"jsonrpc": "2.0", "id": request["id"], "result": respond(request)}))
"#;
        Endpoint::Stdio {
            argv: vec!["python3".to_owned(), "-u".to_owned(), "-c".to_owned(), script.to_owned()],
            env: vec![("PATH".to_owned(), "/usr/bin:/bin".to_owned())],
        }
    }

    /// A server whose second page holds a name that cannot survive
    /// namespacing: the first page's tool parses, the second's cannot.
    /// The cache must hold neither.
    fn half_manifest_endpoint() -> Endpoint {
        let script = r#"
import sys, json
def respond(request):
    method = request.get("method")
    if method == "initialize":
        return {"protocolVersion": "2025-06-18", "capabilities": {}, "serverInfo": {"name": "half"}}
    if method == "tools/list":
        cursor = request["params"].get("cursor")
        if cursor is None:
            return {"tools": [{"name": "good", "description": "Parses.",
                              "inputSchema": {"type": "object", "properties": {}}}],
                    "nextCursor": "page-2"}
        return {"tools": [{"name": "bad tool", "description": "Names cannot carry spaces.",
                           "inputSchema": {"type": "object", "properties": {}}}]}
    return {"error": {"code": -32601, "message": "no such method"}}
while True:
    line = sys.stdin.readline()
    if not line:
        break
    request = json.loads(line)
    if "id" in request:
        print(json.dumps({"jsonrpc": "2.0", "id": request["id"], "result": respond(request)}))
"#;
        Endpoint::Stdio {
            argv: vec!["python3".to_owned(), "-u".to_owned(), "-c".to_owned(), script.to_owned()],
            env: vec![("PATH".to_owned(), "/usr/bin:/bin".to_owned())],
        }
    }

    #[tokio::test]
    async fn a_paginated_server_contributes_every_page() {
        let store = store();
        let (gateway, refusals) = Gateway::probe(
            store,
            vec![ServerConfig { name: "pages".to_owned(), endpoint: paginated_endpoint() }],
        )
        .await;
        assert!(refusals.is_empty(), "{refusals:?}");

        let cached = gateway.cached_tools().expect("cache");
        let names: Vec<&str> = cached.iter().map(|row| row.tool.as_str()).collect();
        assert_eq!(names, vec!["pages__alpha", "pages__beta"], "both pages landed");
    }

    #[tokio::test]
    async fn a_refused_page_leaves_no_partial_manifest_behind() {
        let store = store();
        let (gateway, refusals) = Gateway::probe(
            std::sync::Arc::clone(&store),
            vec![ServerConfig { name: "half".to_owned(), endpoint: half_manifest_endpoint() }],
        )
        .await;
        assert_eq!(refusals.len(), 1, "the probe refused the bad page");
        assert_eq!(refusals[0].0, "half");

        let cached = gateway.cached_tools().expect("cache");
        assert!(
            cached.iter().all(|row| !row.tool.starts_with("half__")),
            "no row from the refused server survived: {cached:?}"
        );
    }

    #[tokio::test]
    async fn probe_caches_tools_and_call_round_trips() {
        let store = store();
        let (mut gateway, refusals) = Gateway::probe(
            std::sync::Arc::clone(&store),
            vec![ServerConfig { name: "demo".to_owned(), endpoint: server_endpoint() }],
        )
        .await;
        assert!(refusals.is_empty(), "{refusals:?}");

        let cached = gateway.cached_tools().expect("cache");
        assert_eq!(cached.len(), 1);
        assert_eq!(cached[0].tool, "demo__echo");
        assert!(cached[0].live, "a fresh probe marks its rows live");

        // The static gateway tool never changes with what servers offer.
        let tool = Gateway::gateway_tool();
        assert_eq!(tool.name(), "mcp");
        assert_eq!(tool.schema().fields().len(), 3);

        // A call through the gateway reaches the remote and returns its text.
        let arguments = CanonicalJson::from_canonical(r#"{"msg":"hi"}"#.to_owned());
        let answer = gateway.call("demo", "echo", &arguments).await.expect("call");
        assert_eq!(answer, "echoed: hi");
    }

    #[tokio::test]
    async fn a_failed_server_is_named_and_the_rest_proceeds() {
        let store = store();
        let (gateway, refusals) = Gateway::probe(
            store,
            vec![
                ServerConfig {
                    name: "dead".to_owned(),
                    endpoint: Endpoint::Stdio {
                        argv: vec!["/nonexistent-supra-mcp-probe".to_owned()],
                        env: vec![],
                    },
                },
                ServerConfig { name: "demo".to_owned(), endpoint: server_endpoint() },
            ],
        )
        .await;
        assert_eq!(refusals.len(), 1);
        assert_eq!(refusals[0].0, "dead");
        assert_eq!(gateway.servers.len(), 1, "the live server proceeded");
    }

    #[tokio::test]
    async fn a_second_probe_appends_only_new_tools() {
        let store = store();
        let endpoint = server_endpoint();
        let first = Gateway::probe(
            std::sync::Arc::clone(&store),
            vec![ServerConfig { name: "demo".to_owned(), endpoint }],
        )
        .await;
        assert!(first.1.is_empty());

        // Probe again: the same tool appends nothing (the row stands), so
        // the cache - and the manifest it feeds - is stable.
        let second = Gateway::probe(
            std::sync::Arc::clone(&store),
            vec![ServerConfig { name: "demo".to_owned(), endpoint: server_endpoint() }],
        )
        .await;
        assert!(second.1.is_empty());

        let cached = second.0.cached_tools().expect("cache");
        assert_eq!(cached.len(), 1, "no duplicate rows from a second probe");
    }

    #[test]
    fn namespacing_refuses_what_it_cannot_carry() {
        assert_eq!(namespaced("srv", "echo").expect("ok"), "srv__echo");
        let long = "x".repeat(65);
        for (server, tool) in
            [("", "echo"), ("srv", ""), ("s r v", "echo"), ("srv", "e cho"), (long.as_str(), "echo")]
        {
            assert!(namespaced(server, tool).is_err(), "{server:?} / {tool:?}");
        }
    }

    #[tokio::test]
    async fn a_server_over_the_budget_is_refused_at_probe() {
        // A server that advertises thousands of tools would spend the
        // session's manifest budget on one remote; the budget refuses at
        // probe time, before any byte reaches the model.
        let store = store();
        let tools: Vec<String> = (0..300)
            .map(|index| format!(r#"{{"name": "tool{index}", "description": "d", "inputSchema": {{}}}}"#))
            .collect();
        let script = format!(
            r#"
import sys, json
while True:
    line = sys.stdin.readline()
    if not line:
        break
    request = json.loads(line)
    if request.get("method") == "initialize":
        print(json.dumps({{"jsonrpc": "2.0", "id": request["id"], "result": {{"protocolVersion": "2025-06-18"}}}}))
    elif request.get("method") == "tools/list":
        print(json.dumps({{"jsonrpc": "2.0", "id": request["id"], "result": {{"tools": [{}]}}}}))
"#,
            tools.join(",")
        );
        let (_gateway, refusals) = Gateway::probe(
            store,
            vec![ServerConfig {
                name: "big".to_owned(),
                endpoint: Endpoint::Stdio {
                    argv: vec![
                        "python3".to_owned(),
                        // `-u`: unbuffered stdout, as the transport tests
                        // proved a piped python otherwise buffers.
                        "-u".to_owned(),
                        "-c".to_owned(),
                        script,
                    ],
                    env: vec![("PATH".to_owned(), "/usr/bin:/bin".to_owned())],
                },
            }],
        )
        .await;
        assert_eq!(refusals.len(), 1, "{refusals:?}");
        assert!(matches!(refusals[0].1, McpError::TooManyTools { count: 300, .. }), "{:?}", refusals[0].1);
    }

    #[tokio::test]
    async fn the_stdio_child_sees_only_the_explicit_env() {
        // The host env holds secrets no third-party server needs; the
        // child's env is exactly what the configuration said. The probe
        // server refuses to answer at all if any of a handful of markers
        // every harness process carries (cargo sets them for its own
        // children; the shell sets the rest) is present. The child's own
        // locale additions (LC_CTYPE) are the interpreter's doing, not a
        // leak, so they are not part of the check.
        let script = r#"
import sys, json, os
HOST_MARKERS = ("HOME", "USER", "SHELL", "CARGO", "RUSTUP", "LANG", "TERM")
while True:
    line = sys.stdin.readline()
    if not line:
        break
    request = json.loads(line)
    if request.get("method") == "initialize":
        if any(marker in os.environ for marker in HOST_MARKERS):
            sys.exit(3)
        print(json.dumps({"jsonrpc": "2.0", "id": request["id"], "result": {"protocolVersion": "2025-06-18"}}))
    elif request.get("method") == "tools/list":
        print(json.dumps({"jsonrpc": "2.0", "id": request["id"], "result": {"tools": []}}))
"#;
        let store = store();
        let (_gateway, refusals) = Gateway::probe(
            store,
            vec![ServerConfig {
                name: "clean".to_owned(),
                endpoint: Endpoint::Stdio {
                    argv: vec!["python3".to_owned(), "-u".to_owned(), "-c".to_owned(), script.to_owned()],
                    env: vec![("PATH".to_owned(), "/usr/bin:/bin".to_owned())],
                },
            }],
        )
        .await;
        assert!(refusals.is_empty(), "the child must see only the explicit env: {refusals:?}");
    }

    #[tokio::test]
    async fn non_text_content_blocks_are_not_answered_as_text() {
        // `tools/call` results can carry image and resource blocks; the
        // gateway answers with text blocks only, and a non-text block
        // must not surface as one - a base64 image blob rendered as text
        // is corruption wearing a reply.
        let script = r#"
import sys, json
while True:
    line = sys.stdin.readline()
    if not line:
        break
    request = json.loads(line)
    if request.get("method") == "initialize":
        print(json.dumps({"jsonrpc": "2.0", "id": request["id"], "result": {"protocolVersion": "2025-06-18"}}))
    elif request.get("method") == "tools/list":
        print(json.dumps({"jsonrpc": "2.0", "id": request["id"], "result": {"tools": [{"name": "echo", "description": "Echo.", "inputSchema": {"type": "object", "properties": {}}}]}}))
    elif request.get("method") == "tools/call":
        print(json.dumps({"jsonrpc": "2.0", "id": request["id"], "result": {"content": [
            # The non-text blocks carry a `text` field too: a filter that
            # reads `text` without checking `type` would answer with all
            # three - which is exactly the mutation this test closes.
            {"type": "image", "data": "aGVsbG8=", "mimeType": "image/png", "text": "image payload"},
            {"type": "text", "text": "the real text"},
            {"type": "resource", "resource": {"uri": "file:///x"}, "text": "resource payload"},
        ]}}))
"#;
        let store = store();
        let (mut gateway, refusals) = Gateway::probe(
            store,
            vec![ServerConfig {
                name: "mixed".to_owned(),
                endpoint: Endpoint::Stdio {
                    argv: vec!["python3".to_owned(), "-u".to_owned(), "-c".to_owned(), script.to_owned()],
                    env: vec![("PATH".to_owned(), "/usr/bin:/bin".to_owned())],
                },
            }],
        )
        .await;
        assert!(refusals.is_empty(), "{refusals:?}");
        let answer = gateway
            .call("mixed", "echo", &supra_types::CanonicalJson::from_canonical("{}".to_owned()))
            .await
            .expect("call");
        assert_eq!(answer, "the real text", "only the text block, no image or resource leakage");
    }

    #[test]
    fn the_gateway_tool_is_static_no_matter_what_the_fleet_offers() {
        // I3's "static gateway schema" as a test: the same registration
        // bytes for every fleet, and the effect resolver ignores the
        // arguments entirely - what the servers offer changes content,
        // never the tool.
        let before = Gateway::gateway_tool();
        let schema = before.schema().to_provider_schema();
        assert_eq!(schema["type"], serde_json::json!("object"));
        assert_eq!(
            schema["required"],
            serde_json::json!(["server", "tool", "arguments"]),
            "the three fields, always - and the primary path won, not the fallback"
        );

        // The resolver is argument-blind: any arguments resolve to the
        // same effect, because the effect of "an external system acts" is
        // not knowable from the arguments. The empty map matters: a
        // resolver keyed on `args.contains_key("server")` would flip
        // between the two.
        let empty = serde_json::Map::new();
        let effect_a = before.resolve_effect(&empty);
        let mut map = serde_json::Map::new();
        map.insert("server".to_owned(), serde_json::json!("anything"));
        let effect_b = before.resolve_effect(&map);
        assert_eq!(effect_a, effect_b, "the resolver ignores the arguments");
    }

    #[tokio::test]
    async fn a_call_to_an_unknown_server_is_named() {
        let store = store();
        let (mut gateway, _) = Gateway::probe(store, vec![]).await;
        let error = gateway
            .call("nope", "echo", &CanonicalJson::from_canonical("{}".to_owned()))
            .await
            .expect_err("unknown");
        assert!(matches!(error, McpError::Transport(_)), "{error}");
    }
}
