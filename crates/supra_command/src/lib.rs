//! The command registry and palette. **T28.7** of the stage sequence.
//!
//! Commands are descriptions plus an authority class; the runtime owns
//! the action, so the registry never holds a closure over harness
//! state. The registry resolves slash input against the same authority
//! axis the permission gate uses - a command is subject to the gate,
//! not exempt from it, and `yolo` lifts consent, never authority.
//!
//! Two prohibitions live in code and are tested, not remembered: there
//! are no `cost`/`cache`/`context` commands (section 7 - transparency
//! that must be requested is never consulted when it matters, so those
//! surfaces live in the status line, T29), and there is no `/think`
//! (the thinking budget is frozen per session, section 6).
//!
//! ```
//! use supra_command::Registry;
//! use supra_types::{Invoker, Mode};
//!
//! let registry = Registry::with_builtins();
//! let help = registry.resolve("help", Invoker::Host, Mode::Auto).expect("known");
//! assert_eq!(help.name, "help");
//!
//! assert!(registry.search("mo").len() >= 2, "mode and model");
//! ```

#![deny(missing_docs)]
#![cfg_attr(test, allow(clippy::expect_used, clippy::panic, clippy::print_stderr))]

/// The command type and the built-in set.
pub mod command;
/// The registry and the fuzzy search.
pub mod registry;

pub use command::{Command, FORBIDDEN, builtins};
pub use registry::{CommandError, Registry};

const _: () = {
    const fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<Command>();
    assert_send_sync::<Registry>();
};
