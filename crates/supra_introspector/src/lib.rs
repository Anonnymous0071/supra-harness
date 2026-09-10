//! Static, dynamic, and cross-agent bug detection. **T22**: findings
//! carry evidence, the blackboard decides.
//!
//! The three gates mirror the turn loop's step 7 and the memory mandate:
//! static analysis (clippy via `run_gate`, plus anything the digest's
//! symbol index can answer without a tool), dynamic execution (`cargo
//! test`), and cross-agent comparison (peer answers to one question;
//! divergence is a finding, never a verdict). A finding never escalates
//! on its own - `bridge` publishes it as a claim and peers vote, so
//! quorum (T21), not a linter, decides.
//!
//! ```
//! use supra_introspector::cross::{Answer, cross_check};
//! use supra_types::AgentId;
//!
//! let answers = vec![
//!     Answer { agent: AgentId::generate(), text: "fix a".to_owned() },
//!     Answer { agent: AgentId::generate(), text: "fix a".to_owned() },
//! ];
//! assert!(cross_check("the fix", &answers).expect("unique agents").is_empty());
//! ```

#![deny(missing_docs)]
#![cfg_attr(test, allow(clippy::expect_used, clippy::panic, clippy::print_stderr))]

/// The blackboard bridge: findings as claims, votes with evidence.
pub mod bridge;
/// Cross-agent comparison: divergence is a finding, never a verdict.
pub mod cross;
/// The refusal shapes.
pub mod error;
/// The finding type and its evidence reference.
pub mod finding;
/// The static and dynamic gates, run over the workspace.
pub mod gates;

pub use bridge::{append_finding, escalates, vote_finding};
pub use cross::{Answer, cross_check};
pub use error::IntrospectError;
pub use finding::{Finding, Kind};
pub use gates::{GateRun, dynamic_gate, parse_cargo_diagnostic, run_gate, static_gate};
