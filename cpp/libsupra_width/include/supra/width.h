/// libsupra_width - terminal cell width and grapheme segmentation.
///
/// The sole arbiter of every layout decision in the supra TUI. If a component
/// computes a width any other way, that component is wrong.
///
/// ## Contract
///
/// Flat C ABI. Every function is `noexcept` by construction: the library is
/// compiled with `-fno-exceptions`, because an exception unwinding into Rust is
/// undefined behaviour, so the machinery is removed rather than merely unused.
///
/// No function allocates. No function retains a pointer past return. Every
/// function is reentrant and thread-safe: all state is in immutable static
/// tables.
///
/// Invalid input never traps. Malformed UTF-8 yields U+FFFD and advances one
/// byte, which guarantees forward progress on any byte sequence - including
/// adversarial tool output. A width function that aborts would take down a
/// session that had been running for hours.
///
/// ## Why a separate library
///
/// Nearly every glyph a terminal UI wants - block elements, geometric shapes,
/// the `because`/`therefore` pair - is East Asian **Ambiguous**: one cell in a
/// Latin locale, two in a CJK locale. Baking in either answer produces layout
/// corruption for half the world's terminals, so ambiguity is resolved from an
/// explicit flag the caller supplies. `supra_width_probe` exists so the TUI can
/// measure its own glyph set at startup and fall back to ASCII when a glyph
/// does not measure one cell.
///
/// Unicode 17.0.0. Tables are generated from the vendored UCD extracts under
/// `data/`; see `tools/gen_width_tables.py`.

#ifndef SUPRA_WIDTH_H
#define SUPRA_WIDTH_H

#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

/* ------------------------------------------------------------------------- */
/* Constants                                                                 */
/* ------------------------------------------------------------------------- */

/// Replacement character substituted for malformed input.
#define SUPRA_WIDTH_REPLACEMENT 0xFFFDu

/// Returned by width functions for a code point that occupies no cell and
/// should not be emitted: C0/C1 controls, unpaired surrogates, unassigned
/// planes. Distinct from a legitimate zero width (combining marks), because the
/// caller must be able to tell "renders as nothing" from "must not be sent".
#define SUPRA_WIDTH_NONPRINTABLE (-1)

/* ------------------------------------------------------------------------- */
/* Ambiguous-width resolution                                                */
/* ------------------------------------------------------------------------- */

/// How to resolve East Asian Ambiguous code points.
///
/// This is a property of the terminal and its locale, not of the text. The
/// caller decides once at startup and passes the same value everywhere;
/// mixing values within one frame produces inconsistent layout.
typedef enum supra_width_ambiguous {
    /// One cell. Correct for Latin locales and the common default.
    SUPRA_WIDTH_AMBIGUOUS_NARROW = 0,
    /// Two cells. Correct under a CJK locale or with a CJK-wide font.
    SUPRA_WIDTH_AMBIGUOUS_WIDE = 1
} supra_width_ambiguous;

/* ------------------------------------------------------------------------- */
/* UTF-8 decoding                                                            */
/* ------------------------------------------------------------------------- */

/// Decode one UTF-8 scalar.
///
/// Rejects overlong encodings, surrogate halves (U+D800..U+DFFF), and values
/// above U+10FFFF. On any malformed sequence, writes
/// `SUPRA_WIDTH_REPLACEMENT` and consumes exactly one byte, so a caller looping
/// on `*consumed` always terminates.
///
/// @param bytes    UTF-8 input; may be NULL only when `len` is 0.
/// @param len      Bytes available at `bytes`.
/// @param out_cp   Receives the scalar, or U+FFFD. Must not be NULL.
/// @param consumed Receives bytes consumed, always >= 1 unless `len` is 0.
///                 Must not be NULL.
/// @return 1 on a well-formed sequence, 0 when the result was substituted.
int supra_utf8_decode(const uint8_t* bytes, size_t len, uint32_t* out_cp, size_t* consumed);

/// Bytes in the UTF-8 encoding of `cp`, or 0 if `cp` is not a valid scalar.
size_t supra_utf8_encoded_len(uint32_t cp);

/* ------------------------------------------------------------------------- */
/* Code point width                                                          */
/* ------------------------------------------------------------------------- */

/// Cells occupied by one code point in isolation.
///
/// @return 2 for East Asian Wide/Fullwidth and default-emoji-presentation
///         code points; 0 for combining marks, format characters, and
///         Extend-class code points; `SUPRA_WIDTH_NONPRINTABLE` for controls,
///         surrogates, and unassigned code points; otherwise 1.
///
/// Not sufficient for measuring text. An emoji ZWJ sequence, a regional
/// indicator pair, and a base plus variation selector all span several code
/// points and occupy fewer cells than the per-code-point sum. Use
/// `supra_width_measure` for text; this function exists for single-glyph
/// decisions such as `supra_width_probe`.
int supra_wcwidth(uint32_t cp, supra_width_ambiguous ambiguous);

/* ------------------------------------------------------------------------- */
/* Grapheme cluster segmentation                                             */
/* ------------------------------------------------------------------------- */

/// Byte length of the grapheme cluster starting at `bytes`, per UAX #29.
///
/// Implements the extended grapheme cluster rules, including GB9c (Indic
/// conjunct break), GB11 (emoji ZWJ sequences), and GB12/GB13 (regional
/// indicator pairing).
///
/// @return Bytes in the cluster, always >= 1 unless `len` is 0.
size_t supra_grapheme_next(const uint8_t* bytes, size_t len);

/// Cells occupied by the grapheme cluster starting at `bytes`.
///
/// A cluster occupies the width of its widest constituent, not the sum: an
/// emoji ZWJ sequence of four code points is two cells, and a base plus
/// combining mark is the width of the base.
///
/// @param out_len Optional; receives the cluster's byte length.
int supra_grapheme_width(const uint8_t* bytes, size_t len, supra_width_ambiguous ambiguous,
                         size_t* out_len);

/* ------------------------------------------------------------------------- */
/* String measurement                                                        */
/* ------------------------------------------------------------------------- */

/// Cells occupied by a UTF-8 string, segmented into grapheme clusters.
///
/// Non-printable code points contribute 0 rather than propagating -1: this
/// measures how much room text needs, and a control character needs none.
/// Use `supra_width_validate` to reject text containing them.
size_t supra_wcswidth(const uint8_t* bytes, size_t len, supra_width_ambiguous ambiguous);

/// Measure a string and report its cluster count in one pass.
///
/// The TUI needs both figures together - width to fit a line, cluster count to
/// place a cursor - and measuring twice doubles the cost on the render path.
///
/// @param out_clusters Optional; receives the number of grapheme clusters.
size_t supra_width_measure(const uint8_t* bytes, size_t len, supra_width_ambiguous ambiguous,
                           size_t* out_clusters);

/* ------------------------------------------------------------------------- */
/* Truncation                                                                */
/* ------------------------------------------------------------------------- */

/// Largest byte prefix of `bytes` whose width does not exceed `max_cells`.
///
/// Never splits a grapheme cluster. When a two-cell cluster would cross the
/// limit it is excluded entirely, so the result may measure `max_cells - 1`.
///
/// Escape-sequence-unaware by design: cutting inside an SGR sequence leaks
/// escape bytes to the terminal. Text carrying escapes must be truncated with
/// `supra_ansi_truncate` from libsupra_ansi (T3), which preserves and reissues
/// the active style.
///
/// @param out_cells Optional; receives the width actually consumed.
size_t supra_width_truncate(const uint8_t* bytes, size_t len, size_t max_cells,
                            supra_width_ambiguous ambiguous, size_t* out_cells);

/* ------------------------------------------------------------------------- */
/* Validation and probing                                                    */
/* ------------------------------------------------------------------------- */

/// Result of `supra_width_validate`.
typedef enum supra_width_validity {
    SUPRA_WIDTH_VALID = 0,
    /// Malformed UTF-8: overlong, truncated, surrogate, or out of range.
    SUPRA_WIDTH_INVALID_UTF8 = 1,
    /// Well-formed UTF-8 containing a control or unassigned code point.
    SUPRA_WIDTH_HAS_NONPRINTABLE = 2
} supra_width_validity;

/// Classify a string without measuring it.
///
/// @param out_offset Optional; receives the byte offset of the first problem.
supra_width_validity supra_width_validate(const uint8_t* bytes, size_t len, size_t* out_offset);

/// Measure a single glyph for the startup width probe.
///
/// Returns the cells the glyph occupies under `ambiguous`. The TUI compares the
/// result against 1 for every glyph in its active set and falls back to an
/// ASCII tier if any differs, which is how an ambiguous-width glyph is caught
/// before it corrupts a layout rather than after.
///
/// @param utf8 NUL-terminated UTF-8 for exactly one grapheme cluster.
/// @return Cells occupied, or `SUPRA_WIDTH_NONPRINTABLE` if `utf8` is NULL,
///         empty, malformed, or spans more than one cluster.
int supra_width_probe(const char* utf8, supra_width_ambiguous ambiguous);

/* ------------------------------------------------------------------------- */
/* Provenance                                                               */
/* ------------------------------------------------------------------------- */

/// Unicode version the tables were generated from, e.g. "17.0.0".
///
/// Static storage; never freed. Surfaced by `supra --version` so a width
/// discrepancy can be attributed to a table version.
const char* supra_width_unicode_version(void);

#ifdef __cplusplus
}  /* extern "C" */
#endif

#endif /* SUPRA_WIDTH_H */
