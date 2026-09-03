//! Content hashing, and the canonical encoding it runs over.
//!
//! # Why this is not the wire serialiser
//!
//! I7 requires canonical (stably ordered) serialisation for the provider request,
//! because unstable `tool_use` key ordering is a documented cache breaker. That
//! serialiser is T13's, and it emits JSON.
//!
//! Hashing deliberately does **not** reuse it. If the hash were computed over the
//! wire bytes, then any change to the wire format - a provider adding a field, a
//! JSON library changing its escaping - would change every hash in the store, and
//! I4's guarantee that a recalled turn is returned *byte-identical* would be
//! measured against a moving target. The encoding here is instead a fixed,
//! minimal, length-prefixed form owned by this crate, with no dependency on
//! `serde_json` at all.
//!
//! # Why length prefixes
//!
//! Concatenating fields before hashing is ambiguous: `("ab", "c")` and
//! `("a", "bc")` produce identical bytes and therefore identical hashes, so two
//! different segments would collide. Every field is written as
//! `tag | length (u64 LE) | bytes`, which removes that class outright. There is a
//! test for exactly this pair.
//!
//! # Why a kind discriminant
//!
//! Two different types can happen to use the same tags in the same order. Each
//! [`crate::Sealable`] declares a `CANONICAL_KIND`, written first, so their
//! encodings live in separate domains and cannot collide by coincidence.

use core::fmt;

use serde::{Deserialize, Deserializer, Serialize, Serializer, de};

/// A BLAKE3 digest over a value's canonical encoding.
///
/// Content-addressed: it covers the value's kind and contents, and deliberately
/// **not** its position in any sequence. That makes it usable directly as a
/// content-addressed store key (T16.6 journal), and leaves ordering to be hashed
/// by whoever owns the ordering - the T14 ledger hashes the sequence of
/// `(SeqNo, ContentHash)` pairs for its prefix hash.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ContentHash([u8; Self::LEN]);

impl ContentHash {
    /// Digest length in bytes.
    pub const LEN: usize = 32;

    /// Characters in the hex form.
    pub const HEX_LEN: usize = Self::LEN * 2;

    /// Digest of a value's canonical encoding.
    #[must_use]
    pub fn of<T: crate::Sealable>(value: &T) -> Self {
        let mut writer = CanonicalWriter::for_kind(T::CANONICAL_KIND);
        value.write_canonical(&mut writer);
        writer.finish()
    }

    /// Wrap raw digest bytes.
    #[must_use]
    pub const fn from_bytes(bytes: [u8; Self::LEN]) -> Self {
        Self(bytes)
    }

    /// The raw digest bytes.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; Self::LEN] {
        &self.0
    }

    /// Read a 64-character lowercase hex digest.
    ///
    /// # Errors
    ///
    /// [`ParseHashError`] when the input is not exactly [`Self::HEX_LEN`] hex
    /// digits. Uppercase is accepted on input; output is always lowercase, so a
    /// hash written by this type round-trips byte-identically.
    pub fn from_hex(text: &str) -> Result<Self, ParseHashError> {
        if text.len() != Self::HEX_LEN {
            return Err(ParseHashError::Length { found: text.len() });
        }

        let mut bytes = [0_u8; Self::LEN];
        for (index, slot) in bytes.iter_mut().enumerate() {
            let start = index * 2;
            let pair = text.get(start..start + 2).ok_or(ParseHashError::NotHex { offset: start })?;
            *slot = u8::from_str_radix(pair, 16).map_err(|_| ParseHashError::NotHex { offset: start })?;
        }
        Ok(Self(bytes))
    }

    /// First 12 hex characters, for a status line or a log line.
    ///
    /// Twelve is 48 bits: enough that a collision within one session's segments is
    /// not a practical concern, short enough to sit in a terminal column. Never
    /// use it for comparison - compare the full value.
    #[must_use]
    pub fn short(&self) -> String {
        self.to_string().chars().take(12).collect()
    }
}

impl fmt::Display for ContentHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for byte in &self.0 {
            write!(f, "{byte:02x}")?;
        }
        Ok(())
    }
}

impl fmt::Debug for ContentHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Short form in Debug: a full digest inside a forty-variant event dump
        // buries the fields a reader is looking for.
        write!(f, "ContentHash({})", self.short())
    }
}

impl Serialize for ContentHash {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for ContentHash {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = <&str>::deserialize(deserializer)?;
        Self::from_hex(text).map_err(de::Error::custom)
    }
}

/// Why a string could not be read as a [`ContentHash`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ParseHashError {
    /// Wrong number of characters.
    #[error("a content hash is {expected} hex characters, found {found}", expected = ContentHash::HEX_LEN)]
    Length {
        /// Characters actually present.
        found: usize,
    },
    /// A non-hex character.
    #[error("not a hex digit at offset {offset}")]
    NotHex {
        /// Byte offset of the offending pair.
        offset: usize,
    },
}

/// Builds the canonical encoding of a value and hashes it.
///
/// Every method appends; there is no way to rewrite what was already written,
/// which mirrors the append-only discipline of the ledger the hashes serve.
pub struct CanonicalWriter {
    hasher: blake3::Hasher,
}

impl CanonicalWriter {
    /// Start an encoding in the domain of `kind`.
    #[must_use]
    pub fn for_kind(kind: u8) -> Self {
        let mut hasher = blake3::Hasher::new();
        // Domain separator, written before any field so two types with
        // coincidentally identical field layouts cannot produce the same digest.
        hasher.update(&[kind]);
        Self { hasher }
    }

    /// Append a tagged byte field.
    pub fn bytes(&mut self, tag: u8, value: &[u8]) {
        self.hasher.update(&[tag]);
        // Length as a fixed 8 bytes little-endian. A varint would be shorter and
        // would reintroduce the ambiguity this exists to remove, since the length
        // of the length would itself be data-dependent.
        self.hasher.update(&(value.len() as u64).to_le_bytes());
        self.hasher.update(value);
    }

    /// Append a tagged string field.
    pub fn str(&mut self, tag: u8, value: &str) {
        self.bytes(tag, value.as_bytes());
    }

    /// Append a tagged unsigned integer.
    pub fn u64(&mut self, tag: u8, value: u64) {
        self.bytes(tag, &value.to_le_bytes());
    }

    /// Append a tagged byte-sized value, such as an enum discriminant.
    pub fn u8(&mut self, tag: u8, value: u8) {
        self.bytes(tag, &[value]);
    }

    /// Append a tagged boolean.
    pub fn bool(&mut self, tag: u8, value: bool) {
        self.u8(tag, u8::from(value));
    }

    /// Append a tagged element count, before writing that many elements.
    ///
    /// Required for sequences: without it, a two-element list and the
    /// concatenation of two one-element lists encode identically.
    pub fn count(&mut self, tag: u8, len: usize) {
        self.u64(tag, len as u64);
    }

    /// Append an optional string, distinguishing absent from empty.
    ///
    /// `None` and `Some("")` must not collide: an absent thinking-block signature
    /// and an empty one mean different things to a provider.
    pub fn opt_str(&mut self, tag: u8, value: Option<&str>) {
        match value {
            None => self.u8(tag, 0),
            Some(text) => {
                self.u8(tag, 1);
                self.str(tag, text);
            }
        }
    }

    /// Append the canonical encoding of a nested sealable value.
    pub fn nested<T: crate::Sealable>(&mut self, tag: u8, value: &T) {
        self.u8(tag, T::CANONICAL_KIND);
        value.write_canonical(self);
    }

    /// Finish and produce the digest.
    #[must_use]
    pub fn finish(self) -> ContentHash {
        ContentHash(*self.hasher.finalize().as_bytes())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Two fields, so the concatenation-ambiguity property has something to bite
    /// on.
    struct Pair(&'static str, &'static str);

    impl crate::Sealable for Pair {
        const CANONICAL_KIND: u8 = 0xF0;

        fn write_canonical(&self, writer: &mut CanonicalWriter) {
            writer.str(1, self.0);
            writer.str(2, self.1);
        }
    }

    /// Same tags, same order, different type: the domain separator must keep them
    /// apart.
    struct Twin(&'static str, &'static str);

    impl crate::Sealable for Twin {
        const CANONICAL_KIND: u8 = 0xF1;

        fn write_canonical(&self, writer: &mut CanonicalWriter) {
            writer.str(1, self.0);
            writer.str(2, self.1);
        }
    }

    #[test]
    fn distinct_tags_separate_distinct_fields() {
        // Necessary but not sufficient, and worth naming honestly: this passes even
        // without length prefixes, because the two tags differ. The test that
        // actually depends on the prefix is the next one.
        assert_ne!(ContentHash::of(&Pair("ab", "c")), ContentHash::of(&Pair("a", "bc")));
    }

    #[test]
    fn a_payload_containing_a_tag_byte_cannot_forge_a_field_boundary() {
        // The case that genuinely rests on the length prefix. Distinct tags already
        // separate distinct fields, so a two-field collision proves nothing; the
        // prefix earns its place when the *same* tag repeats - a sequence - and the
        // payload contains that tag byte.
        //
        // Not hypothetical: block text is arbitrary model and tool output, and 0x02
        // is a real tag in this crate's encodings.
        //
        // Without the prefix, both of these encode as `02 61 02 62`.
        let mut two_fields = CanonicalWriter::for_kind(1);
        two_fields.str(2, "a");
        two_fields.str(2, "b");

        let mut one_field = CanonicalWriter::for_kind(1);
        one_field.bytes(2, b"a\x02b");

        assert_ne!(
            two_fields.finish(),
            one_field.finish(),
            "a payload must not be able to smuggle a field boundary"
        );
    }

    #[test]
    fn a_payload_that_looks_like_a_length_header_is_also_unambiguous() {
        // The symmetric case: content shaped like a tag plus a length. The prefix is
        // fixed width and read positionally rather than scanned for, so content
        // cannot impersonate it.
        let mut plain = CanonicalWriter::for_kind(1);
        plain.str(3, "x");

        let mut crafted = CanonicalWriter::for_kind(1);
        crafted.bytes(3, b"\x03\x01\x00\x00\x00\x00\x00\x00\x00x");

        assert_ne!(plain.finish(), crafted.finish());
    }

    #[test]
    fn the_kind_discriminant_separates_domains() {
        assert_ne!(ContentHash::of(&Pair("x", "y")), ContentHash::of(&Twin("x", "y")));
    }

    #[test]
    fn hashing_is_deterministic() {
        // The premise of the whole prefix-stability design: identical content must
        // hash identically, every time, in any process.
        assert_eq!(ContentHash::of(&Pair("a", "b")), ContentHash::of(&Pair("a", "b")));
    }

    #[test]
    fn any_change_changes_the_digest() {
        let base = ContentHash::of(&Pair("alpha", "beta"));
        assert_ne!(base, ContentHash::of(&Pair("alpha", "betb")));
        assert_ne!(base, ContentHash::of(&Pair("alpha", "beta ")));
        assert_ne!(base, ContentHash::of(&Pair("", "alphabeta")));
    }

    #[test]
    fn absent_and_empty_do_not_collide() {
        let mut absent = CanonicalWriter::for_kind(1);
        absent.opt_str(7, None);

        let mut empty = CanonicalWriter::for_kind(1);
        empty.opt_str(7, Some(""));

        assert_ne!(absent.finish(), empty.finish());
    }

    #[test]
    fn counts_separate_a_list_from_its_concatenation() {
        let mut two = CanonicalWriter::for_kind(1);
        two.count(1, 2);
        two.str(2, "a");
        two.str(2, "b");

        let mut one_then_one = CanonicalWriter::for_kind(1);
        one_then_one.count(1, 1);
        one_then_one.str(2, "a");
        one_then_one.count(1, 1);
        one_then_one.str(2, "b");

        assert_ne!(two.finish(), one_then_one.finish());
    }

    #[test]
    fn hex_round_trips() {
        let hash = ContentHash::of(&Pair("round", "trip"));
        let text = hash.to_string();
        assert_eq!(text.len(), ContentHash::HEX_LEN);
        assert_eq!(ContentHash::from_hex(&text), Ok(hash));
        // Output is lowercase, and uppercase input is accepted.
        assert_eq!(ContentHash::from_hex(&text.to_uppercase()), Ok(hash));
        assert_eq!(text, text.to_lowercase());
    }

    #[test]
    fn hex_parsing_rejects_malformed_input() {
        assert_eq!(ContentHash::from_hex(""), Err(ParseHashError::Length { found: 0 }));
        assert_eq!(ContentHash::from_hex("ab"), Err(ParseHashError::Length { found: 2 }));

        let bad = "z".repeat(ContentHash::HEX_LEN);
        assert_eq!(ContentHash::from_hex(&bad), Err(ParseHashError::NotHex { offset: 0 }));

        // Non-ASCII must be rejected on length rather than panicking on a byte
        // slice that splits a character.
        let wide = "\u{4E2D}".repeat(ContentHash::HEX_LEN);
        assert!(ContentHash::from_hex(&wide).is_err());
    }

    #[test]
    fn short_form_is_a_prefix_of_the_full_form() {
        let hash = ContentHash::of(&Pair("s", "h"));
        assert_eq!(hash.short().len(), 12);
        assert!(hash.to_string().starts_with(&hash.short()));
    }
}
