//! The host-side permission gate.
//!
//! **T16.7** of the stage sequence. Two axes, kept apart because conflating
//! them is what makes "just run it" dangerous:
//!
//! | Axis | Question | Owned by | User-relaxable |
//! | ---- | -------- | -------- | -------------- |
//! | `ToolClass` | *who* may invoke | `supra_types` (T6), enforced by T20's WASM imports | never |
//! | Consent | does this need the user's yes | this crate's [`gate()`], via `Mode` | yes - that is what modes are for |
//!
//! What T16.7 adds on top of T6's shapes:
//!
//! - **The catalogue** ([`catalogue`]): a resolved effect, as a closed
//!   enumeration of shapes, classified into a [`supra_types::Reversibility`].
//!   Classification runs on the resolved effect, never the tool name -
//!   `shell_run("cargo test")` is R0 and `shell_run("rm -rf node_modules")`
//!   is R3, and the difference is data the caller supplies, not a substring
//!   this crate guesses at. The structural shape (T15.7's reparse-gated
//!   splice) is the one damping applies to: verified structure earns one
//!   class lower than a blind edit on the same file, measurably.
//! - **The gate** ([`gate()`]): explicit deny first; non-relaxable authority
//!   second; mandatory escape-hatch ceremony third; ordinary consent last.
//!   An allow may skip only the final mode/reversibility decision. It cannot
//!   manufacture caller authority or waive a separate ceremony.
//!
//!   The escape-hatch exception: a guard-stepping-aside effect asks in
//!   *every* mode and under every allow rule, because consent is not the axis
//!   a sandbox rides on.
//! - **Batched prompts** ([`Batch`]): many questions become one prompt,
//!   answers come back per item, and an unanswered item refuses rather
//!   than runs - a question the user did not answer is not consent.
//!
//! # What this crate deliberately does not own
//!
//! Rule *loading* and precedence assembly live with configuration (T16.8 /
//! T30): the gate evaluates the `&[Rule]` it is handed, so the same gate
//! serves every source set. Prompt *rendering* lives with the TUI (T29):
//! the gate produces [`Outcome`] values with reasons; it never draws. The
//! mode matrix itself is `supra_types`' (T6) - re-deriving it here would
//! give two implementations of one table, the mistake this project keeps
//! refusing.
//!
//! # Usage
//!
//! ```
//! use supra_permission::{Batch, Effect, Request, gate};
//! use supra_types::{Invoker, Mode, ToolClass};
//!
//! let request = Request {
//!     effect: Effect::Remove { target: "node_modules".to_owned() },
//!     class: ToolClass::Agent,
//!     invoker: Invoker::Host,
//!     summary: "shell_run rm -rf node_modules".to_owned(),
//! };
//!
//! let outcome = gate(&request, Mode::Auto, &[]);
//! let batch = Batch::collect(&[outcome]).expect("there was an ask");
//! let decisions = batch.resolve(&[true]);
//! assert!(decisions[0].is_run());
//! # Ok::<(), supra_permission::PermissionError>(())
//! ```

#![deny(missing_docs)]
// Tests assert with `.expect()` and `panic!`. Scoped to `cfg(test)` so no
// allow reaches a shipped path.
#![cfg_attr(test, allow(clippy::expect_used, clippy::panic, clippy::print_stderr))]

pub mod catalogue;
pub mod error;
pub mod gate;

pub use catalogue::{Effect, RecoveryBasis};
pub use error::PermissionError;
pub use gate::{AskReason, Batch, Outcome, RefuseReason, Request, gate};

impl Outcome {
    /// Whether this outcome permits the request to proceed.
    ///
    /// The one-word answer a turn loop wants; the reason stays attached for
    /// the surfaces that render it.
    #[must_use]
    pub const fn is_run(&self) -> bool {
        matches!(self, Self::Run)
    }
}

/// The gate sits on every tool invocation, so `Send + Sync` is a requirement
/// rather than an observation.
const _: () = {
    const fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<Request>();
    assert_send_sync::<Outcome>();
    assert_send_sync::<Batch>();
};
