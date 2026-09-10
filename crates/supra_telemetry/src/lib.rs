//! Anonymous, opt-in usage telemetry. **T28** of the stage sequence.
//!
//! What ships and what does not, in one sentence each: a report carries
//! a tier, three counters, and a random per-report id - and no session
//! id, no file paths, no environment, no prompt bytes. Consent lives in
//! one marker file (`on` or `off`, absent is off) and the default is
//! `Off`: telemetry that ships enabled is telemetry that was never
//! asked for.
//!
//! The sink is a function the runtime supplies. This crate never opens
//! a socket - the report is bytes, and where bytes go is a decision
//! that belongs to the configuration layer (T30) and the operator, not
//! to a library.
//!
//! ```
//! use supra_telemetry::{Consent, Counters};
//!
//! assert_eq!(Consent::default(), Consent::Off);
//!
//! let mut counters = Counters::default();
//! counters.turn();
//! let report = counters.report(supra_types::Tier::E1);
//! assert_eq!(report.turns(), 1);
//! ```

#![deny(missing_docs)]
#![cfg_attr(test, allow(clippy::expect_used, clippy::panic, clippy::print_stderr))]

/// The consent marker.
pub mod consent;
/// The report and its counters.
pub mod report;

pub use consent::Consent;
pub use report::{Counters, Report};

const _: () = {
    const fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<Report>();
    assert_send_sync::<Counters>();
    assert_send_sync::<Consent>();
};
