//! Persistent pty shell sessions, spawned through the T16 sandbox.
//!
//! **T16.5** of the stage sequence. The T4 notes bind this stage twice, and
//! both bindings shaped the crate:
//!
//! - "descriptors inherited across `exec` remain usable. T16 and T16.5 must
//!   close descriptors they do not intend to pass." The pty pair is born
//!   CLOEXEC (`supra_ffi::pty`), the slave is handed to the child as its
//!   stdio through the sandbox, and the parent's copy closes the moment the
//!   spawn succeeds. The fd audit runs before every exec, because the child
//!   is a subprocess and the host's pipes are not its business.
//! - The interactive-prompt detection is a heuristic with false positives
//!   (T4's verification), which is why [`shaping::Shaped::suggests_prompt`]
//!   is a *flag the caller asks about*, never a kill trigger.
//!
//! T3's verification binds the shaping too: the C1-positional rule lives in
//! `libsupra_ansi`, and every byte this crate reads routes through
//! `supra_ffi::ansi::Scanner` - no reimplementation, because two parsers for
//! one grammar disagree on exactly the bytes that matter (T15's lesson,
//! restated for escape sequences).
//!
//! # The shape of a session
//!
//! [`session::ShellSession`] owns one pty pair, one child, and one
//! [`shaping::Shaper`]. `read` feeds the master into the shaper and returns
//! the raw bytes for consumers that want them; `render` pulls the shaped
//! transcript out; `write` sends input; `resize` reaches the child through
//! the kernel. Drop kills the child and closes the pair.
//!
//! # Authority vs consent
//!
//! The session has no unsandboxed path. `yolo` does not skip the sandbox
//! here - this crate has no flag for it, by construction. A caller that
//! wants a command outside the sandbox must not use this crate, and the
//! permission layer (T16.7) is what polices that choice.
//!
//! # Usage
//!
//! ```no_run
//! use supra_ffi::width::Ambiguous;
//! use supra_sandbox::{default_policy, TreeBudget};
//! use supra_shell::{ShellSession, Shaper, spawn_simple};
//!
//! let workspace = std::env::temp_dir();
//! let policy = default_policy(&workspace);
//! let tree = TreeBudget::default();
//! let shaper = Shaper::new(120, 1024, Ambiguous::Narrow);
//!
//! let mut session = spawn_simple(
//!     &policy,
//!     &["/bin/sh", "-c", "echo hi"],
//!     &[("PATH", "/usr/bin:/bin")],
//!     Some(workspace.as_path()),
//!     &tree,
//!     supra_ffi::pty::PtySize::default_size(),
//!     shaper,
//! )
//! .expect("spawn");
//!
//! let mut raw = Vec::new();
//! session.read(&mut raw).expect("read");
//! session.wait(None).expect("wait");
//! # Ok::<(), supra_shell::ShellError>(())
//! ```

#![deny(missing_docs)]
// `unsafe` never appears in this crate: every descriptor, syscall, and
// escape-grammar decision lives in `supra_ffi`, and this crate composes.
// Tests assert with `.expect()`; scoped to `cfg(test)` so no allow reaches a
// shipped path.
#![cfg_attr(test, allow(clippy::expect_used, clippy::panic, clippy::print_stderr))]

pub mod error;
pub mod session;
pub mod shaping;

pub use error::ShellError;
pub use session::{ShellSession, spawn_simple};
pub use shaping::{Shaped, Shaper};

/// Sessions move between the turn loop and the TUI reader thread, so
/// `Send + Sync` is a requirement rather than an observation.
const _: () = {
    const fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<ShellSession>();
    assert_send_sync::<Shaper>();
};
