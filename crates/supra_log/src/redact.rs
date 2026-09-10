//! Redaction at the sink.
//!
//! # Why this exists at all
//!
//! T7 removed every field that could hold a credential from the configuration
//! schema, and then a test found the leak anyway: the error *refusing* a pasted
//! credential quoted the line it was refusing, straight into this log. The lesson was
//! not "fix that message" - it was that a secret reaches a sink through paths nobody
//! enumerated in advance.
//!
//! So redaction happens **at the sink**, on the formatted line, after every call site
//! has had its say. It is a net, not the primary mechanism. The primary mechanisms are
//! upstream: T7's schema has no field that can hold a credential, and T12 owns a
//! `Secret<T>` whose `Debug` and `Display` reveal nothing. This catches what escapes
//! those.
//!
//! # Two mechanisms, because each misses what the other catches
//!
//! **By field name.** In a structured line the field is named, so
//! `"api_key":"anything"` can be redacted whatever the value looks like. This is the
//! stronger of the two: it does not care whether the secret is shaped like a secret.
//!
//! **By value shape.** A credential pasted into a free-text message has no field name
//! to catch it, but `sk-` followed by forty-eight token characters is recognisable
//! wherever it appears.
//!
//! # What must survive, and why that constrains the design
//!
//! A generic "high entropy" rule would be more thorough and would make this crate
//! useless. supra's own diagnostic material *is* high-entropy strings: ULIDs, content
//! hashes, cache keys, git revisions. Redacting those would leave a log that cannot be
//! correlated with anything.
//!
//! Two survivals are load-bearing enough to be tests rather than intentions:
//!
//! - a `supra_types::ContentHash`, in both its 64-character and 12-character forms,
//!   because every prefix-stability diagnosis is a comparison between two of them;
//! - the *value* of `api_key_env`, which T7 defines as the **name** of an environment
//!   variable. Redacting `ANTHROPIC_API_KEY` would hide the one piece of information a
//!   reader needs to fix a credential problem.
//!
//! The field-name rule is therefore an exact match and never a prefix match: `api_key`
//! is a secret, `api_key_env` is a signpost, and the difference is four characters.
//!
//! # ASCII scanning on UTF-8 input
//!
//! Every prefix and every character class here is ASCII. A byte in a multi-byte UTF-8
//! sequence is always `>= 0x80`, so it can never match one of these classes - which
//! means every offset the scanner computes falls on a character boundary, and slicing
//! at it cannot panic. That is why the implementation can work in bytes without
//! decoding.

use std::borrow::Cow;

/// What replaces a redacted value: the marker plus the length that was removed.
///
/// The length is kept deliberately. It distinguishes "the field was empty" from "the
/// field held something", and it lets a reader confirm a key was the length it should
/// have been without learning it.
fn marker(len: usize) -> String {
    format!("<redacted:{len}>")
}

/// Characters a credential-like token is made of.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Charset {
    /// Letters, digits, `-`, `_`, `.`: the union of the shapes real API keys use.
    Token,
    /// Upper-case letters and digits, as AWS access key identifiers use.
    Upper,
}

impl Charset {
    const fn contains(self, byte: u8) -> bool {
        match self {
            Self::Token => byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'+' | b'/'),
            Self::Upper => byte.is_ascii_uppercase() || byte.is_ascii_digit(),
        }
    }
}

/// A credential recognisable from a distinctive prefix.
struct Shape {
    /// The literal that introduces it.
    prefix: &'static str,
    /// Shortest payload that is worth treating as a credential.
    ///
    /// Set well above the length of any plausible identifier that shares the prefix,
    /// and well below the length of any real credential.
    min_payload: usize,
    /// What the payload is made of.
    charset: Charset,
}

/// Prefixed credential shapes.
///
/// Each entry is a *labelled* secret: something whose prefix announces what it is.
/// Nothing here matches on entropy alone, for the reason in the module documentation.
const SHAPES: &[Shape] = &[
    // Anthropic and OpenAI style keys, including `sk-ant-`.
    Shape { prefix: "sk-", min_payload: 16, charset: Charset::Token },
    // GitHub personal access tokens, in all their prefixes.
    Shape { prefix: "ghp_", min_payload: 16, charset: Charset::Token },
    Shape { prefix: "gho_", min_payload: 16, charset: Charset::Token },
    Shape { prefix: "ghu_", min_payload: 16, charset: Charset::Token },
    Shape { prefix: "ghs_", min_payload: 16, charset: Charset::Token },
    Shape { prefix: "ghr_", min_payload: 16, charset: Charset::Token },
    Shape { prefix: "github_pat_", min_payload: 16, charset: Charset::Token },
    // Slack.
    Shape { prefix: "xoxb-", min_payload: 16, charset: Charset::Token },
    Shape { prefix: "xoxp-", min_payload: 16, charset: Charset::Token },
    Shape { prefix: "xoxa-", min_payload: 16, charset: Charset::Token },
    Shape { prefix: "xoxs-", min_payload: 16, charset: Charset::Token },
    // Google.
    Shape { prefix: "AIza", min_payload: 30, charset: Charset::Token },
    // AWS access key identifiers: four-letter prefix then sixteen upper-case.
    Shape { prefix: "AKIA", min_payload: 16, charset: Charset::Upper },
    Shape { prefix: "ASIA", min_payload: 16, charset: Charset::Upper },
    // Any Authorization header value, whatever the scheme wraps.
    Shape { prefix: "Bearer ", min_payload: 16, charset: Charset::Token },
];

/// Field names whose value is a secret regardless of its shape.
///
/// **Exact** matches. `api_key` is here and `api_key_env` is deliberately not: T7
/// defines the latter as the *name* of an environment variable, and a reader chasing a
/// credential problem needs to see it.
const SECRET_FIELDS: &[&str] = &[
    "api_key",
    "apikey",
    "auth_token",
    "authorization",
    "credential",
    "credentials",
    "passwd",
    "password",
    "private_key",
    "secret",
    "secret_key",
    "session_key",
    "token",
];

/// Marker that opens a PEM block.
const PEM_BEGIN: &str = "-----BEGIN ";

/// Remove anything that looks like a credential from one formatted line.
///
/// Borrows when nothing matched, which is the overwhelmingly common case and keeps the
/// logging path allocation-free for ordinary lines.
#[must_use]
pub fn redact(line: &str) -> Cow<'_, str> {
    // Cheap pre-check. Scanning for the first byte of any prefix is far cheaper than
    // running the full matcher, and almost every line exits here.
    if !might_contain_secret(line) {
        return Cow::Borrowed(line);
    }

    let bytes = line.as_bytes();
    let mut out = String::with_capacity(line.len());
    let mut index = 0;

    while index < bytes.len() {
        if let Some((consumed, replacement)) = match_at(line, index) {
            out.push_str(&replacement);
            index += consumed;
            continue;
        }
        // Copy one byte. Safe as a byte push because a non-matching position is either
        // ASCII or part of a multi-byte sequence being copied verbatim.
        out.push_str(line.get(index..=index).unwrap_or_else(|| next_char_slice(line, index)));
        index += slice_len(line, index);
    }

    if out == line { Cow::Borrowed(line) } else { Cow::Owned(out) }
}

/// Whether any mechanism could possibly fire.
///
/// A fast rejection for the overwhelming majority of lines, which contain nothing
/// interesting and should not pay for a character-by-character walk.
///
/// **Derived from the tables, never restated.** The first version listed its own
/// literals - `"sk-"`, `"gh"`, `"xox"` - and was wrong within minutes: `github_pat_`
/// does not contain `gh`, because the letters are g-i-t-h. Every `github_pat_`
/// credential passed straight through a matcher that would have caught it, and the
/// failure was silent in the only direction that matters. A second, weaker copy of the
/// matcher's knowledge is a place for the two to disagree, so this reads the same
/// tables the matcher does, and a property test walks every entry to confirm it.
fn might_contain_secret(line: &str) -> bool {
    SHAPES.iter().any(|shape| line.contains(shape.prefix))
        || SECRET_FIELDS.iter().any(|field| line.contains(field))
        || line.contains(PEM_BEGIN)
}

/// Try every mechanism at one offset.
///
/// Returns the bytes consumed from the input and the text to emit in their place.
fn match_at(line: &str, index: usize) -> Option<(usize, String)> {
    if let Some(found) = match_pem(line, index) {
        return Some(found);
    }
    if let Some(found) = match_field(line, index) {
        return Some(found);
    }
    match_shape(line, index)
}

/// A PEM block, redacted whole.
///
/// A private key in a log is the worst case, so the rule is coarse on purpose: from
/// `-----BEGIN ` to the end of the closing marker, or to the end of the line if the
/// block was truncated. Nothing inside is worth preserving.
fn match_pem(line: &str, index: usize) -> Option<(usize, String)> {
    if !line[index..].starts_with(PEM_BEGIN) {
        return None;
    }
    let rest = &line[index..];
    let end = rest
        .find("-----END ")
        .and_then(|at| rest[at..].find("-----\\n").map(|close| at + close + "-----\\n".len()))
        .or_else(|| {
            rest.find("-----END ").and_then(|at| rest[at + 9..].find("-----").map(|close| at + 9 + close + 5))
        })
        .unwrap_or(rest.len());
    Some((end, marker(end)))
}

/// A named field whose value is secret whatever it looks like.
///
/// Handles both renderings a formatter produces: `"api_key":"value"` in JSON and
/// `api_key=value` in the compact form.
fn match_field(line: &str, index: usize) -> Option<(usize, String)> {
    let rest = &line[index..];

    for field in SECRET_FIELDS {
        // JSON: the quoted name, a colon, then a quoted value.
        if let Some(after_name) = strip_json_name(rest, field) {
            if let Some((value_len, total)) = json_string_value(after_name) {
                let consumed = rest.len() - after_name.len() + total;
                let head = &rest[..rest.len() - after_name.len()];
                return Some((consumed, format!("{head}\"{}\"", marker(value_len))));
            }
        }

        // Compact: the bare name, an equals sign, then a bare or quoted value.
        if let Some(after) = rest.strip_prefix(*field) {
            if let Some(value) = after.strip_prefix('=') {
                if !is_boundary(line.as_bytes(), index) {
                    continue;
                }
                let value_len = compact_value_len(value);
                if value_len > 0 {
                    return Some((field.len() + 1 + value_len, format!("{field}={}", marker(value_len))));
                }
            }
        }
    }
    None
}

fn compact_value_len(value: &str) -> usize {
    if value.starts_with('"') {
        return json_string_value(value).map_or_else(|| value.len(), |(_, total)| total);
    }
    value.bytes().take_while(|byte| !byte.is_ascii_whitespace()).count()
}

/// Match `"<field>":` allowing the whitespace a formatter may insert.
fn strip_json_name<'a>(rest: &'a str, field: &str) -> Option<&'a str> {
    let after_open = rest.strip_prefix('"')?;
    let after_name = after_open.strip_prefix(field)?;
    let after_close = after_name.strip_prefix('"')?;
    let after_colon = after_close.trim_start().strip_prefix(':')?;
    Some(after_colon.trim_start())
}

/// Length of a JSON string value and the bytes it occupies including its quotes.
fn json_string_value(rest: &str) -> Option<(usize, usize)> {
    let body = rest.strip_prefix('"')?;
    let bytes = body.as_bytes();
    let mut at = 0;
    while at < bytes.len() {
        match bytes[at] {
            b'\\' => at += 2,
            b'"' => return Some((at, at + 2)),
            _ => at += 1,
        }
    }
    None
}

/// A prefixed credential shape.
fn match_shape(line: &str, index: usize) -> Option<(usize, String)> {
    let bytes = line.as_bytes();
    if !is_boundary(bytes, index) {
        return None;
    }
    let rest = &line[index..];

    for shape in SHAPES {
        if !rest.starts_with(shape.prefix) {
            continue;
        }
        let payload = &bytes[index + shape.prefix.len()..];
        let length = payload.iter().take_while(|byte| shape.charset.contains(**byte)).count();
        if length >= shape.min_payload {
            return Some((shape.prefix.len() + length, format!("{}{}", shape.prefix, marker(length))));
        }
    }
    None
}

/// Whether `index` starts a token rather than sitting inside one.
///
/// Without this, `mask-1234567890123456` would be redacted: it contains `sk-` at
/// offset two. The byte before a match must not itself be a token character.
fn is_boundary(bytes: &[u8], index: usize) -> bool {
    if index == 0 {
        return true;
    }
    let previous = bytes[index - 1];
    !(previous.is_ascii_alphanumeric() || matches!(previous, b'-' | b'_' | b'.'))
}

/// The slice for the character starting at `index`.
fn next_char_slice(line: &str, index: usize) -> &str {
    let end = (index + 1..=line.len()).find(|at| line.is_char_boundary(*at)).unwrap_or(line.len());
    line.get(index..end).unwrap_or("")
}

/// Byte length of the character starting at `index`.
fn slice_len(line: &str, index: usize) -> usize {
    next_char_slice(line, index).len().max(1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use supra_types::{CanonicalWriter, ContentHash, Sealable, TurnId};

    #[test]
    fn an_ordinary_line_is_borrowed_not_copied() {
        // The hot path. Almost every line has no secret in it, and allocating for all
        // of them would make the logger the expensive part of a turn.
        let line = r#"{"level":"INFO","target":"supra_core","message":"turn started"}"#;
        assert!(matches!(redact(line), Cow::Borrowed(_)));
    }

    #[test]
    fn the_keys_that_were_actually_pasted_are_redacted() {
        // Shaped like the two credentials that were pasted into a chat by accident,
        // which is the concrete incident this module answers. The values here are
        // invented, but the shape is the one that matters.
        for key in [
            "sk-GDlWK9BcaRJJX2D8fLkJ4gZO52bpSORPOYqo39ILOvmGiXtf",
            "sk-ant-api03-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
        ] {
            let line = format!("failed to authenticate with {key}");
            let redacted = redact(&line);
            assert!(!redacted.contains(key), "the key survived: {redacted}");
            assert!(redacted.contains("sk-<redacted:"), "{redacted}");
        }
    }

    #[test]
    fn a_content_hash_survives_in_both_its_forms() {
        // Load-bearing. Every prefix-stability diagnosis is a comparison between two
        // content hashes, so a rule that ate them would make this log useless for the
        // thing it exists to debug.
        struct Body(&'static str);
        impl Sealable for Body {
            const CANONICAL_KIND: u8 = 0xE0;
            fn write_canonical(&self, writer: &mut CanonicalWriter) {
                writer.str(1, self.0);
            }
        }

        let hash = ContentHash::of(&Body("a prefix"));
        let full = hash.to_string();
        let short = hash.short();

        let line = format!("cache break: expected {full} found {short}");
        let redacted = redact(&line);
        assert!(redacted.contains(&full), "the full hash was eaten: {redacted}");
        assert!(redacted.contains(&short), "the short hash was eaten: {redacted}");
    }

    #[test]
    fn a_ulid_survives() {
        let id = TurnId::generate().to_string();
        let line = format!("turn {id} completed");
        assert!(redact(&line).contains(&id), "a ULID must remain correlatable");
    }

    #[test]
    fn a_git_revision_survives() {
        let sha = "e83e9c2b1f4a5d6c7b8a9f0e1d2c3b4a5f6e7d8c";
        let line = format!("digest updated at {sha}");
        assert!(redact(&line).contains(sha));
    }

    #[test]
    fn the_name_of_a_credential_variable_survives() {
        // T7 stores the NAME of an environment variable, never the value. Redacting
        // the name would hide the one thing a reader needs to fix the problem - and
        // the difference from a real secret field is four characters.
        let line = r#"{"api_key_env":"ANTHROPIC_API_KEY","provider":"anthropic"}"#;
        let redacted = redact(line);
        assert!(redacted.contains("ANTHROPIC_API_KEY"), "{redacted}");
        assert!(!redacted.contains("redacted"), "{redacted}");

        let compact = "api_key_env=ANTHROPIC_API_KEY provider=anthropic";
        assert!(redact(compact).contains("ANTHROPIC_API_KEY"), "{}", redact(compact));

        // The keyring form too.
        let keyring = r#"{"api_key_keyring":"supra/anthropic"}"#;
        assert!(redact(keyring).contains("supra/anthropic"));
    }

    #[test]
    fn a_named_secret_field_is_redacted_whatever_its_value_looks_like() {
        // The stronger of the two mechanisms: the value here is an ordinary word, so
        // no shape rule would catch it.
        let line = r#"{"api_key":"hunter2","target":"provider"}"#;
        let redacted = redact(line);
        assert!(!redacted.contains("hunter2"), "{redacted}");
        assert!(redacted.contains(r#""api_key":"<redacted:7>""#), "{redacted}");
        assert!(redacted.contains("provider"), "context must survive: {redacted}");
    }

    #[test]
    fn compact_quoted_values_are_redacted_whole() {
        let line = r#"password="correct horse \"battery\" staple" next=visible"#;
        let redacted = redact(line);
        for secret in ["correct", "horse", "battery", "staple"] {
            assert!(!redacted.contains(secret), "{redacted}");
        }
        assert!(redacted.contains("next=visible"), "{redacted}");
    }

    #[test]
    fn every_named_secret_field_is_actually_wired_in() {
        // Exhaustive over the table, so a name that is declared but never matched
        // cannot pass unnoticed.
        for field in SECRET_FIELDS {
            let json = format!(r#"{{"{field}":"leaked-value-here"}}"#);
            let redacted = redact(&json);
            assert!(!redacted.contains("leaked-value-here"), "{field} (json): {redacted}");

            let compact = format!("{field}=leaked-value-here rest=ok");
            let redacted = redact(&compact);
            assert!(!redacted.contains("leaked-value-here"), "{field} (compact): {redacted}");
            assert!(redacted.contains("rest=ok"), "{field}: context must survive: {redacted}");
        }
    }

    #[test]
    fn every_shape_is_actually_wired_in() {
        for shape in SHAPES {
            let payload = "A1b2C3d4E5f6G7h8".repeat(4);
            let payload = match shape.charset {
                Charset::Token => payload,
                Charset::Upper => "ABCDEFGH12345678".repeat(4),
            };
            let line = format!("value {}{payload} end", shape.prefix);
            let redacted = redact(&line);
            assert!(
                redacted.contains(&format!("{}<redacted:", shape.prefix)),
                "{} not matched: {redacted}",
                shape.prefix
            );
            assert!(redacted.contains("end"), "context must survive: {redacted}");
        }
    }

    #[test]
    fn a_prefix_inside_a_longer_token_is_not_a_match() {
        // `mask-1234567890123456` contains `sk-` at offset two. Without the left
        // boundary check, an ordinary identifier would be redacted.
        for benign in [
            "mask-1234567890123456",
            "task-abcdefghijklmnop",
            "risk-0000000000000000",
            "path/to/mask-1234567890123456",
        ] {
            let line = format!("value {benign}");
            assert!(redact(&line).contains(benign), "{benign} was eaten");
        }
    }

    #[test]
    fn a_short_payload_is_not_a_credential() {
        for benign in ["sk-test", "sk-1", "ghp_short", "Bearer x"] {
            let line = format!("value {benign} end");
            assert!(redact(&line).contains(benign), "{benign} was eaten");
        }
    }

    #[test]
    fn an_authorization_header_value_is_redacted_whatever_it_wraps() {
        let line = "sending Authorization: Bearer abcdefghijklmnopqrstuvwxyz012345";
        let redacted = redact(line);
        assert!(!redacted.contains("abcdefghijklmnopqrstuvwxyz012345"), "{redacted}");
        // 26 letters plus 6 digits.
        assert!(redacted.contains("Bearer <redacted:32>"), "{redacted}");
    }

    #[test]
    fn the_fast_path_never_rejects_a_line_the_matcher_would_change() {
        // The property that closes a real bug. The pre-check used to restate the
        // matcher's literals and got one wrong - `github_pat_` does not contain `gh` -
        // so every credential with that prefix passed straight through. Deriving the
        // pre-check from the tables makes the two agree by construction; this walks
        // every entry to prove they still do.
        for shape in SHAPES {
            let payload = match shape.charset {
                Charset::Token => "A1b2C3d4E5f6G7h8".repeat(4),
                Charset::Upper => "ABCDEFGH12345678".repeat(4),
            };
            let line = format!("prefix {}{payload} suffix", shape.prefix);
            assert!(
                might_contain_secret(&line),
                "the fast path rejects {:?}, which the matcher catches",
                shape.prefix
            );
            assert!(
                matches!(redact(&line), Cow::Owned(_)),
                "{:?} reached the matcher but was not redacted",
                shape.prefix
            );
        }

        for field in SECRET_FIELDS {
            let line = format!(r#"{{"{field}":"value-here"}}"#);
            assert!(might_contain_secret(&line), "the fast path rejects {field}");
            assert!(matches!(redact(&line), Cow::Owned(_)), "{field} was not redacted");
        }

        let pem = format!("{PEM_BEGIN}RSA PRIVATE KEY-----\\nbody\\n-----END RSA PRIVATE KEY-----");
        assert!(might_contain_secret(&pem));
        assert!(matches!(redact(&pem), Cow::Owned(_)));
    }

    #[test]
    fn a_pem_block_is_removed_whole() {
        // The worst case. Coarse on purpose: nothing inside a private key block is
        // worth preserving.
        let line =
            "key material -----BEGIN RSA PRIVATE KEY-----\\nMIIEow==\\n-----END RSA PRIVATE KEY-----\\n tail";
        let redacted = redact(line);
        assert!(!redacted.contains("MIIEow"), "{redacted}");
        assert!(!redacted.contains("PRIVATE KEY"), "{redacted}");
        assert!(redacted.contains("tail"), "context must survive: {redacted}");
    }

    #[test]
    fn a_truncated_pem_block_is_still_removed() {
        let line = "-----BEGIN OPENSSH PRIVATE KEY-----\\nb3BlbnNzaC1r";
        let redacted = redact(line);
        assert!(!redacted.contains("b3BlbnNzaC1r"), "{redacted}");
    }

    #[test]
    fn redaction_is_stable_under_reapplication() {
        // The sink may see a line that has already passed through here - a nested
        // message, a replayed event. Redacting twice must not corrupt the marker.
        let line = "key sk-GDlWK9BcaRJJX2D8fLkJ4gZO52bpSORPOYqo39ILOvmGiXtf end";
        let once = redact(line).into_owned();
        let twice = redact(&once).into_owned();
        assert_eq!(once, twice);
    }

    #[test]
    fn several_secrets_on_one_line_are_all_removed() {
        let line = concat!(
            r#"{"api_key":"first-secret","note":"and sk-ABCDEFGHIJKLMNOPQRSTUVWX too","#,
            r#""token":"third-secret"}"#
        );
        let redacted = redact(line);
        assert!(!redacted.contains("first-secret"), "{redacted}");
        assert!(!redacted.contains("ABCDEFGHIJKLMNOPQRSTUVWX"), "{redacted}");
        assert!(!redacted.contains("third-secret"), "{redacted}");
        assert!(redacted.contains("note"), "structure must survive: {redacted}");
    }

    #[test]
    fn the_length_is_kept_so_a_reader_can_tell_empty_from_present() {
        let empty = redact(r#"{"api_key":""}"#).into_owned();
        assert!(empty.contains("<redacted:0>"), "{empty}");

        let present = redact(r#"{"api_key":"abcd"}"#).into_owned();
        assert!(present.contains("<redacted:4>"), "{present}");
    }

    #[test]
    fn multibyte_input_is_never_split() {
        // The scanner works in bytes. Every class it tests is ASCII, so a continuation
        // byte can never match - but the copy path still has to be boundary-safe.
        let line = "\u{4E2D}\u{6587} sk-ABCDEFGHIJKLMNOPQRSTUVWX \u{1F600} tail";
        let redacted = redact(line);
        assert!(redacted.contains('\u{4E2D}'), "{redacted}");
        assert!(redacted.contains('\u{1F600}'), "{redacted}");
        assert!(!redacted.contains("ABCDEFGHIJKLMNOPQRSTUVWX"), "{redacted}");
    }

    #[test]
    fn an_empty_line_is_handled() {
        assert_eq!(redact(""), "");
        assert_eq!(redact("\n"), "\n");
    }

    #[test]
    fn a_json_value_containing_an_escaped_quote_is_bounded_correctly() {
        let line = r#"{"api_key":"ab\"cd","after":"visible"}"#;
        let redacted = redact(line);
        assert!(!redacted.contains(r#"ab\"cd"#), "{redacted}");
        assert!(redacted.contains("visible"), "the next field must survive: {redacted}");
    }
}
