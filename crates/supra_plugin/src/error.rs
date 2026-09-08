//! What the plugin host refused, and who must act.
//!
//! The host sits between the harness and third-party components, so every
//! refusal names the side that produced it: the author whose component
//! asked for too much, or the operator whose configuration bounds it.
//!
//! | Variant | Shape | Who acts |
//! | ------- | ----- | -------- |
//! | `Engine` | wasmtime itself refused | the operator |
//! | `UnlistedImport` | the component imports something outside its class set | the author |
//! | `MissingHostImport` | the host offers no function the class set needs | the operator (a host bug) |
//! | `WrongSignature` | a component import's type is not the declared one | the author |
//! | `FuelExhausted` | the guest burnt its fuel | the author (or the budget) |
//! | `Trap` | the guest trapped | the author |

use thiserror::Error;

/// A plugin-host refusal.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum PluginError {
    /// wasmtime refused: the engine would not build, the bytes are not a
    /// component, instantiation failed for a platform reason. The detail
    /// is wasmtime's own, unedited - it names the step.
    #[error("wasmtime refused: {0}")]
    Engine(#[from] wasmtime::Error),

    /// The component imports something outside its class set.
    ///
    /// Isolation in this harness is by absence (the T20 sentence): an
    /// Agent-class plugin whose imports include a host-only or
    /// user-only function is refused *before instantiation*, with the
    /// import's path named. The author either lowers the function's
    /// class or removes the import.
    #[error("component {component:?} imports {import:?}, outside its class set")]
    UnlistedImport {
        /// The component, by its configured name.
        component: String,
        /// The import path, as the component declares it.
        import: String,
    },

    /// A host function the class set needs is not registered on the host.
    ///
    /// This is a harness bug, not an author bug: the author asked for
    /// nothing outside their class, and the host promised the class's
    /// imports. The path names which host function is missing.
    #[error("the host offers no function {import:?} that the class set needs")]
    MissingHostImport {
        /// The import path.
        import: String,
    },

    /// A component import's type is not the declared one.
    ///
    /// Refused at `get_typed_func` time with both arities named: a
    /// component that declares `run(string) -> string` and exports
    /// `run() -> string` has a broken contract the call would otherwise
    /// discover mid-turn.
    #[error("import {import:?} has the wrong signature: {detail}")]
    WrongSignature {
        /// The import path.
        import: String,
        /// What differs.
        detail: String,
    },

    /// The guest burnt its whole fuel budget: the call stopped where it
    /// was, no partial result is returned, and the component is unusable
    /// for this turn (its store keeps the consumed state).
    #[error("component {component:?} exhausted its fuel budget of {budget} units")]
    FuelExhausted {
        /// The component, by its configured name.
        component: String,
        /// The budget it was given.
        budget: u64,
    },

    /// The guest trapped. The detail is the trap's own message - it names
    /// the fault, and the author owns it.
    #[error("component {component:?} trapped: {detail}")]
    Trap {
        /// The component, by its configured name.
        component: String,
        /// The trap message.
        detail: String,
    },
}
