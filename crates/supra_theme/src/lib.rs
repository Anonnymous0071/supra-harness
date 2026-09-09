//! Semantic tokens, per-glyph width probing, and a responsive banner.
//! **T28.5** of the stage sequence.
//!
//! The TUI's palette, measured. Three pieces:
//!
//! - [`theme`]: eight semantic tokens. The TUI asks for roles - `Error`,
//!   `Muted`, `Accent` - and the theme answers with bytes; no component
//!   embeds an escape sequence. A theme also carries its East Asian
//!   Ambiguous resolution, because a theme is a locale decision as much
//!   as a colour decision.
//! - [`gauge`]: per-glyph width probing. The T2 finding binds here: the
//!   Block Elements range is *not* one width class - U+2588 is
//!   Ambiguous while U+2591 is Neutral - so the obvious `█`/`░` pairing
//!   mixes classes and a gauge silently changes length under a CJK
//!   locale. `gauge_for` picks the block pair when it is stable and a
//!   same-class portable pair otherwise.
//! - [`banner`]: a responsive wordmark, every line measured against the
//!   terminal it was asked for.
//!
//! ```
//! use supra_ffi::width::Ambiguous;
//! use supra_theme::{Token, Theme, gauge_for};
//!
//! let theme = Theme::default_dark();
//! let error = theme.paint(Token::Error, "boom");
//! assert!(error.contains("boom"));
//!
//! let gauge = gauge_for(Ambiguous::Wide);
//! assert_eq!(gauge.render(2, 1), "##.");
//! ```

#![deny(missing_docs)]
#![cfg_attr(test, allow(clippy::expect_used, clippy::panic, clippy::print_stderr))]

/// The responsive banner.
pub mod banner;
/// Per-glyph width probing and gauge selection.
pub mod gauge;
/// Semantic tokens and themes.
pub mod theme;

pub use banner::banner;
pub use gauge::{GaugeGlyphs, gauge_for};
pub use theme::{Theme, Token};

const _: () = {
    const fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<Theme>();
    assert_send_sync::<GaugeGlyphs>();
};
