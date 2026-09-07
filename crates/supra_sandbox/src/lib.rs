//! Process-isolation policy layer.
//!
//! **T16** of the stage sequence. The stage sits on top of the C++ sandbox
//! the FFI crate already binds: the C side does the work, this crate is the
//! shape a turn-loop call site uses to ask for that work. Three pieces live
//! in one crate because the order between them is the product:
//!
//! - **The fd audit** ([`fd_audit`]) runs before the C side does. The T4 note
//!   is the binding: "descriptors inherited across `exec` remain usable. T16
//!   and T16.5 must close descriptors they do not intend to pass." A
//!   filesystem policy that does not also audit descriptors is a policy
//!   that the child can defeat by reading the host's pipes.
//!
//! - **The host-side spawn** ([`spawn`]) composes the audit, the guard
//!   (T12.5), and the FFI in that order. A refusal at any step returns
//!   without touching the next; a future contributor who reorders the
//!   steps is removing a layer the design says must hold.
//!
//! - **The process-tree budget** ([`tree::TreeBudget`]) catches what the
//!   guard cannot. The T12.5 note is the binding: "the env-marker guard
//!   layer can be stripped by a command that deliberately clears the
//!   environment, and the process-tree budget catches the consequence
//!   rather than the intent." A counter is the only shape that does not
//!   try to model the un-modellable.
//!
//! # What this stage is **not** for
//!
//! T16.5 takes the persistent-PTY side; T16.6 takes the write-ahead
//! journal; T16.7 takes the reversibility classifier. This crate composes
//! them by reference - it does not own their shapes. A change to the
//! T16.7 matrix would land in `supra_types` and propagate; a change to
//! the C-side Landlock ABI would land in `supra_ffi` and propagate. T16
//! is the host-side glue, not the policy.
//!
//! # Authority vs consent
//!
//! A refusal returned by this crate is one of two shapes:
//! [`crate::error::SandboxError::Authority`] (a guard layer said no) or
//! [`crate::error::SandboxError::Consent`] (the permission gate said no).
//! The shapes are distinct because the user-relaxable axis is the second
//! only. `yolo` skips consent; nothing here skips authority.
//!
//! # Usage
//!
//! ```no_run
//! use supra_sandbox::{default_policy, spawn, SpawnRequest, TreeBudget};
//! use supra_types::{Mode, Reversibility};
//!
//! let workspace = std::env::temp_dir();
//! let mut policy = default_policy(&workspace);
//! policy.allow_inherited_path("/var/log/supra.log");
//!
//! let argv = ["/bin/true"];
//! let tree = TreeBudget::default();
//! let request = SpawnRequest {
//!     argv: &argv,
//!     env: &[],
//!     working_dir: None,
//!     reversibility: Reversibility::R0,
//!     mode: Mode::Auto,
//!     marker: None,
//!     lineage: None,
//!     child: None,
//!     claim_vote: None,
//! };
//! let _process = spawn(&policy, &request, &tree).expect("the spawn refused");
//! # Ok::<(), supra_sandbox::SandboxError>(())
//! ```

#![deny(missing_docs)]
// `unsafe_code = "warn"` is set workspace-wide; this crate is the only
// stage that *calls* `unsafe` indirectly through `supra_ffi`, but the
// `unsafe` keyword itself does not appear here. Confinement is structural:
// the FFI is the only crate in the workspace that holds it, and every
// other crate honours the rule by not reaching for it. A re-allow here
// would mean a missing abstraction in `supra_ffi`, not a local need.
// Tests assert with `.expect()` and `panic!`. Scoped to `cfg(test)` so no
// allow reaches a shipped path.
#![cfg_attr(test, allow(clippy::expect_used, clippy::panic, clippy::print_stderr))]

pub mod error;
pub mod fd_audit;
pub mod policy;
pub mod spawn;
pub mod tree;

pub use error::SandboxError;
pub use fd_audit::{DescriptorRecord, find_leaks, snapshot as snapshot_descriptors};
pub use policy::{SandboxPolicy, bootstrap_paths, default_policy};
pub use spawn::{SpawnRequest, SpawnTicket, platform_tier, spawn, status_summary};
pub use tree::TreeBudget;

/// Sandbox policy layer is on the turn loop and on tool calls, so this is a
/// requirement rather than an observation.
const _: () = {
    const fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<SandboxPolicy>();
    assert_send_sync::<TreeBudget>();
};
