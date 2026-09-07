//! What a T16 policy operation refused, and who is responsible for fixing it.
//!
//! Two distinct refusal shapes appear, and a crate that conflates them forces
//! the caller to guess. The variants here name the shape, not the cause - a
//! security boundary that says "what happened" is one the operator can act
//! on, where "why" alone invites a fishing expedition.
//!
//! | Variant | Shape | Where to look |
//! | ------- | ----- | ------------- |
//! | `Authority` | the guard's seven layers refused the spawn | `supra_guard` - marker, lineage, self-vote |
//! | `Consent` | the permission gate said no, or no decision was reached | T16.7 |
//! | `LeakyDescriptor` | the audit found an fd without `FD_CLOEXEC` | the harness, not the user |
//! | `Unsupported` | the policy requested a feature the platform lacks | sandboxing tier, or bwrap absent |
//!
//! `Ffi` wraps the underlying binding's refusal verbatim: the C side names the
//! step (unshare, landlock, execve), and the message is what the operator
//! reads first. Collapsing it into a flat string would be the same mistake as
//! conflating authority and consent.

use thiserror::Error;

/// A T16 refusal.
///
/// Each variant names the shape in the message, so a log line tells the
/// operator which kind of failure happened without consulting this module.
#[derive(Clone, Debug, PartialEq, Eq, Error)]
#[non_exhaustive]
pub enum SandboxError {
    /// The seven-layer guard (T12.5) refused the spawn.
    ///
    /// Authority is never relaxable: a `yolo` invocation does not lower a
    /// layer. `Refusals` is the full list - one refusal would hide what
    /// several layers saw, and the operator needs the full picture to tell
    /// a targeted attempt from a confused one.
    #[error("guard refused the spawn across {0} layer(s); details: {1:?}")]
    Authority(usize, Vec<supra_guard::Refusal>),

    /// The permission gate refused the spawn.
    ///
    /// `mode` is what the user asked for, `reversibility` is what the
    /// classifier found. The two together let the message name both the
    /// decision and the matrix cell it came from, so a T16.7 reader can
    /// reproduce the verdict without re-deriving it.
    #[error("permission gate refused the spawn: {mode:?} / {reversibility:?}")]
    Consent {
        /// The mode in force at the decision.
        mode: supra_types::Mode,
        /// The reversibility the classifier assigned.
        reversibility: supra_types::Reversibility,
    },

    /// The pre-spawn audit found a descriptor without `FD_CLOEXEC`.
    ///
    /// The T4 note is the binding: "Not isolation from already-open
    /// descriptors. Anything inherited across `exec` stays usable. The caller
    /// must close what it does not intend to pass." This variant is the
    /// refusal that the caller did not. A `yolo` invocation does not
    /// silence it - the audit is authority, not consent.
    #[error("leaky descriptor fd {fd} ({path}); every host fd must have FD_CLOEXEC before the child execs")]
    LeakyDescriptor {
        /// The descriptor number.
        fd: i32,
        /// What the descriptor resolves to, when the audit could name it.
        path: String,
    },

    /// The platform lacks a feature the policy requires.
    ///
    /// Surfaced rather than swallowed: a sandbox that reports success while
    /// enforcing nothing is worse than no sandbox at all, because the caller
    /// stops looking. T30's `sandboxing` budget fails the run when this
    /// variant appears, rather than allowing a silent degrade.
    #[error("the platform does not support the requested policy: {detail}")]
    Unsupported {
        /// What the platform could not provide.
        detail: String,
    },

    /// The underlying FFI call refused; the detail comes from the C side.
    ///
    /// Kept verbatim: the C side names the step (`unshare`, `landlock`,
    /// `execve`), and the message is what the operator reads first.
    #[error("libsupra_sandbox refused: {0}")]
    Ffi(String),
}
