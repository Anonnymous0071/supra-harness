//! The terminal surface. **T29**: status line, gauge, thinking display,
//! viewport, spinner, panels.
//!
//! Everything here is pure rendering: bytes in, strings out, no terminal
//! I/O. T30 owns the raw-mode terminal and the event pump; this crate
//! owns what the screen shows. Every colour goes through the theme
//! (T28.5 tokens), every gauge goes through `gauge_for` - never an
//! embedded escape sequence, never a fixed cell cost.
//!
//! The status line sheds by priority as the terminal narrows, but four
//! segments never shed: context %, cache %, session spend, and the
//! cache-break marker. An invisible cost leak is the failure this
//! design exists to prevent.
//!
//! The thinking display is read-only: `∵ Thinking…` while streaming,
//! `∴ Thought for Ns (ctrl+o to collapse)` after - no cost preview, the
//! tokens are billed either way.

#![deny(missing_docs)]
#![cfg_attr(test, allow(clippy::expect_used, clippy::panic, clippy::print_stderr))]

/// The gauge meter.
pub mod meter;
/// Bordered panels.
pub mod panel;
/// The spinner.
pub mod spinner;
/// The status line and its priority shed.
pub mod status;
/// The thinking display.
pub mod thinking;
/// The scroll viewport.
pub mod viewport;

pub use meter::Meter;
pub use panel::Panel;
pub use spinner::{SPINNER_FRAMES, Spinner};
pub use status::{Cost, StatusLine, StatusSegment};
pub use thinking::{ThinkingDisplay, ThinkingState};
pub use viewport::Viewport;

const _: () = {
    const fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<StatusLine>();
    assert_send_sync::<Meter>();
    assert_send_sync::<ThinkingDisplay>();
    assert_send_sync::<Viewport>();
    assert_send_sync::<Spinner>();
    assert_send_sync::<Panel>();
};
