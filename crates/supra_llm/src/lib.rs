//! Provider clients for supra-harness.
//!
//! **T13** of the stage sequence: three providers behind one call shape, each with its
//! own [`CachePolicy`], one canonical serialiser, and a thinking
//! budget frozen at startup.
//!
//! # What this stage is for
//!
//! Everything T14 needs to render a request: the provider's cache behaviour (how many
//! breakpoints, which TTLs, what minimum length), the canonical bytes that keep the
//! prefix stable, and the thinking budget the prompt is rendered against.
//!
//! # What this stage is not
//!
//! Not the turn loop (T23), not the prompt ledger (T14), not the cohort choreography
//! (I6). This crate renders one request and reads one response. Retry policy, fan-out,
//! and warm-up sequencing belong to the caller that knows how many peers are waiting.
//!
//! # The three providers
//!
//! One client speaks to three backends: the first with four explicit breakpoints,
//! the second with one cache key, the third with implicit caching and a conservative
//! policy. Section 11 records the third backend's behaviour as unverified; its policy
//! encodes that honestly rather than guessing the favourable end of the range.
//!
//! # Usage
//!
//! ```no_run
//! use supra_llm::{CachePolicy, ProviderKind, canonicalize};
//!
//! let policy = CachePolicy::for_kind(ProviderKind::Anthropic);
//! assert!(policy.thinking_allowed(2048));
//!
//! let args = canonicalize(r#"{"b":1,"a":2}"#).expect("valid");
//! assert_eq!(args.as_str(), r#"{"a":2,"b":1}"#);
//! # Ok::<(), supra_llm::CanonicalError>(())
//! ```

#![deny(missing_docs)]
#![forbid(unsafe_code)]
// Tests assert with `.expect()` and `panic!`. Scoped to `cfg(test)` so no allow reaches a
// shipped path.
#![cfg_attr(test, allow(clippy::expect_used, clippy::panic, clippy::print_stderr))]

pub mod canonical;
pub mod client;
pub mod error;
pub mod policy;
pub mod providers;

pub use canonical::{CanonicalError, canonicalize, canonicalize_value, parse_strict};
pub use client::{Client, Completion, ContentBlock, Message, Request, Role, StopReason, Thinking, Usage};
pub use error::LlmError;
pub use policy::{CachePolicy, ProviderKind};
