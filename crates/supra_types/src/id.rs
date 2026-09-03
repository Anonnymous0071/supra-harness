//! Identifiers.
//!
//! Every entity that can be referenced across a session boundary gets a ULID:
//! 128 bits, lexicographically sortable, millisecond timestamp in the high bits.
//!
//! # Why a distinct type per kind
//!
//! `SessionId`, `TurnId`, `AgentId`, `ClaimId`, and `SegmentId` are separate
//! types over the same representation. Passing a `TurnId` where an `AgentId` is
//! expected is a real bug class in an event-driven system with forty event
//! variants, and the compiler rejects it for free.
//!
//! # Why generation is not monotonic
//!
//! The `ulid` crate offers a monotonic `Generator`, which guarantees strictly
//! increasing values within a single millisecond. This module deliberately uses
//! the non-monotonic `Ulid::generate()` instead, for two reasons.
//!
//! The first is mechanical: a monotonic generator carries mutable state, so k
//! peers minting claim ids concurrently would contend on one lock on the turn
//! path, and the budget for cohort spawn at k=80 is under a second.
//!
//! The second is a property of the peer model. Within one millisecond, ULIDs
//! order by their random bits, so the turn loop's ULID tiebreak does **not**
//! privilege whichever peer the scheduler happened to run first. A monotonic
//! generator would make the tiebreak a proxy for scheduling order, which is a
//! quiet form of hierarchy in a system whose whole premise is that no peer is
//! privileged. The order is still total and still stable once minted, which is
//! all a tiebreak requires.
//!
//! Across milliseconds the timestamp prefix dominates, so ids remain sortable by
//! creation time at the granularity that matters for storage and logs.

use core::fmt;
use core::str::FromStr;

use serde::{Deserialize, Serialize};

/// Why a string could not be read as an identifier.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("not a valid ULID: {0}")]
pub struct ParseIdError(String);

/// Declare a ULID-backed identifier.
///
/// The serialised form is the 26-character Crockford base32 encoding rather than
/// a 128-bit integer, so a row in SQLite or a line in a log is readable and
/// greppable without a decoder.
macro_rules! declare_id {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        #[serde(into = "String", try_from = "String")]
        pub struct $name(ulid::Ulid);

        impl $name {
            /// Mint a new identifier from the current time and 80 random bits.
            #[must_use]
            pub fn generate() -> Self {
                Self(ulid::Ulid::generate())
            }

            /// The all-zero identifier.
            ///
            /// Sorts before every generated value, which makes it usable as the
            /// lower bound of a range scan. It is not a valid entity id.
            #[must_use]
            pub const fn nil() -> Self {
                Self(ulid::Ulid::nil())
            }

            /// Whether this is [`Self::nil`].
            #[must_use]
            pub const fn is_nil(&self) -> bool {
                self.0.is_nil()
            }

            /// Milliseconds since the Unix epoch, from the timestamp prefix.
            #[must_use]
            pub const fn timestamp_ms(&self) -> u64 {
                self.0.timestamp_ms()
            }

            /// The raw 128-bit value, for storage layers that want a fixed-width
            /// column.
            #[must_use]
            pub const fn to_u128(&self) -> u128 {
                self.0.0
            }

            /// Rebuild from a raw 128-bit value.
            #[must_use]
            pub const fn from_u128(raw: u128) -> Self {
                Self(ulid::Ulid(raw))
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                let mut buffer = [0_u8; ulid::ULID_LEN];
                f.write_str(self.0.array_to_str(&mut buffer))
            }
        }

        impl fmt::Debug for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                // The type name plus the encoded value, so a Debug dump of an
                // event says which kind of id it is holding.
                write!(f, "{}({})", stringify!($name), self)
            }
        }

        impl FromStr for $name {
            type Err = ParseIdError;

            fn from_str(text: &str) -> Result<Self, Self::Err> {
                ulid::Ulid::from_string(text)
                    .map(Self)
                    .map_err(|_| ParseIdError(text.to_owned()))
            }
        }

        impl TryFrom<String> for $name {
            type Error = ParseIdError;

            fn try_from(text: String) -> Result<Self, Self::Error> {
                text.parse()
            }
        }

        impl From<$name> for String {
            fn from(id: $name) -> Self {
                id.to_string()
            }
        }
    };
}

declare_id! {
    /// One `supra` session: a process lifetime, a prompt ledger, and a store.
    SessionId
}

declare_id! {
    /// One turn: a user prompt through to a completed response.
    TurnId
}

declare_id! {
    /// One peer. The host process holds one of these too, as lineage root.
    AgentId
}

declare_id! {
    /// One claim published to the blackboard, and the unit a quorum votes on.
    ClaimId
}

declare_id! {
    /// One sealed prompt segment.
    SegmentId
}

declare_id! {
    /// One finding raised by a deterministic gate or by a peer review.
    ///
    /// Distinct from [`ClaimId`] on purpose: a claim is something a peer proposes
    /// and a cohort votes on, while a finding is something a gate or a reviewer
    /// asserts and which escalates the tier when confirmed. Conflating them would
    /// let a finding be voted away.
    FindingId
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_through_its_string_form() {
        let id = TurnId::generate();
        let text = id.to_string();
        assert_eq!(text.len(), 26, "Crockford base32 is fixed width");
        assert_eq!(text.parse::<TurnId>(), Ok(id));
    }

    #[test]
    fn round_trips_through_its_integer_form() {
        let id = SegmentId::generate();
        assert_eq!(SegmentId::from_u128(id.to_u128()), id);
    }

    #[test]
    fn rejects_malformed_text() {
        assert!("".parse::<AgentId>().is_err());
        assert!("too-short".parse::<AgentId>().is_err());
        // 'I', 'L', 'O', and 'U' are excluded from the Crockford alphabet.
        assert!("IIIIIIIIIIIIIIIIIIIIIIIIII".parse::<AgentId>().is_err());
    }

    #[test]
    fn ordering_is_total_and_stable() {
        // The property the turn loop's tiebreak rests on: any two ids compare,
        // and the answer does not change between comparisons.
        let ids: Vec<ClaimId> = (0..64).map(|_| ClaimId::generate()).collect();
        for left in &ids {
            for right in &ids {
                let first = left.cmp(right);
                assert_eq!(first, left.cmp(right), "comparison must be stable");
                assert_eq!(first.reverse(), right.cmp(left), "and antisymmetric");
            }
        }
    }

    #[test]
    fn later_milliseconds_sort_later() {
        // Across milliseconds the timestamp prefix dominates, which is what makes
        // an id range scan in T10 meaningful.
        let early = TurnId::from_u128(u128::from(1_700_000_000_000_u64) << 80);
        let late = TurnId::from_u128(u128::from(1_700_000_000_001_u64) << 80);
        assert!(early < late);
        assert_eq!(early.timestamp_ms(), 1_700_000_000_000);
    }

    #[test]
    fn nil_sorts_before_everything_generated() {
        let nil = SessionId::nil();
        assert!(nil.is_nil());
        assert!(nil < SessionId::generate());
        assert!(!SessionId::generate().is_nil());
    }

    #[test]
    fn distinct_kinds_are_distinct_types() {
        // Documented here because it is the reason five near-identical types
        // exist. If these were one type, this file would compile with the two
        // values swapped and the mistake would surface as a lookup miss.
        let turn = TurnId::generate();
        let agent = AgentId::from_u128(turn.to_u128());
        assert_eq!(turn.to_string(), agent.to_string(), "same bits");
        // ...and yet `turn == agent` does not compile, which is the point.
    }
}
