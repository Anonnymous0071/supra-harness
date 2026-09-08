//! The wasmtime host: WIT-world plugins with ToolClass-derived import
//! sets.
//!
//! **T20** of the stage sequence: *wasmtime host, WIT ABI, verified import
//! set*. Two sentences from the architecture carry the whole stage:
//!
//! - §3: "capability isolation by absence: an agent has no import through
//!   which to spawn."
//! - §6: `ToolClass` is enforced by "absence of the WASM capability".
//!
//! Isolation here is not a check that refuses after the fact - it is a
//! namespace that simply has nothing in it. The host builds one linker per
//! class, each holding exactly the class's imports, so a guest has no name
//! to call anything outside its class through.
//!
//! # The world
//!
//! [`world::WORLD`] is the WIT text, one source for the ABI: three worlds
//! (`agent`, `host`, `user`) ascending the same ladder as `ToolClass`,
//! each exporting `run(string) -> string` and importing its class's
//! interfaces. The text is stated in `world.rs`'s docs and shipped as
//! `supra.wit` - one source, two consumers only works when there is one
//! source, and a test pins the agreement.
//!
//! # The lifecycle
//!
//! [`Host::new`] builds the engine (fuel on) and the three linkers;
//! [`Host::verify`] type-checks a component's declared imports against
//! its class **without instantiating it**; [`Host::instantiate`] links,
//! checks the `run` export's signature, and hands back a [`Plugin`] whose
//! store carries the fuel budget. [`Plugin::call`] runs the export;
//! exhaustion is [`PluginError::FuelExhausted`], not a hang.
//!
//! # Usage
//!
//! ```no_run
//! use supra_plugin::Host;
//!
//! let host = Host::new().expect("engine");
//! // `component` arrives as bytes: compiled off-host or cached.
//! # Ok::<(), supra_plugin::PluginError>(())
//! ```

#![deny(missing_docs)]
// Tests assert with `.expect()` and `panic!`. Scoped to `cfg(test)` so no
// allow reaches a shipped path.
#![cfg_attr(test, allow(clippy::expect_used, clippy::panic, clippy::print_stderr))]

pub mod error;
pub mod host;
pub mod world;

pub use error::PluginError;
pub use host::{DEFAULT_FUEL, Host, HostState, Plugin};
pub use world::{WORLD, imports_for};

/// The host rides the turn loop (one instantiation per peer claim), so
/// `Send` is a requirement rather than an observation. `Sync` is not
/// required: a `Plugin`'s store is single-owner by wasmtime's own rules.
const _: () = {
    const fn assert_send<T: Send>() {}
    assert_send::<Host>();
    assert_send::<Plugin>();
};
