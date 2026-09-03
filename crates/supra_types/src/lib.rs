//! Core contracts for supra-harness.
//!
//! **T6** of the stage sequence: the types every later stage holds, and the
//! invariants that are enforced by those types rather than by convention.
//!
//! # What lives here
//!
//! | Module | Owns |
//! | ------ | ---- |
//! | [`id`] | ULID-backed identifiers; `ClaimId` and `FindingId` are distinct on purpose |
//! | [`hash`] | content hashing over a length-prefixed canonical encoding, deliberately separate from the wire format |
//! | [`sealed`] | invariant I1: [`Sealed`] has no mutation path, and a tampered digest is refused on load |
//! | [`segment`] | the ledger's units: four kinds matching the four breakpoint regions, tool input held as canonical text |
//! | [`ephemeral`] | invariant I2: `Sealed<EphemeralBlock>` is unnameable, so volatile state cannot become perpetual |
//! | [`cache`] | invariants I5/I7/I8 as verified arithmetic: breakpoints, TTLs, the ten-times gradient, minimum lengths |
//! | [`lineage`] | the structural half of the anti-self-spawn guard: depth 1, no cycles, revalidated on load |
//! | [`permission`] | the two axes kept apart: [`ToolClass`] is never relaxable, and the mode matrix is executable |
//! | [`cohort`] | peer cohorts: rational quorum, byzantine tolerance, the verdict budget, incremental tallies |
//! | [`money`] | micro-dollars in integers, because a float is the one primitive whose text form is not byte-stable |
//! | [`event`] | the taxonomy the bus filters and the TUI renders |
//!
//! # Why no I/O
//!
//! This crate is the contract layer, so it depends on nothing that performs I/O -
//! no runtime, no store handle, no provider client. Every stage can hold these types
//! without inheriting those things, which keeps the dependency map in
//! `docs/ARCHITECTURE.md` honest: T15.5's cohort estimation declares no dependency
//! on `supra_llm`, and a negative test there enforces it, and that is only possible
//! because the shared vocabulary they both need sits below both of them.
//!
//! # Why there are no floats
//!
//! [`MicroUsd`] is integers, [`Confidence`] is an enum, and the cache multipliers are
//! integer percentages. Invariant I7 requires byte-stable serialisation, and a
//! float's text form is the one primitive whose stability is not obvious. Quorum is
//! likewise computed as an integer rational, because the float spelling is wrong at
//! k=3. Removing floats here removes the question from every stage above.
//!
//! # How the invariants are enforced
//!
//! By types and by CI, not by convention. [`Sealed`] simply has no mutable accessor;
//! its bound on `T` makes sealing an ephemeral block a type error; deserialisation
//! revalidates rather than trusting what was stored. What absence cannot prove is
//! checked structurally by `scripts/check-invariants.sh`, wired into `just lint`,
//! which fails the build if a mutable accessor appears on `Sealed`, a `Sealable`
//! impl appears on `EphemeralBlock`, or a `Mode` parameter appears on `ToolClass`.

#![deny(missing_docs)]
#![forbid(unsafe_code)]
// Tests assert invariants with `.expect()`, `.expect_err()`, and `panic!` - that is
// how a failing test reports itself. The workspace bans those to keep a
// long-running agent process alive, which is not a property tests have. Scoped to
// `cfg(test)` so no allow reaches a shipped code path.
#![cfg_attr(test, allow(clippy::expect_used, clippy::panic))]

pub mod cache;
pub mod cohort;
pub mod ephemeral;
pub mod event;
pub mod hash;
pub mod id;
pub mod lineage;
pub mod money;
pub mod permission;
pub mod sealed;
pub mod segment;

pub use cache::{
    Breakpoint, CacheTtl, LOOKBACK_BLOCKS, MAX_BREAKPOINTS, MinimumCacheable, READ_MULTIPLIER_PERCENT,
    UNCACHED_MULTIPLIER_PERCENT, padding_needed, ttl_order_is_valid,
};
pub use cohort::{
    Confidence, DEFAULT_PEER_LIMIT, MAX_REQUESTS_PER_MINUTE, PEER_CEILING, QuorumTally, TallyError, Tier,
    Verdict, VerdictError, Vote, byzantine_tolerance, quorum, shards_needed,
};
pub use ephemeral::{EphemeralBlock, EphemeralKey};
pub use event::{CacheBreakCause, Event, Topic};
pub use hash::{CanonicalWriter, ContentHash, ParseHashError};
pub use id::{AgentId, ClaimId, FindingId, ParseIdError, SegmentId, SessionId, TurnId};
pub use lineage::{Lineage, LineageError};
pub use money::MicroUsd;
pub use permission::{
    Decision, Invoker, Mode, Reversibility, Rule, RuleEffect, RuleSource, ToolClass, decide, resolve,
};
pub use sealed::{Sealable, Sealed, SeqNo};
pub use segment::{Block, CanonicalJson, MemoryIndexEntry, Role, Segment, SegmentError, SegmentKind};

#[cfg(test)]
mod tests {
    use super::*;

    /// The invariants this crate enforces, as one test each, named for the
    /// invariant. If a later refactor breaks one, the failure message names the
    /// section of the architecture document it contradicts.
    #[test]
    fn i1_a_sealed_value_has_no_mutation_path() {
        // Structural absence cannot be asserted directly; what can be asserted is
        // that the value is unchanged after any read path, and that the only way to
        // get a `Sealed` with a given hash is to seal those contents.
        let body = Block::Text("immutable".to_owned());
        let sealed = Sealed::seal(SeqNo::from_raw(1), body);

        // Read paths: deref, get, as_ref, hash, seq.
        let read: &Block = &sealed;
        let via_get = sealed.get();
        let via_as_ref: &Block = sealed.as_ref();

        assert_eq!(read, via_get);
        assert_eq!(via_get, via_as_ref);
        assert_eq!(sealed.hash(), ContentHash::of(&Block::Text("immutable".to_owned())));
        assert!(sealed.verify());
    }

    #[test]
    fn i2_an_ephemeral_block_is_not_sealable() {
        // `Sealed<EphemeralBlock>` does not compile, because `Sealed` carries the
        // `Sealable` bound on its type definition. The next best thing to a negative
        // compile test: the type exists, is freely mutable, and can be rendered -
        // and `Sealed`'s bound is checked structurally by check-invariants.sh.
        let mut block = EphemeralBlock::new();
        block.set(EphemeralKey::ContextPercent, "42");
        assert_eq!(block.render(), "context: 42\n");
    }

    #[test]
    fn i5_the_four_breakpoints_are_declared_in_prefix_order() {
        // Longer TTLs must precede shorter ones, and the declared order is the
        // order the provider requires.
        assert!(ttl_order_is_valid(&Breakpoint::ALL));
        assert_eq!(Breakpoint::ALL.len(), MAX_BREAKPOINTS);
    }

    #[test]
    fn i8_padding_closes_a_silent_failure() {
        // Below the minimum, caching fails with no error. The contract is the
        // function that grows the prefix instead.
        assert_eq!(padding_needed(400, MinimumCacheable::Tokens512), 112);
    }

    #[test]
    fn quorum_at_three_is_two_not_three() {
        // The float spelling demands unanimity at k=3, which would make every
        // dissent an escalation. See cohort::tests for the exhaustive walk.
        assert_eq!(quorum(3), 2);
        assert_eq!(byzantine_tolerance(3), 0);
    }
}
