//! Invariant I1: the append-only prompt ledger, enforced by types.
//!
//! > The prompt is never rewritten, only appended to. `Sealed<Segment>` has no
//! > `&mut` accessor, and `PromptLedger` (T14) exposes only
//! > `append(Sealed<Segment>)`. Every segment carries a sequence number and a
//! > content hash.
//!
//! # What is deliberately absent
//!
//! [`Sealed`] has no `DerefMut`, no `as_mut`, no `get_mut`, no public field, and
//! **no `into_inner`**. The last one is the least obvious and the most important:
//! if the value could be moved out, it could be edited and re-sealed at the same
//! sequence number, which is a rewrite wearing an append's clothes. Every reader
//! that legitimately needs the contents - the T13 wire serialiser, the T10
//! persistence layer, the T29 renderer - needs only `&T`, which [`Sealed::get`]
//! and `Deref` provide.
//!
//! Absence cannot be asserted at runtime, so `scripts/check-invariants.sh` fails
//! the build if a mutable accessor appears in this module. That is the CI half of
//! "enforced by types and by CI, not by convention".
//!
//! # Why the bound sits on the struct
//!
//! `Sealed<T: Sealable>` carries its bound on the type definition rather than only
//! on its impls. Bounds on structs are usually discouraged because they propagate,
//! but propagation is the mechanism here: it makes `Sealed<EphemeralBlock>`
//! unnameable rather than merely unconstructible, so invariant I2 is a type error
//! at the point someone writes the type, not a runtime refusal later.

use core::fmt;
use core::ops::Deref;

use serde::{Deserialize, Deserializer, Serialize, de};

use crate::hash::{CanonicalWriter, ContentHash};

/// A value with a fixed, hashable canonical encoding.
///
/// Implementing this is a statement that the value is **eligible to enter the
/// prompt prefix**. Volatile per-turn state must not implement it; see
/// [`crate::EphemeralBlock`].
pub trait Sealable {
    /// Domain separator for this type's canonical encoding.
    ///
    /// Written before any field, so two types whose fields happen to line up
    /// cannot produce the same digest. Values must be unique across the crate;
    /// `0xF0` upward is reserved for tests.
    const CANONICAL_KIND: u8;

    /// Append this value's canonical encoding.
    ///
    /// Must be deterministic and must not depend on anything outside the value -
    /// no clock, no environment, no iteration order of a hash map. A hash that
    /// varies across processes silently disables the prompt cache, which is the
    /// exact failure this crate exists to prevent.
    fn write_canonical(&self, writer: &mut CanonicalWriter);
}

/// Position in the ledger.
///
/// Monotonic and gap-free. The T14 ledger owns assignment; this type only knows
/// how to be the next one.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SeqNo(u64);

impl SeqNo {
    /// The first position.
    pub const ZERO: Self = Self(0);

    /// The position after this one.
    ///
    /// Arithmetic overflow would need 2^64 seals in one session. It is not
    /// guarded, and it is not silent either: `overflow-checks` is enabled in every
    /// build profile including `release`, so a wrap aborts rather than quietly
    /// reusing a position - which is the one outcome that would break I1.
    #[must_use]
    pub const fn next(self) -> Self {
        Self(self.0 + 1)
    }

    /// The underlying counter.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }

    /// Build from a stored counter.
    #[must_use]
    pub const fn from_raw(raw: u64) -> Self {
        Self(raw)
    }
}

impl fmt::Display for SeqNo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "#{}", self.0)
    }
}

/// An immutable, position-stamped, content-hashed ledger entry.
///
/// Construction is the only point at which the contents are chosen. After that the
/// value is readable and nothing else.
#[derive(Clone, PartialEq, Eq, Serialize)]
pub struct Sealed<T: Sealable> {
    seq: SeqNo,
    hash: ContentHash,
    value: T,
}

impl<T: Sealable> Sealed<T> {
    /// Seal `value` at `seq`.
    ///
    /// The hash covers the value's kind and contents but **not** `seq`, so it
    /// stays content-addressed and usable as a store key. Ordering is hashed by
    /// whoever owns ordering: the T14 ledger digests the sequence of
    /// `(SeqNo, ContentHash)` pairs to produce its prefix hash.
    #[must_use]
    pub fn seal(seq: SeqNo, value: T) -> Self {
        let hash = ContentHash::of(&value);
        Self { seq, hash, value }
    }

    /// Position in the ledger.
    #[must_use]
    pub const fn seq(&self) -> SeqNo {
        self.seq
    }

    /// Digest of the contents.
    #[must_use]
    pub const fn hash(&self) -> ContentHash {
        self.hash
    }

    /// The contents, by shared reference.
    #[must_use]
    pub const fn get(&self) -> &T {
        &self.value
    }

    /// Recompute the digest and compare it with the stored one.
    ///
    /// Always true for a value sealed in this process. It exists for values that
    /// crossed a trust boundary - a row read back from SQLite, a session file
    /// edited by hand - and is applied automatically on deserialisation.
    #[must_use]
    pub fn verify(&self) -> bool {
        ContentHash::of(&self.value) == self.hash
    }
}

impl<T: Sealable> Deref for Sealed<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.value
    }
}

impl<T: Sealable> AsRef<T> for Sealed<T> {
    fn as_ref(&self) -> &T {
        &self.value
    }
}

impl<T: Sealable + fmt::Debug> fmt::Debug for Sealed<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Sealed")
            .field("seq", &self.seq)
            .field("hash", &self.hash)
            .field("value", &self.value)
            .finish()
    }
}

/// The on-disk shape, used only as a deserialisation staging area.
#[derive(Deserialize)]
struct SealedRepr<T> {
    seq: SeqNo,
    hash: ContentHash,
    value: T,
}

impl<'de, T> Deserialize<'de> for Sealed<T>
where
    T: Sealable + Deserialize<'de>,
{
    /// Reads a sealed value and **rejects a digest that does not match**.
    ///
    /// Without this, deserialisation would be a hole straight through I1: a
    /// tampered or corrupted store row would yield a `Sealed` whose recorded hash
    /// disagrees with its contents, and T14's prefix comparison would then be
    /// verifying one lie against another. Verifying here means a corrupted session
    /// fails to load with a clear error instead of resuming into a state whose
    /// cache accounting is wrong.
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let repr = SealedRepr::<T>::deserialize(deserializer)?;
        let computed = ContentHash::of(&repr.value);
        if computed != repr.hash {
            return Err(de::Error::custom(format!(
                "sealed value at {} has hash {} but its contents hash to {}",
                repr.seq,
                repr.hash.short(),
                computed.short()
            )));
        }
        Ok(Self { seq: repr.seq, hash: repr.hash, value: repr.value })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
    struct Body {
        text: String,
    }

    impl Sealable for Body {
        const CANONICAL_KIND: u8 = 0xF2;

        fn write_canonical(&self, writer: &mut CanonicalWriter) {
            writer.str(1, &self.text);
        }
    }

    fn body(text: &str) -> Body {
        Body { text: text.to_owned() }
    }

    #[test]
    fn sealing_records_position_and_content() {
        let sealed = Sealed::seal(SeqNo::from_raw(7), body("hello"));
        assert_eq!(sealed.seq(), SeqNo::from_raw(7));
        assert_eq!(sealed.get().text, "hello");
        assert_eq!(sealed.hash(), ContentHash::of(&body("hello")));
        assert!(sealed.verify());
    }

    #[test]
    fn the_hash_is_content_addressed_not_position_addressed() {
        // Deliberate: the digest is a content-addressed store key, so the same
        // content at two positions shares one key.
        let first = Sealed::seal(SeqNo::ZERO, body("same"));
        let second = Sealed::seal(SeqNo::from_raw(99), body("same"));
        assert_eq!(first.hash(), second.hash());
        assert_ne!(first.seq(), second.seq());
    }

    #[test]
    fn different_content_gets_a_different_hash() {
        let left = Sealed::seal(SeqNo::ZERO, body("a"));
        let right = Sealed::seal(SeqNo::ZERO, body("b"));
        assert_ne!(left.hash(), right.hash());
    }

    #[test]
    fn reading_goes_through_deref_without_allocating() {
        let sealed = Sealed::seal(SeqNo::ZERO, body("borrowed"));
        // Deref gives the read path every consumer needs; there is no mutable
        // counterpart, which is invariant I1.
        assert_eq!(sealed.text.len(), 8);
        assert_eq!(sealed.as_ref().text, "borrowed");
    }

    #[test]
    fn sequence_numbers_advance_by_one() {
        let mut seq = SeqNo::ZERO;
        for expected in 0..8_u64 {
            assert_eq!(seq.get(), expected);
            seq = seq.next();
        }
        assert_eq!(SeqNo::ZERO.to_string(), "#0");
    }

    #[test]
    fn a_tampered_digest_is_refused_on_load() {
        // The hole this closes: a store row whose contents were edited without
        // updating the digest must not load as a valid sealed segment.
        let sealed = Sealed::seal(SeqNo::from_raw(3), body("original"));
        let json = serde_json::to_string(&sealed).expect("serialise");

        let tampered = json.replace("original", "tampered");
        assert!(tampered.contains("tampered"), "the fixture must actually differ");

        let error = serde_json::from_str::<Sealed<Body>>(&tampered)
            .expect_err("a mismatched digest must not deserialise");
        assert!(error.to_string().contains("contents hash to"), "got: {error}");
    }

    #[test]
    fn an_intact_value_round_trips() {
        let sealed = Sealed::seal(SeqNo::from_raw(11), body("intact"));
        let json = serde_json::to_string(&sealed).expect("serialise");
        let restored: Sealed<Body> = serde_json::from_str(&json).expect("deserialise");
        assert_eq!(restored, sealed);
        assert!(restored.verify());
    }
}
