//! Canonical JSON: the only sanctioned producer of [`supra_types::CanonicalJson`].
//!
//! # Why this module exists
//!
//! Provider documentation names unstable `tool_use` key ordering as a cache breaker: two
//! requests with the same tool calls in different key orders are different prefixes, so
//! the second pays a full cache write for bytes that are semantically identical. T6 holds
//! the canonical *text* ([`supra_types::CanonicalJson`]) but cannot verify canonicity - doing so would
//! mean parsing JSON there, duplicating this serialiser, and giving I7 two
//! implementations that can disagree. This module is the single implementation.
//!
//! # What "canonical" means
//!
//! - Object keys sorted byte-wise (UTF-8 lexicographic), recursively, at every depth.
//! - No insignificant whitespace: `{"a":1}` not `{ "a": 1 }`.
//! - Strings escaped per RFC 8259 with the shortest spelling (`\"`, `\\`, `\n`, `\u00XX`
//!   for other controls). `serde_json` is the substrate that gets escaping right; the
//!   sorting is what this module adds.
//! - Numbers emitted as `serde_json` emits them. Floats are refused outright (see below).
//!
//! # Floats are refused, not formatted
//!
//! T6's contract layer contains no floats because a float's text form is the one primitive
//! whose byte-stability is not obvious across platforms and libraries. A canonicaliser
//! that accepted floats would have to pick a formatting - shortest round-trip, fixed
//! precision, integer-when-whole - and every choice is a second implementation of a
//! decision the contract layer already made by refusing. Tool arguments carrying floats
//! are a caller defect, reported as one.
//!
//! # Duplicate keys are refused, not last-wins
//!
//! JSON objects with duplicate keys are legal input to `serde_json::Value` (last wins,
//! silently). Canonicalising one would bless a lost argument as stable. The parser here
//! rejects duplicates before sorting, so `{"a":1,"a":2}` is an error, not a choice.

use serde_json::Value;
use thiserror::Error;

/// Why canonicalisation failed.
#[derive(Clone, Debug, PartialEq, Eq, Error)]
pub enum CanonicalError {
    /// A float was found where only integers, strings, bools, nulls, arrays, and objects
    /// may appear.
    #[error("tool arguments must not contain floats (found at {path}): floats have no byte-stable text form")]
    Float {
        /// JSON path to the offending value, for the message.
        path: String,
    },
    /// An object held the same key twice.
    #[error("tool arguments must not repeat object keys (duplicate {key:?} at {path})")]
    DuplicateKey {
        /// The repeated key.
        key: String,
        /// JSON path to the object.
        path: String,
    },
    /// The input was not valid JSON.
    #[error("tool arguments are not valid JSON: {detail}")]
    Invalid {
        /// The parser's message.
        detail: String,
    },
    /// A number does not fit in i64/u64.
    #[error("number at {path} does not fit in 64 bits")]
    NumberOutOfRange {
        /// JSON path to the offending value.
        path: String,
    },
}

/// Serialise a JSON text canonically.
///
/// Parses `input`, rejects floats, duplicates, and out-of-range numbers, sorts every
/// object's keys, and emits the minimal form. The output is what
/// [`supra_types::CanonicalJson::from_canonical`] wraps - this function is the assertion
/// behind that constructor's name.
///
/// # Errors
///
/// [`CanonicalError`] for any input that cannot be made canonical.
pub fn canonicalize(input: &str) -> Result<supra_types::CanonicalJson, CanonicalError> {
    let value = parse_strict(input)?;
    check(&value, "$")?;
    let mut out = String::with_capacity(input.len());
    emit(&value, &mut out);
    Ok(supra_types::CanonicalJson::from_canonical(out))
}

/// Serialise an already-parsed value canonically. Same contract as [`canonicalize`].
///
/// # Errors
///
/// [`CanonicalError`] for any value that cannot be made canonical.
pub fn canonicalize_value(value: &Value) -> Result<supra_types::CanonicalJson, CanonicalError> {
    check(value, "$")?;
    let mut out = String::with_capacity(64);
    emit(value, &mut out);
    Ok(supra_types::CanonicalJson::from_canonical(out))
}

/// Reject floats, duplicate keys, and non-64-bit numbers, reporting the path.
#[allow(
    clippy::match_same_arms,
    reason = "Null and Bool are distinct scalar leaves sharing one verdict; merging them into an or-pattern would read as one case with two spellings"
)]
fn check(value: &Value, path: &str) -> Result<(), CanonicalError> {
    match value {
        // Null and booleans carry nothing to refuse.
        Value::Null => Ok(()),
        Value::Bool(_) => Ok(()),
        Value::Number(number) => {
            if number.is_f64() {
                Err(CanonicalError::Float { path: path.to_owned() })
            } else if number.is_i64() || number.is_u64() {
                Ok(())
            } else {
                Err(CanonicalError::NumberOutOfRange { path: path.to_owned() })
            }
        }
        Value::String(_) => Ok(()),
        Value::Array(items) => {
            for (index, item) in items.iter().enumerate() {
                check(item, &format!("{path}[{index}]"))?;
            }
            Ok(())
        }
        Value::Object(map) => {
            // Duplicates are rejected at parse by `parse_strict` (serde_json::Value takes
            // last-wins silently), so by the time a `Value` exists its keys are unique.
            // What remains for `check` is the float and range refusal, applied recursively.
            for (key, item) in map {
                check(item, &format!("{path}[{key:?}]"))?;
            }
            Ok(())
        }
    }
}

/// Emit the minimal form with keys sorted byte-wise at every depth.
fn emit(value: &Value, out: &mut String) {
    match value {
        Value::Null => out.push_str("null"),
        Value::Bool(true) => out.push_str("true"),
        Value::Bool(false) => out.push_str("false"),
        Value::Number(number) => out.push_str(&number.to_string()),
        Value::String(text) => {
            // `serde_json` escaping is the substrate that gets RFC 8259 right; the
            // canonicaliser adds ordering, not a second escaper.
            out.push_str(&serde_json::to_string(text).unwrap_or_else(|_| "\"\"".to_owned()));
        }
        Value::Array(items) => {
            out.push('[');
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                emit(item, out);
            }
            out.push(']');
        }
        Value::Object(map) => {
            // `serde_json::Map` without the `preserve_order` feature is a `BTreeMap`:
            // iteration is already byte-sorted, so the explicit sort below is redundant
            // *today*. It stays because the ordering guarantee must not depend on a
            // transitive feature flag no member selects: enabling `preserve_order`
            // anywhere in the graph would silently switch the map to insertion order,
            // and the canonicaliser would start emitting whatever the parser saw.
            // A redundant sort is cheap; a feature-dependent guarantee is not one.
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            out.push('{');
            for (index, key) in keys.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                out.push_str(&serde_json::to_string(key).unwrap_or_else(|_| "\"\"".to_owned()));
                out.push(':');
                emit(&map[*key], out);
            }
            out.push('}');
        }
    }
}

/// Parse with duplicate detection. `serde_json::from_str::<Value>` takes last-wins
/// silently; this parses via a duplicate-rejecting pass first.
///
/// # Errors
///
/// [`CanonicalError::Invalid`] when the text is not shaped as JSON,
/// [`CanonicalError::DuplicateKey`] when an object repeats a key.
pub fn parse_strict(input: &str) -> Result<Value, CanonicalError> {
    reject_duplicates(input)?;
    serde_json::from_str(input).map_err(|error| CanonicalError::Invalid { detail: error.to_string() })
}

/// Scan the raw text for a repeated key within one object.
///
/// Recursive-descent over the raw text: [`Scanner::value`] consumes one JSON value,
/// [`Scanner::object`] tracks the keys seen at its own depth. Whitespace is skipped by
/// [`Scanner::ws`]. Numbers, literals, and strings are consumed without building a
/// `Value` - only object keys need decoding (escapes resolved for identity, so
/// `{"A":1,"\\u0041":2}` is caught).
///
/// A struct with an explicit cursor, not nested `fn` items: the pedantic lint forbids
/// items after statements, and the scanner needs helpers sharing position state.
/// Each method takes `&mut self` and advances the cursor.
struct Scanner<'a> {
    bytes: &'a [u8],
    text: &'a str,
    position: usize,
}

impl<'a> Scanner<'a> {
    fn new(input: &'a str) -> Self {
        Self { bytes: input.as_bytes(), text: input, position: 0 }
    }

    fn run(&mut self) -> Result<(), CanonicalError> {
        self.value()?;
        self.ws();
        if self.position != self.bytes.len() {
            return Err(CanonicalError::Invalid { detail: "trailing characters".to_owned() });
        }
        Ok(())
    }

    fn ws(&mut self) {
        while self.position < self.bytes.len()
            && matches!(self.bytes[self.position], b' ' | b'\t' | b'\n' | b'\r')
        {
            self.position += 1;
        }
    }

    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.position).copied()
    }

    fn string(&mut self) -> Result<String, CanonicalError> {
        // `position` is at the opening quote.
        self.position += 1;
        let mut text = String::new();
        loop {
            let Some(byte) = self.peek() else {
                return Err(CanonicalError::Invalid { detail: "unterminated string".to_owned() });
            };
            if byte == b'"' {
                self.position += 1;
                return Ok(text);
            }
            if byte != b'\\' {
                // ASCII fast path; multibyte falls through to char decoding.
                if byte < 0x80 {
                    text.push(byte as char);
                    self.position += 1;
                } else {
                    let slice = &self.text[self.position..];
                    let next = slice.chars().next().unwrap_or('\u{FFFD}');
                    text.push(next);
                    self.position += next.len_utf8();
                }
                continue;
            }
            self.position += 1;
            let Some(escaped) = self.peek() else {
                return Err(CanonicalError::Invalid { detail: "truncated escape".to_owned() });
            };
            match escaped {
                b'"' => text.push('"'),
                b'\\' => text.push('\\'),
                b'/' => text.push('/'),
                b'b' => text.push('\u{0008}'),
                b'f' => text.push('\u{000C}'),
                b'n' => text.push('\n'),
                b'r' => text.push('\r'),
                b't' => text.push('\t'),
                b'u' => {
                    if self.position + 4 >= self.bytes.len() {
                        return Err(CanonicalError::Invalid {
                            detail: "truncated unicode escape".to_owned(),
                        });
                    }
                    let hex = &self.text[self.position + 1..self.position + 5];
                    let code = u32::from_str_radix(hex, 16).map_err(|_| CanonicalError::Invalid {
                        detail: "malformed unicode escape".to_owned(),
                    })?;
                    self.position += 4;
                    // A high surrogate pairs with the `\uXXXX` that follows; serde (the
                    // parser the canonical form is checked against) resolves pairs, so
                    // the scanner must decode the same key the parser will decode - a
                    // lone surrogate is not a JSON string at all.
                    if (0xD800..0xDC00).contains(&code) {
                        if self.text[self.position + 1..].starts_with("\\u") {
                            let low_hex = &self.text[self.position + 3..self.position + 7];
                            let low = u32::from_str_radix(low_hex, 16).unwrap_or(0);
                            self.position += 6;
                            let combined = 0x1_0000 + ((code - 0xD800) << 10) + (low.saturating_sub(0xDC00));
                            text.push(char::from_u32(combined).unwrap_or('\u{FFFD}'));
                            self.position += 1;
                            continue;
                        }
                        return Err(CanonicalError::Invalid { detail: "lone high surrogate".to_owned() });
                    }
                    if (0xDC00..0xE000).contains(&code) {
                        return Err(CanonicalError::Invalid { detail: "lone low surrogate".to_owned() });
                    }
                    text.push(char::from_u32(code).unwrap_or('\u{FFFD}'));
                    self.position += 1;
                    continue;
                }
                _ => {
                    return Err(CanonicalError::Invalid { detail: "invalid escape".to_owned() });
                }
            }
            self.position += 1;
        }
    }

    fn value(&mut self) -> Result<(), CanonicalError> {
        self.ws();
        let Some(byte) = self.peek() else {
            return Err(CanonicalError::Invalid { detail: "truncated value".to_owned() });
        };
        match byte {
            b'{' => self.object(),
            b'[' => {
                self.position += 1;
                self.ws();
                if self.peek() == Some(b']') {
                    self.position += 1;
                    return Ok(());
                }
                loop {
                    self.value()?;
                    self.ws();
                    let Some(next) = self.peek() else {
                        return Err(CanonicalError::Invalid { detail: "unterminated array".to_owned() });
                    };
                    if next == b']' {
                        self.position += 1;
                        return Ok(());
                    }
                    if next != b',' {
                        return Err(CanonicalError::Invalid { detail: "expected ',' or ']'".to_owned() });
                    }
                    self.position += 1;
                }
            }
            b'"' => {
                let _ = self.string()?;
                Ok(())
            }
            b't' if self.text[self.position..].starts_with("true") => {
                self.position += 4;
                Ok(())
            }
            b'f' if self.text[self.position..].starts_with("false") => {
                self.position += 5;
                Ok(())
            }
            b'n' if self.text[self.position..].starts_with("null") => {
                self.position += 4;
                Ok(())
            }
            _ => {
                // A number or a syntax error; serde_json decides below. Skip to the next
                // structural character so the scanner keeps its bearings.
                while self.position < self.bytes.len()
                    && !matches!(self.bytes[self.position], b',' | b']' | b'}' | b' ' | b'\t' | b'\n' | b'\r')
                {
                    self.position += 1;
                }
                Ok(())
            }
        }
    }

    fn object(&mut self) -> Result<(), CanonicalError> {
        self.position += 1; // consume '{'
        let mut seen = std::collections::BTreeSet::new();
        self.ws();
        if self.peek() == Some(b'}') {
            self.position += 1;
            return Ok(());
        }
        loop {
            self.ws();
            if self.peek() != Some(b'"') {
                return Err(CanonicalError::Invalid { detail: "expected an object key".to_owned() });
            }
            let key = self.string()?;
            if !seen.insert(key.clone()) {
                return Err(CanonicalError::DuplicateKey { key, path: "$".to_owned() });
            }
            self.ws();
            if self.peek() != Some(b':') {
                return Err(CanonicalError::Invalid {
                    detail: "expected ':' after an object key".to_owned(),
                });
            }
            self.position += 1;
            self.value()?;
            self.ws();
            let Some(next) = self.peek() else {
                return Err(CanonicalError::Invalid { detail: "unterminated object".to_owned() });
            };
            if next == b'}' {
                self.position += 1;
                return Ok(());
            }
            if next != b',' {
                return Err(CanonicalError::Invalid { detail: "expected ',' or '}'".to_owned() });
            }
            self.position += 1;
        }
    }
}

fn reject_duplicates(input: &str) -> Result<(), CanonicalError> {
    Scanner::new(input).run()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_sort_byte_wise_at_every_depth() {
        let out = canonicalize(r#"{"b":1,"a":{"d":4,"c":3}}"#).expect("valid");
        assert_eq!(out.as_str(), r#"{"a":{"c":3,"d":4},"b":1}"#);
    }

    #[test]
    fn whitespace_is_insignificant() {
        let tight = canonicalize(r#"{"a":1,"b":[1,2]}"#).expect("tight");
        let loose = canonicalize("{ \"a\" : 1 , \"b\" : [ 1 , 2 ] }").expect("loose");
        assert_eq!(tight.as_str(), loose.as_str());
        assert_eq!(tight.as_str(), r#"{"a":1,"b":[1,2]}"#);
    }

    #[test]
    fn key_order_does_not_change_the_bytes() {
        // The I7 property: two texts with the same meaning canonicalise identically, so
        // the prefix hash is stable across serialisations.
        let first = canonicalize(r#"{"x":1,"y":2}"#).expect("first");
        let second = canonicalize(r#"{"y":2,"x":1}"#).expect("second");
        assert_eq!(first.as_str(), second.as_str());
    }

    #[test]
    fn insertion_order_never_survives_canonicalisation() {
        // Defensive, not load-bearing: `serde_json::Map` without `preserve_order` is a
        // `BTreeMap`, so iteration is already sorted and deleting `keys.sort()` changes
        // nothing today. The test pins the output bytes against the day a transitive
        // feature enables `preserve_order` (IndexMap, insertion order) - at which point
        // this test, not a cache-miss report, is what catches it.
        let out = canonicalize(r#"{"z":0,"m":{"z":0,"a":0},"a":0}"#).expect("valid");
        assert_eq!(out.as_str(), r#"{"a":0,"m":{"a":0,"z":0},"z":0}"#);
    }

    #[test]
    fn the_map_behind_value_is_not_insertion_ordered() {
        // The assumption the sort's redundancy rests on, stated as a test so a feature
        // change breaks it loudly: parse an unsorted object and read the key order back.
        // `BTreeMap` yields sorted; `IndexMap` (preserve_order) would yield insertion.
        let value: serde_json::Value = serde_json::from_str(r#"{"z":0,"m":0,"a":0}"#).expect("valid");
        let order: Vec<&String> = value.as_object().expect("object").keys().collect();
        assert_eq!(order, vec!["a", "m", "z"], "Map is insertion-ordered: the sort is load-bearing");
    }

    #[test]
    fn floats_are_refused_with_a_path() {
        let error = canonicalize(r#"{"a":[1,2.5]}"#).expect_err("floats are refused");
        assert!(matches!(error, CanonicalError::Float { .. }), "{error}");
        // The path names the offending location; the exact spelling is the contract.
        assert_eq!(
            error,
            CanonicalError::Float { path: "$[\"a\"][1]".to_owned() },
            "path must pinpoint the float"
        );
    }

    #[test]
    fn duplicate_keys_are_refused_not_last_wins() {
        let error = canonicalize(r#"{"a":1,"a":2}"#).expect_err("duplicates are refused");
        assert!(matches!(error, CanonicalError::DuplicateKey { .. }), "{error}");
    }

    #[test]
    fn escaped_duplicate_keys_are_refused() {
        // Source-distinct but semantically identical: {"A":1,"\u0041":2} parses to one key.
        let error = canonicalize("{\"A\":1,\"\\u0041\":2}").expect_err("escaped duplicates are refused");
        assert!(matches!(error, CanonicalError::DuplicateKey { .. }), "{error}");
    }

    #[test]
    fn surrogate_escaped_duplicate_keys_are_refused() {
        // Literal 𝄞 and its surrogate-pair escape decode to the same string, so serde
        // would collapse them into one key and silently last-wins.
        let error = canonicalize("{\"𝄞\":1,\"\\uD834\\uDD1E\":2}")
            .expect_err("surrogate-escaped duplicates are refused");
        assert!(matches!(error, CanonicalError::DuplicateKey { .. }), "{error}");
    }

    #[test]
    fn lone_surrogate_escapes_are_invalid() {
        assert!(matches!(canonicalize("{\"\\uD834\":1}"), Err(CanonicalError::Invalid { .. })));
        assert!(matches!(canonicalize("{\"\\uDD1E\":1}"), Err(CanonicalError::Invalid { .. })));
    }

    #[test]
    fn a_paired_surrogate_decodes_to_its_scalar() {
        let out = canonicalize("{\"\\uD834\\uDD1E\":1}").expect("paired surrogate");
        assert_eq!(out.as_str(), "{\"𝄞\":1}");
    }

    #[test]
    fn invalid_json_is_an_error_not_a_panic() {
        assert!(matches!(canonicalize("{oops"), Err(CanonicalError::Invalid { .. })));
    }

    #[test]
    fn all_scalar_shapes_survive() {
        let out =
            canonicalize(r#"{"n":null,"t":true,"f":false,"i":-42,"u":18446744073709551615,"s":"a\"b\nc"}"#)
                .expect("scalars");
        assert_eq!(
            out.as_str(),
            r#"{"f":false,"i":-42,"n":null,"s":"a\"b\nc","t":true,"u":18446744073709551615}"#
        );
    }

    #[test]
    fn empty_containers_canonicalise() {
        assert_eq!(canonicalize("{}").expect("empty").as_str(), "{}");
        assert_eq!(canonicalize("[]").expect("empty").as_str(), "[]");
        assert_eq!(canonicalize_value(&Value::Null).expect("null").as_str(), "null");
    }
}
