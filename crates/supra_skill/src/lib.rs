//! Skill loading, dependency resolution, and hot reload.
//!
//! **T19** of the stage sequence: *skill loading, dependency resolution,
//! hot reload*. A skill is a directory with a `SKILL.md` - front matter
//! (`name`, `description`, optional `requires`) plus a Markdown body.
//!
//! Three bindings shape the crate:
//!
//! - **I2** - the body is content, never a `system` mutation. The prompt
//!   carries name + description (what the model reads to decide whether
//!   to open the body); the body rides as content on demand. This is the
//!   token-efficiency contract every skill system the user studied
//!   converged on, kept as the loader's default shape.
//! - **I3** - a skill that contributes tools appends through T17's
//!   registry; a hot reload never *removes* what a session already
//!   advertised. This crate loads and resolves; registration is T23's,
//!   on the same append-only terms MCP (T18) registers.
//! - **The T15 watcher shape** - hot reload is `apply_event(&notify::Event)`
//!   the turn loop forwards, not a background thread with its own
//!   timing. A failed reload refuses and the previous state stands: the
//!   author's broken file is the author's error, not the session's.
//!
//! # Parsing by hand, deliberately
//!
//! The front matter is a `key: value` line grammar - small enough to own.
//! A YAML dependency would carry its version pins and its grammar
//! ambiguities into a file the user edits by hand; the loader refuses
//! what it cannot parse with the offending line quoted, and preserves
//! unknown keys verbatim rather than refusing them (the author's
//! metadata is the author's business).
//!
//! # Determinism
//!
//! Load order, dependency order, and the listing order are all
//! deterministic for a given set of files: paths sort, the topological
//! walk keeps name order among unrelated skills, and `all()` iterates
//! the name map. Same skills, same bytes, every session - the prefix
//! property, applied to skill listings.
//!
//! # Usage
//!
//! ```no_run
//! use supra_skill::Skills;
//!
//! let mut skills = Skills::load("skills").expect("load");
//! for skill in skills.topological() {
//!     // dependencies first; name + description are the prompt listing,
//!     // the body is content the model opens on demand
//! #     let _ = skill;
//! }
//! ```
#![deny(missing_docs)]
// Tests assert with `.expect()` and `panic!`. Scoped to `cfg(test)` so no
// allow reaches a shipped path.
#![cfg_attr(test, allow(clippy::expect_used, clippy::panic, clippy::print_stderr))]

pub mod error;
pub mod loader;
pub mod skill;

pub use error::SkillError;
pub use loader::Skills;
pub use skill::Skill;

/// The set rides the turn loop behind the watcher's event forwarding, so
/// `Send + Sync` is a requirement rather than an observation.
const _: () = {
    const fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<Skills>();
    assert_send_sync::<Skill>();
};
