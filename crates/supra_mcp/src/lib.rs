//! MCP client gateway: stdio and HTTP transports behind one static
//! gateway schema, manifests cached to SQLite, discovery by append.
//!
//! **T18** of the stage sequence. The binding is I3, stated in full:
//! "Every tool is registered at startup; MCP servers are probed once and
//! their manifests cached to SQLite. Disabling a tool uses `allowed_tools`
//! or `tool_choice`, never removal. Dynamic capability sits behind a
//! **static gateway schema**, and newly discovered tools are described by
//! appending - append does not break a prefix, so dynamic discovery
//! becomes cache-compatible."
//!
//! What that means as types:
//!
//! - **The static gateway** ([`Gateway::gateway_tool`]): one tool, `mcp`,
//!   whose three-field manifest (`server`, `tool`, `arguments`) never
//!   changes no matter what the fleet offers. The prefix the model saw at
//!   BP1 cannot break, because there is nothing dynamic in `tools` to
//!   break it.
//! - **The probe, once** ([`Gateway::probe`]): handshake, list, cache -
//!   at construction or not at all. A server that refuses (down, wrong
//!   version, over budget) is named and skipped; the fleet proceeds.
//! - **The cache** ([`cache`]): the third `schema_component` owner.
//!   Append-only by construction - `INSERT OR IGNORE`, never update,
//!   never delete - because a manifest that shrank would break the
//!   prefix exactly like a rewritten one. Discovery appends; that is the
//!   whole compatibility trick.
//! - **The transports** ([`transport`]): stdio (a child process speaking
//!   newline-delimited JSON) and HTTP (POST), behind one `call` shape.
//!   stdio children run with an explicit environment - the host's env
//!   holds secrets no third-party server needs.
//! - **The budget** (`TOOLS_PER_SERVER`): a server that advertises
//!   thousands of tools would spend the session's manifest budget on one
//!   remote; the budget refuses at probe time.
//!
//! # No SDK
//!
//! MCP is JSON-RPC 2.0 with a handful of methods. The envelope here is
//! ~40 lines and owned; an SDK's version pins would fight the workspace's,
//! and its grammar would be a second implementation of one this crate can
//! state in full - the T15.7 lesson applied to dependencies.
//!
//! # Usage
//!
//! ```no_run
//! # async fn demo() -> Result<(), supra_mcp::McpError> {
//! use std::sync::Arc;
//! use supra_mcp::{Endpoint, Gateway, ServerConfig};
//! use supra_store::Store;
//!
//! let store = Arc::new(Store::open("session.db").expect("store"));
//! let servers = vec![ServerConfig {
//!     name: "demo".to_owned(),
//!     endpoint: Endpoint::Stdio {
//!         argv: vec!["demo-server".to_owned()],
//!         env: vec![],
//!     },
//! }];
//!
//! let (mut gateway, refusals) = Gateway::probe(store, servers).await;
//! assert!(refusals.is_empty(), "or the named server is down: {refusals:?}");
//!
//! let answer = gateway
//!     .call("demo", "echo", &supra_types::CanonicalJson::from_canonical(
//!         r#"{"msg":"hi"}"#.to_owned(),
//!     ))
//!     .await?;
//! # Ok(())
//! # }
//! ```

#![deny(missing_docs)]
// Tests assert with `.expect()` and `panic!`. Scoped to `cfg(test)` so no
// allow reaches a shipped path.
#![cfg_attr(test, allow(clippy::expect_used, clippy::panic, clippy::print_stderr))]

pub mod cache;
pub mod error;
pub mod gateway;
pub mod rpc;
pub mod transport;

pub use error::McpError;
pub use gateway::{Gateway, ServerConfig, TOOLS_PER_SERVER};
pub use transport::{Endpoint, Transport};

/// The gateway rides the turn loop behind the registry's static `mcp`
/// tool, so `Send + Sync` is a requirement rather than an observation.
const _: () = {
    const fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<Gateway>();
    assert_send_sync::<ServerConfig>();
    assert_send_sync::<CachedTool>();
};

pub use cache::CachedTool;
