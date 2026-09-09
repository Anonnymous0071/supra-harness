//! Eight lifecycle hooks, prefix-safe by type. **T27** of the stage
//! sequence.
//!
//! A hook is a user command that runs at a lifecycle point. The safety
//! rule is structural, not advisory: only the four boundary points
//! (session start, session end, turn start, turn end) accept hooks,
//! and the four inside points (before/after tool, before evict, after
//! cache break) refuse at registration - a user command executing
//! mid-prefix can mutate what the provider has already cached, and a
//! cache break is the tax nobody asked for.
//!
//! A hook is an observer, not a gate: a failing command is reported
//! but does not stop the other hooks, and exit 42 requests a `Stop`
//! that the caller may honor. The triggering event travels as JSON on
//! the hook's stdin, plus `SUPRA_HOOK_POINT` and `SUPRA_TURN_COUNT`
//! in the environment.
//!
//! ```
//! use supra_hook::{Hook, Registry};
//!
//! let mut registry = Registry::new();
//! registry
//!     .register_named("turn-start", "cargo test --quiet")
//!     .expect("turn-start is a boundary point");
//! assert_eq!(registry.len(), 1);
//!
//! let refused = registry.register_named("before-tool", "cargo test --quiet");
//! assert!(refused.is_err(), "before-tool is inside the prefix");
//! ```

#![deny(missing_docs)]
#![cfg_attr(test, allow(clippy::expect_used, clippy::panic, clippy::print_stderr))]

/// The refusal shapes.
pub mod error;
/// The eight points and the prefix-safety rule.
pub mod point;
/// The registry: register, look up, fire.
pub mod registry;

pub use error::HookError;
pub use point::{HookContext, HookOutcome, HookPoint};
pub use registry::{Hook, Registry};

const _: () = {
    const fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<Registry>();
    assert_send_sync::<Hook>();
    assert_send_sync::<HookContext>();
};
