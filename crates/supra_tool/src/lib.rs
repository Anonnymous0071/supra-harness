//! The tool registry: frozen manifests, robust invocation for weak
//! tool-callers, and instruction-carrying preconditions.
//!
//! **T17** of the stage sequence - the tool surface between the model and
//! the harness. Three bindings shaped it:
//!
//! - **I3** (architecture): tools are frozen for the session lifetime.
//!   Every tool registers at startup; disabling uses `allowed_tools`, never
//!   removal; the manifest is byte-stable so the prefix at BP1 never
//!   breaks. [`Registry`] makes this structural: no `remove` exists, and
//!   [`Registry::disable`] is a session set the dispatcher consults, not a
//!   mutation of the registry.
//! - **§5 instruction efficiency**: the workflow is not described in
//!   prose; it is the only path the tool surface permits. [`Precondition`]
//!   is that principle as a type - `edit_file` fails with a structured
//!   instruction (``read_file {path} before editing it``) when the file was
//!   not read this session, replacing ~10 unreliable prompt tokens with
//!   zero prompt tokens and a reliable refusal.
//! - **The robust-invocation mandate**: a weak tool-calling model must
//!   never lose the loop to a tool error. Every refusal in
//!   [`ToolError`] is structured and field-naming - the unknown tool lists
//!   the registered names, the bad argument names the shape that arrived,
//!   the schema refusal names the field - because the message is the
//!   model's only channel and the retry has to be able to be correct.
//!
//! # One serialiser, no second parse
//!
//! Argument text goes through [`supra_llm::canonicalize`] exactly once -
//! the strict, duplicate-refusing serialiser that is the only sanctioned
//! producer of [`CanonicalJson`] (T13). The invocation carries the
//! canonical bytes to the ledger unchanged, so what was hashed is what
//! reaches the wire. This crate never parses JSON outside that path;
//! re-serialising a parsed value on the way out is how invisible cache
//! breaks happen, and it does not happen here.
//!
//! # Registry vs gate vs executor
//!
//! The registry validates and resolves; it does not decide or run. The
//! resolved effect ([`Effect`], T16.7's catalogue) travels with the
//! invocation to the permission gate, which the turn loop (T23) owns -
//! classification runs on the resolved effect, never the tool name, and
//! the registry is the layer that knows the arguments. Execution dispatch
//! is the turn loop's too: the registry's job ends at a validated
//! [`Invocation`].
//!
//! # Usage
//!
//! ```
//! use std::collections::BTreeSet;
//! use supra_permission::Effect;
//! use supra_tool::{Field, FieldType, Precondition, Registry, SessionFacts, Tool};
//! use supra_types::ToolClass;
//!
//! let edit = Tool::register(
//!     "edit_file",
//!     ToolClass::Agent,
//!     vec![
//!         Field { name: "path".into(), field_type: FieldType::Text, required: true, description: "The file.".into() },
//!         Field { name: "old".into(), field_type: FieldType::Text, required: true, description: "Text to replace.".into() },
//!         Field { name: "new".into(), field_type: FieldType::Text, required: true, description: "Replacement.".into() },
//!     ],
//!     |_| Effect::BlindEdit,
//! )
//! .expect("register")
//! .with_precondition(Precondition::read_before_edit());
//!
//! let mut registry = Registry::new();
//! registry.register(edit).expect("register");
//!
//! // Not read yet: the refusal carries the instruction.
//! let error = registry
//!     .invoke("edit_file", r#"{ "path": "a.rs", "old": "x", "new": "y" }"#, &SessionFacts::default())
//!     .expect_err("the precondition refuses");
//! assert!(error.to_string().contains("read_file"));
//!
//! // Read, then edit: the invocation is validated, canonical, and carries
//! // its effect for the gate.
//! let facts = SessionFacts { files_read: BTreeSet::from(["a.rs".to_owned()]) };
//! let invocation = registry
//!     .invoke("edit_file", r#"{ "path": "a.rs", "old": "x", "new": "y" }"#, &facts)
//!     .expect("invoke");
//! assert_eq!(invocation.arguments().as_str(), r#"{"new":"y","old":"x","path":"a.rs"}"#);
//! # Ok::<(), supra_tool::ToolError>(())
//! ```

#![deny(missing_docs)]
// Tests assert with `.expect()` and `panic!`. Scoped to `cfg(test)` so no
// allow reaches a shipped path.
#![cfg_attr(test, allow(clippy::expect_used, clippy::panic, clippy::print_stderr))]

pub mod error;
pub mod registry;
pub mod schema;

pub use error::ToolError;
pub use registry::{Invocation, Precondition, Registry, SessionFacts, Tool};
pub use schema::{Field, FieldType, Schema};

/// The registry rides the turn loop and the tool array is rendered once
/// per session, so `Send + Sync` is a requirement rather than an
/// observation.
const _: () = {
    const fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<Registry>();
    assert_send_sync::<Tool>();
    assert_send_sync::<SessionFacts>();
    assert_send_sync::<Invocation>();
};
