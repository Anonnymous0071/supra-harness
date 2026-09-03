//! Terminal cell width and grapheme segmentation.
//!
//! Safe surface over `libsupra_width` (T2). Every function is total: no input
//! panics, and malformed UTF-8 is handled rather than rejected.

use core::ffi::CStr;

use crate::sys;

/// How to resolve East Asian Ambiguous code points.
///
/// A property of the terminal and its locale, not of the text. Decide once at
/// startup and pass the same value everywhere: mixing values within one frame
/// produces inconsistent layout.
///
/// This exists as a parameter rather than a constant because the alternative
/// silently corrupts layout for half the world's terminals. `U+2588 FULL BLOCK`
/// is one cell in a Latin locale and two under a CJK locale, and a gauge built
/// from it changes length depending on where it runs.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Ambiguous {
    /// One cell. Correct for Latin locales, and the common default.
    #[default]
    Narrow,
    /// Two cells. Correct under a CJK locale or with a CJK-wide font.
    Wide,
}

impl Ambiguous {
    const fn raw(self) -> core::ffi::c_int {
        match self {
            Self::Narrow => sys::AMBIGUOUS_NARROW,
            Self::Wide => sys::AMBIGUOUS_WIDE,
        }
    }
}

/// Cells occupied by one code point or cluster.
///
/// An enum rather than an integer because the C ABI returns `-1` as a sentinel,
/// and an `i32` return would let a caller add that sentinel into a running total.
/// `Zero` and `NonPrintable` are deliberately distinct: the first renders as
/// nothing (a combining mark), the second must not be sent at all (a control
/// character), and a caller sanitising output needs to tell them apart.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Width {
    /// Must not be emitted: a control, surrogate, or unassigned code point.
    NonPrintable,
    /// Occupies no cell: a combining mark, format character, or Extend class.
    Zero,
    /// One cell.
    One,
    /// Two cells.
    Two,
}

impl Width {
    fn from_raw(raw: core::ffi::c_int) -> Self {
        match raw {
            sys::WIDTH_NONPRINTABLE => Self::NonPrintable,
            0 => Self::Zero,
            1 => Self::One,
            // The ABI documents 0, 1, 2, and -1. Anything else would be a library
            // bug; treating it as the widest known value keeps a layout from
            // under-counting, which is the failure that wraps a line.
            _ => Self::Two,
        }
    }

    /// Cells to reserve when laying out text.
    ///
    /// `NonPrintable` counts as 0: it needs no room. Use
    /// [`validate`] to reject text containing such code points.
    #[must_use]
    pub const fn cells(self) -> usize {
        match self {
            Self::NonPrintable | Self::Zero => 0,
            Self::One => 1,
            Self::Two => 2,
        }
    }
}

/// One decoded UTF-8 scalar.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Decoded {
    /// The scalar, or `U+FFFD` when the input was malformed.
    pub code_point: u32,
    /// Bytes consumed. Always at least 1 unless the input was empty.
    pub len: usize,
    /// Whether the sequence was well-formed.
    pub valid: bool,
}

/// Decode one UTF-8 scalar.
///
/// Malformed input yields `U+FFFD` and consumes exactly one byte, so a caller
/// looping on `len` always terminates - including on adversarial tool output.
#[must_use]
pub fn decode(bytes: &[u8]) -> Decoded {
    let mut code_point: u32 = 0;
    let mut len: usize = 0;

    // SAFETY: `bytes.as_ptr()` is valid for `bytes.len()` bytes, and both output
    // pointers reference live locals. The callee reads no more than `len` bytes
    // and writes exactly one value through each pointer.
    let valid =
        unsafe { sys::supra_utf8_decode(bytes.as_ptr(), bytes.len(), &raw mut code_point, &raw mut len) };

    Decoded { code_point, len, valid: valid == 1 }
}

/// Bytes in the UTF-8 encoding of `code_point`, or `None` if it is not a scalar.
#[must_use]
pub fn encoded_len(code_point: u32) -> Option<usize> {
    // SAFETY: takes a value, returns a value, touches no memory.
    let len = unsafe { sys::supra_utf8_encoded_len(code_point) };
    (len > 0).then_some(len)
}

/// Cells occupied by one code point in isolation.
///
/// Not sufficient for measuring text: an emoji ZWJ sequence, a regional indicator
/// pair, and a base plus variation selector each span several code points and
/// occupy fewer cells than the per-code-point sum. Use [`measure`] for text.
#[must_use]
pub fn char_width(code_point: u32, ambiguous: Ambiguous) -> Width {
    // SAFETY: value in, value out, no memory access.
    Width::from_raw(unsafe { sys::supra_wcwidth(code_point, ambiguous.raw()) })
}

/// Byte length of the grapheme cluster starting at `bytes`, per UAX #29.
///
/// Always at least 1 unless `bytes` is empty.
#[must_use]
pub fn cluster_len(bytes: &[u8]) -> usize {
    // SAFETY: pointer and length describe the same slice; the callee only reads.
    unsafe { sys::supra_grapheme_next(bytes.as_ptr(), bytes.len()) }
}

/// Width and byte length of the grapheme cluster starting at `bytes`.
///
/// A cluster occupies the width of its *widest* constituent, not the sum: a
/// five-code-point family emoji is two cells, and summing would give eight.
#[must_use]
pub fn cluster_width(bytes: &[u8], ambiguous: Ambiguous) -> (Width, usize) {
    let mut len: usize = 0;
    // SAFETY: slice pointer and length agree; `out_len` references a live local.
    let raw =
        unsafe { sys::supra_grapheme_width(bytes.as_ptr(), bytes.len(), ambiguous.raw(), &raw mut len) };
    (Width::from_raw(raw), len)
}

/// Cells occupied by a UTF-8 string.
///
/// Non-printable code points contribute nothing: this reports how much room text
/// needs, and a control character needs none.
#[must_use]
pub fn width(bytes: &[u8], ambiguous: Ambiguous) -> usize {
    // SAFETY: slice pointer and length agree; the callee only reads.
    unsafe { sys::supra_wcswidth(bytes.as_ptr(), bytes.len(), ambiguous.raw()) }
}

/// Width and grapheme cluster count in one pass.
///
/// Both figures together, because a renderer needs width to fit a line and
/// cluster count to place a cursor, and measuring twice doubles the cost on the
/// render path.
#[must_use]
pub fn measure(bytes: &[u8], ambiguous: Ambiguous) -> (usize, usize) {
    let mut clusters: usize = 0;
    // SAFETY: slice pointer and length agree; `out_clusters` is a live local.
    let cells =
        unsafe { sys::supra_width_measure(bytes.as_ptr(), bytes.len(), ambiguous.raw(), &raw mut clusters) };
    (cells, clusters)
}

/// Result of truncating to a cell budget.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Truncated {
    /// Bytes of the input that fit. Ends on a cluster boundary.
    pub bytes: usize,
    /// Cells consumed. May be less than the limit when a two-cell cluster would
    /// have crossed it.
    pub cells: usize,
}

/// Largest byte prefix whose width does not exceed `max_cells`.
///
/// Never splits a grapheme cluster: a two-cell cluster that would cross the limit
/// is excluded whole, because half a wide glyph is a corrupted cell rather than a
/// narrow glyph.
///
/// Escape-unaware by design. Cutting inside an SGR sequence leaks escape bytes to
/// the terminal, so styled text must go through [`crate::ansi::plan_truncate`].
#[must_use]
pub fn truncate(bytes: &[u8], max_cells: usize, ambiguous: Ambiguous) -> Truncated {
    let mut cells: usize = 0;
    // SAFETY: slice pointer and length agree; `out_cells` is a live local.
    let taken = unsafe {
        sys::supra_width_truncate(bytes.as_ptr(), bytes.len(), max_cells, ambiguous.raw(), &raw mut cells)
    };
    Truncated { bytes: taken, cells }
}

/// Why a string is unsuitable for display.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Invalid {
    /// Malformed UTF-8: overlong, truncated, surrogate, or out of range.
    Utf8 {
        /// Byte offset of the first malformed sequence.
        offset: usize,
    },
    /// Well-formed UTF-8 containing a control or unassigned code point.
    NonPrintable {
        /// Byte offset of the first such code point.
        offset: usize,
    },
}

/// Classify a string without measuring it.
///
/// The two failures are distinguished because the remedy differs: malformed bytes
/// must be replaced, whereas a control character can be stripped.
///
/// # Errors
///
/// [`Invalid::Utf8`] when the input is malformed, [`Invalid::NonPrintable`] when
/// it is well-formed but carries a control or unassigned code point. A verdict
/// the ABI does not document reports as `Utf8`, the fail-safe default.
#[allow(clippy::match_same_arms)]
pub fn validate(bytes: &[u8]) -> Result<(), Invalid> {
    let mut offset: usize = 0;
    // SAFETY: slice pointer and length agree; `out_offset` is a live local.
    let verdict = unsafe { sys::supra_width_validate(bytes.as_ptr(), bytes.len(), &raw mut offset) };

    // The INVALID_UTF8 arm and the wildcard share a body on purpose: the explicit
    // arm documents the ABI's own constant, the wildcard is the fail-safe for a
    // value it does not document, and both report malformed.
    match verdict {
        sys::VALIDITY_VALID => Ok(()),
        sys::VALIDITY_INVALID_UTF8 => Err(Invalid::Utf8 { offset }),
        sys::VALIDITY_HAS_NONPRINTABLE => Err(Invalid::NonPrintable { offset }),
        // Unreachable per the ABI. Reported as malformed rather than accepted,
        // since treating an unknown verdict as valid would be the unsafe default.
        _ => Err(Invalid::Utf8 { offset }),
    }
}

// A single grapheme cluster cannot approach this, so the probe buffer never
// truncates in practice; the bound is here so the copy is provably safe.
const PROBE_MAX: usize = 64;

/// Measure a single glyph, for the startup width probe.
///
/// Returns `None` for anything that is not exactly one printable grapheme
/// cluster. The TUI compares the result against `One` for every glyph in its
/// active set and falls back to an ASCII tier if any differs - which is how an
/// ambiguous-width glyph is caught before it corrupts a layout rather than after.
#[must_use]
pub fn probe(glyph: &str, ambiguous: Ambiguous) -> Option<Width> {
    // The C ABI takes a NUL-terminated string. Rather than allocate, reject a
    // glyph containing an interior NUL: such input cannot be a single printable
    // cluster anyway, so the answer is the same.
    if glyph.is_empty() || glyph.as_bytes().contains(&0) {
        return None;
    }

    if glyph.len() >= PROBE_MAX {
        return None;
    }

    let mut buffer = [0_u8; PROBE_MAX];
    buffer[..glyph.len()].copy_from_slice(glyph.as_bytes());

    // SAFETY: `buffer` is NUL-terminated - it was zeroed and `glyph.len() < PROBE_MAX`,
    // so at least one trailing zero remains - and the callee only reads up to
    // that terminator.
    let raw = unsafe { sys::supra_width_probe(buffer.as_ptr().cast(), ambiguous.raw()) };

    match Width::from_raw(raw) {
        Width::NonPrintable => None,
        other => Some(other),
    }
}

/// Unicode version the tables were generated from, for example `"17.0.0"`.
///
/// Surfaced by `supra --version` so a width discrepancy can be attributed to a
/// table version rather than guessed at.
#[must_use]
pub fn unicode_version() -> &'static str {
    // SAFETY: the callee returns a pointer to a static NUL-terminated string that
    // is never freed, so a 'static borrow is sound.
    let raw = unsafe { sys::supra_width_unicode_version() };
    if raw.is_null() {
        return "unknown";
    }
    // SAFETY: non-null, NUL-terminated, static storage as documented by the ABI.
    unsafe { CStr::from_ptr(raw) }.to_str().unwrap_or("unknown")
}

/// Iterator over grapheme clusters, yielding each cluster and its width.
///
/// Borrowed rather than allocating, so a renderer can walk a line without
/// touching the heap.
#[derive(Clone, Debug)]
pub struct Clusters<'a> {
    rest: &'a [u8],
    ambiguous: Ambiguous,
}

impl<'a> Clusters<'a> {
    /// Start iterating over `bytes`.
    #[must_use]
    pub const fn new(bytes: &'a [u8], ambiguous: Ambiguous) -> Self {
        Self { rest: bytes, ambiguous }
    }
}

/// One grapheme cluster.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cluster<'a> {
    /// The cluster's bytes.
    pub bytes: &'a [u8],
    /// Its display width.
    pub width: Width,
}

impl<'a> Iterator for Clusters<'a> {
    type Item = Cluster<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.rest.is_empty() {
            return None;
        }

        let (width, len) = cluster_width(self.rest, self.ambiguous);
        // The ABI guarantees at least 1 for a non-empty slice. Stopping rather
        // than looping forever keeps a library bug from hanging the renderer.
        if len == 0 || len > self.rest.len() {
            self.rest = &[];
            return None;
        }

        let (cluster, rest) = self.rest.split_at(len);
        self.rest = rest;
        Some(Cluster { bytes: cluster, width })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ambiguous_is_a_parameter_not_a_constant() {
        // The reason this crate exposes Ambiguous at all: the same code point has
        // two correct answers, and picking one at compile time corrupts layout
        // for whoever gets the other.
        assert_eq!(char_width(0x2588, Ambiguous::Narrow), Width::One);
        assert_eq!(char_width(0x2588, Ambiguous::Wide), Width::Two);

        // And the trap recorded in T2: the block range is not uniform. U+2591 is
        // Neutral, so pairing it with U+2588 for a gauge mixes width classes.
        assert_eq!(char_width(0x2591, Ambiguous::Narrow), Width::One);
        assert_eq!(char_width(0x2591, Ambiguous::Wide), Width::One);
    }

    #[test]
    fn non_printable_is_distinct_from_zero_width() {
        // A caller sanitising output must tell "renders as nothing" from "must not
        // be sent", which an integer return would flatten.
        assert_eq!(char_width(0x07, Ambiguous::Narrow), Width::NonPrintable);
        assert_eq!(char_width(0x0300, Ambiguous::Narrow), Width::Zero);
        assert_eq!(Width::NonPrintable.cells(), 0);
        assert_eq!(Width::Zero.cells(), 0);
    }

    #[test]
    fn decode_always_advances() {
        // The property every scan loop depends on. Exhaustive over single bytes,
        // because one exception is a hang rather than a wrong answer.
        for byte in 0..=u8::MAX {
            let decoded = decode(&[byte]);
            assert_eq!(decoded.len, 1, "byte {byte:#04x} must consume exactly 1");
        }

        let empty = decode(&[]);
        assert_eq!(empty.len, 0);
        assert!(!empty.valid);
        assert_eq!(empty.code_point, sys::WIDTH_REPLACEMENT);
    }

    #[test]
    fn clusters_iterate_without_allocating() {
        // Family emoji: five code points, one cluster, two cells.
        let family = "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}";
        let clusters: Vec<_> = Clusters::new(family.as_bytes(), Ambiguous::Narrow).collect();
        assert_eq!(clusters.len(), 1);
        assert_eq!(clusters[0].width, Width::Two);
        assert_eq!(clusters[0].bytes, family.as_bytes());

        let mixed = "a\u{4E2D}b";
        let widths: Vec<_> = Clusters::new(mixed.as_bytes(), Ambiguous::Narrow).map(|c| c.width).collect();
        assert_eq!(widths, vec![Width::One, Width::Two, Width::One]);
    }

    #[test]
    fn clusters_cover_the_input_exactly() {
        let input = "a\u{301}\u{4E2D}\u{1F600}z";
        let rejoined: Vec<u8> = Clusters::new(input.as_bytes(), Ambiguous::Narrow)
            .flat_map(|c| c.bytes.iter().copied())
            .collect();
        assert_eq!(rejoined, input.as_bytes());
    }

    #[test]
    fn truncate_never_exceeds_the_limit() {
        let inputs = ["hello", "\u{4E2D}\u{6587}", "\u{1F600}\u{1F601}", "a\u{301}e\u{302}"];
        for input in inputs {
            for limit in 0..=12 {
                for ambiguous in [Ambiguous::Narrow, Ambiguous::Wide] {
                    let result = truncate(input.as_bytes(), limit, ambiguous);
                    assert!(result.cells <= limit, "{input:?} at limit {limit}");
                    // The reported width must match what the prefix measures, or a
                    // caller padding to the limit computes the wrong padding.
                    let remeasured = width(&input.as_bytes()[..result.bytes], ambiguous);
                    assert_eq!(remeasured, result.cells, "{input:?} at limit {limit}");
                }
            }
        }
    }

    #[test]
    fn validate_distinguishes_the_two_failures() {
        assert_eq!(validate(b"hello"), Ok(()));
        assert_eq!(validate(b"ab\xFFcd"), Err(Invalid::Utf8 { offset: 2 }));
        assert_eq!(validate(b"ab\x07cd"), Err(Invalid::NonPrintable { offset: 2 }));
        assert_eq!(validate(&[]), Ok(()));
    }

    #[test]
    fn probe_rejects_anything_but_one_printable_cluster() {
        assert_eq!(probe("a", Ambiguous::Narrow), Some(Width::One));
        assert_eq!(probe("\u{4E2D}", Ambiguous::Narrow), Some(Width::Two));

        // Braille is Neutral in every locale, which is why the spinner uses it.
        assert_eq!(probe("\u{280B}", Ambiguous::Narrow), Some(Width::One));
        assert_eq!(probe("\u{280B}", Ambiguous::Wide), Some(Width::One));

        // The thinking-block glyphs are Ambiguous - the signal the TUI acts on.
        assert_eq!(probe("\u{2235}", Ambiguous::Narrow), Some(Width::One));
        assert_eq!(probe("\u{2235}", Ambiguous::Wide), Some(Width::Two));

        assert_eq!(probe("", Ambiguous::Narrow), None);
        assert_eq!(probe("ab", Ambiguous::Narrow), None, "two clusters");
        assert_eq!(probe("\u{7}", Ambiguous::Narrow), None, "control");
        assert_eq!(probe("a\0b", Ambiguous::Narrow), None, "interior NUL");
    }

    #[test]
    fn encoded_len_rejects_non_scalars() {
        assert_eq!(encoded_len(0x41), Some(1));
        assert_eq!(encoded_len(0x10_FFFF), Some(4));
        assert_eq!(encoded_len(0xD800), None, "surrogate");
        assert_eq!(encoded_len(0x11_0000), None, "out of range");
    }

    #[test]
    fn unicode_version_is_reported() {
        let version = unicode_version();
        assert!(version.starts_with("17."), "got {version}");
    }

    #[test]
    fn empty_input_is_handled_everywhere() {
        // A slice with no bytes has a dangling-but-aligned pointer, which is
        // exactly the case a hand-written binding gets wrong.
        assert_eq!(width(&[], Ambiguous::Narrow), 0);
        assert_eq!(measure(&[], Ambiguous::Narrow), (0, 0));
        assert_eq!(cluster_len(&[]), 0);
        assert_eq!(cluster_width(&[], Ambiguous::Narrow).0, Width::NonPrintable);
        assert_eq!(truncate(&[], 10, Ambiguous::Narrow), Truncated { bytes: 0, cells: 0 });
        assert_eq!(Clusters::new(&[], Ambiguous::Narrow).count(), 0);
    }
}
