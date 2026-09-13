/// libsupra_ansi - escape sequence parsing, SGR state, and style-safe
/// truncation.
///
/// ## Contract
///
/// Flat C ABI. Every function is `noexcept` by construction: the library is
/// compiled with `-fno-exceptions`, because an exception unwinding into Rust is
/// undefined behaviour, so the machinery is removed rather than merely unused.
///
/// No function allocates. No function retains a pointer past return. Every
/// function is reentrant: all mutable state is caller-owned and passed by
/// pointer.
///
/// Malformed input never traps and never stalls. Every scan advances at least
/// one byte, so a caller looping on the result terminates on any byte sequence -
/// including adversarial tool output.
///
/// ## Why not a regex
///
/// The usual approach is `/\x1b\[[0-9;]*m/`. It is wrong on all of the
/// following, each of which occurs in real terminal output:
///
/// * **OSC terminators.** Two are legal: `ESC \` (ST) and `BEL`. A parser that
///   knows only one runs past the end of a hyperlink or title sequence and eats
///   the text after it.
/// * **Sub-parameters.** `ESC [ 4:3 m` (curly underline) and
///   `ESC [ 38:2::255:0:0 m` (RGB) separate arguments with `:`, not `;`.
/// * **8-bit C1 controls.** `0x9B` is CSI and `0x9D` is OSC, with no ESC byte
///   at all.
/// * **DCS and APC strings.** Terminated like OSC, and their payloads may
///   contain bytes that look like the start of other sequences.
/// * **Split sequences.** Shell output arrives in chunks, and an escape
///   sequence can straddle a chunk boundary. A stateless matcher sees two
///   fragments and mangles both.
///
/// This is a state machine derived from the DEC STD 070 / VT500 parser, which
/// handles all of the above by construction.
///
/// ## Truncation
///
/// `supra_ansi_plan_truncate` is the function this library exists for. It
/// produces output that is always **style-balanced**: any styling opened before
/// the cut is closed at the cut, and no partial escape sequence is ever emitted.
///
/// Cutting inside an escape sequence sends the terminal a fragment it will
/// interpret as a command over whatever follows, corrupting the rest of the
/// screen rather than just one line. Leaving styling open bleeds colour into
/// unrelated output. Both faults persist until the next full repaint, which is
/// why this is enforced here rather than left to callers.
///
/// Cell widths come from libsupra_width (T2); this library never decides a
/// width itself.

#ifndef SUPRA_ANSI_H
#define SUPRA_ANSI_H

#include <stddef.h>
#include <stdint.h>

#include "supra/width.h"

#ifdef __cplusplus
extern "C" {
#endif

/* ------------------------------------------------------------------------- */
/* Limits                                                                    */
/* ------------------------------------------------------------------------- */

/// Maximum CSI/DCS parameters retained. Beyond this the sequence is still
/// consumed and skipped correctly, but its parameters are not reported.
/// DEC STD 070 specifies 16; anything longer is malformed in practice.
#define SUPRA_ANSI_MAX_PARAMS 16

/// Maximum sub-parameters per parameter, for `38:2::r:g:b` style arguments.
#define SUPRA_ANSI_MAX_SUBPARAMS 8

/// Maximum bytes of an OSC or DCS payload reported to the caller. Longer
/// payloads are consumed correctly but reported truncated, with `payload_full`
/// carrying the real length.
#define SUPRA_ANSI_MAX_PAYLOAD 256

/// Upper bound on the trailing bytes a truncation plan can require: an SGR
/// reset plus a hyperlink close.
#define SUPRA_ANSI_MAX_TRAILER 24

/* ------------------------------------------------------------------------- */
/* Colour                                                                    */
/* ------------------------------------------------------------------------- */

typedef enum supra_ansi_color_kind {
    /// Terminal default. Emitted as SGR 39 / 49.
    SUPRA_ANSI_COLOR_DEFAULT = 0,
    /// Palette index 0-255, in `index`.
    SUPRA_ANSI_COLOR_INDEXED = 1,
    /// Direct colour, in `r`, `g`, `b`.
    SUPRA_ANSI_COLOR_RGB = 2
} supra_ansi_color_kind;

typedef struct supra_ansi_color {
    uint8_t kind; /**< supra_ansi_color_kind */
    uint8_t index;
    uint8_t r;
    uint8_t g;
    uint8_t b;
} supra_ansi_color;

/* ------------------------------------------------------------------------- */
/* Style                                                                     */
/* ------------------------------------------------------------------------- */

/// Attribute bits, matching their SGR parameter where one exists.
typedef enum supra_ansi_attr {
    SUPRA_ANSI_ATTR_BOLD = 1u << 0,
    SUPRA_ANSI_ATTR_DIM = 1u << 1,
    SUPRA_ANSI_ATTR_ITALIC = 1u << 2,
    SUPRA_ANSI_ATTR_BLINK = 1u << 3,
    SUPRA_ANSI_ATTR_INVERSE = 1u << 4,
    SUPRA_ANSI_ATTR_HIDDEN = 1u << 5,
    SUPRA_ANSI_ATTR_STRIKE = 1u << 6,
    SUPRA_ANSI_ATTR_OVERLINE = 1u << 7
} supra_ansi_attr;

/// Underline style. Separate from the attribute bits because SGR 4 takes a
/// sub-parameter (`ESC [ 4:3 m` for curly), and because SGR 21 means double
/// underline on terminals that implement it.
typedef enum supra_ansi_underline {
    SUPRA_ANSI_UNDERLINE_NONE = 0,
    SUPRA_ANSI_UNDERLINE_SINGLE = 1,
    SUPRA_ANSI_UNDERLINE_DOUBLE = 2,
    SUPRA_ANSI_UNDERLINE_CURLY = 3,
    SUPRA_ANSI_UNDERLINE_DOTTED = 4,
    SUPRA_ANSI_UNDERLINE_DASHED = 5
} supra_ansi_underline;

/// Complete SGR state. Fixed size, trivially copyable, no allocation.
///
/// `hyperlink_open` tracks OSC 8: an unterminated hyperlink makes every
/// subsequent cell clickable, so truncation has to close it.
typedef struct supra_ansi_style {
    uint32_t attrs; /**< bitwise OR of supra_ansi_attr */
    supra_ansi_color fg;
    supra_ansi_color bg;
    supra_ansi_color underline_color;
    uint8_t underline; /**< supra_ansi_underline */
    uint8_t hyperlink_open;
} supra_ansi_style;

/// The default style: no attributes, default colours, no hyperlink.
supra_ansi_style supra_ansi_style_default(void);

/// True when `style` differs from the default and therefore needs a reset.
int supra_ansi_style_is_default(const supra_ansi_style* style);

/// Serialise `style` as a single SGR sequence.
///
/// Emits the shortest correct form: a leading reset then only the parameters
/// that differ from the default. Emits nothing for a default style, because on
/// the render path a spurious 4-byte reset per line is measurable waste.
///
/// @return Bytes needed, excluding any NUL. When the return value exceeds
///         `cap`, nothing was written; retry with a larger buffer.
size_t supra_ansi_style_emit(const supra_ansi_style* style, uint8_t* out, size_t cap);

/// Emit the sequence that returns the terminal from `style` to default.
///
/// `ESC [ 0 m` plus an OSC 8 close when a hyperlink is open. Empty for a
/// default style.
size_t supra_ansi_style_emit_reset(const supra_ansi_style* style, uint8_t* out, size_t cap);

/* ------------------------------------------------------------------------- */
/* Tokens                                                                    */
/* ------------------------------------------------------------------------- */

typedef enum supra_ansi_token_kind {
    /// A run of printable bytes. Contributes cells.
    SUPRA_ANSI_TOKEN_TEXT = 0,
    /// A C0 or C1 control that is not part of a longer sequence.
    SUPRA_ANSI_TOKEN_CONTROL = 1,
    /// A complete CSI sequence. `final_byte`, `params`, and `intermediates`
    /// are populated.
    SUPRA_ANSI_TOKEN_CSI = 2,
    /// A complete two-or-three byte escape sequence such as `ESC ( B`.
    SUPRA_ANSI_TOKEN_ESC = 3,
    /// A complete OSC string. `payload` holds the body without introducer or
    /// terminator.
    SUPRA_ANSI_TOKEN_OSC = 4,
    /// A complete DCS string.
    SUPRA_ANSI_TOKEN_DCS = 5,
    /// A complete SOS, PM, or APC string. Content is not interpreted.
    SUPRA_ANSI_TOKEN_APC = 6,
    /// Input ended mid-sequence. `length` covers the bytes consumed so far;
    /// the caller must carry them into the next chunk. Never emitted with
    /// `SUPRA_ANSI_FINAL`.
    SUPRA_ANSI_TOKEN_PARTIAL = 7,
    /// A sequence that violated the grammar. Consumed and reported so a caller
    /// can log it; safe to ignore.
    SUPRA_ANSI_TOKEN_MALFORMED = 8
} supra_ansi_token_kind;

typedef struct supra_ansi_token {
    uint8_t kind; /**< supra_ansi_token_kind */

    /// Byte offset of this token within the buffer passed to the scanner.
    size_t offset;
    /// Bytes consumed. Always >= 1 unless the buffer was empty.
    size_t length;

    /// For CONTROL: the control byte. For CSI/ESC/DCS: the final byte.
    uint8_t final_byte;

    /// Private-mode and intermediate bytes, e.g. the `?` of `ESC [ ? 25 h`.
    uint8_t intermediates[2];
    uint8_t intermediate_count;

    /// Numeric parameters. A parameter omitted by the sender is reported as
    /// `-1`, which is distinct from an explicit `0` and changes the meaning of
    /// several sequences.
    int32_t params[SUPRA_ANSI_MAX_PARAMS];
    uint8_t param_count;
    /// Parameters present beyond SUPRA_ANSI_MAX_PARAMS.
    uint8_t params_dropped;

    /// Sub-parameters of `params[0..param_count]`, flattened. `subparam_index`
    /// gives the first slot for parameter i and `subparam_count` how many.
    int32_t subparams[SUPRA_ANSI_MAX_SUBPARAMS];
    uint8_t subparam_index[SUPRA_ANSI_MAX_PARAMS];
    uint8_t subparam_count[SUPRA_ANSI_MAX_PARAMS];

    /// OSC/DCS/APC body, without introducer or terminator.
    uint8_t payload[SUPRA_ANSI_MAX_PAYLOAD];
    uint16_t payload_len;
    /// Real payload length, which may exceed `payload_len`.
    size_t payload_full;
} supra_ansi_token;

/* ------------------------------------------------------------------------- */
/* Scanner                                                                   */
/* ------------------------------------------------------------------------- */

/// Resumable scanner state. Caller-allocated; zero-initialise to start.
///
/// Resumability is not a convenience. Shell output arrives in chunks and an
/// escape sequence can straddle a boundary, so a scanner that cannot suspend
/// mid-sequence will corrupt exactly the output that matters - the progress
/// line, the coloured error - at exactly the moment it is written.
///
/// `pos` is the read cursor within the buffer currently being scanned. It is
/// reset automatically when a buffer is exhausted, so the usage is a plain
/// `while (supra_ansi_scan(...))` loop per chunk.
typedef struct supra_ansi_scanner {
    uint8_t state;
    size_t pos;

    /// Set when the current buffer was scanned to its end mid-sequence.
    ///
    /// The cursor cannot simply be reset at that point: the caller's drain loop
    /// would rescan the same buffer forever, and a following chunk longer than
    /// this one would have its leading bytes skipped by a stale cursor. Deferring
    /// the reset to the next call is also what terminates the loop when a
    /// completed token ends exactly at the last byte.
    uint8_t buffer_done;

    /// Continuation bytes still owed by an in-flight UTF-8 character.
    ///
    /// Needed because the 8-bit C1 control range (0x80..0x9F) is a subset of the
    /// UTF-8 continuation range (0x80..0xBF): the second byte of U+6587 is 0x96,
    /// which is also the C1 code for START OF GUARDED AREA. Tracking the
    /// expectation makes the distinction positional, and keeps it correct when a
    /// chunk boundary lands inside a multi-byte character.
    uint8_t utf8_expect;

    uint8_t intermediates[2];
    uint8_t intermediate_count;

    int32_t params[SUPRA_ANSI_MAX_PARAMS];
    uint8_t param_count;
    uint8_t params_dropped;
    uint8_t param_has_digits;
    /// 1 while the last separator seen was `:`, so digits accumulate into a
    /// sub-parameter rather than the parameter.
    uint8_t in_subparam;

    int32_t subparams[SUPRA_ANSI_MAX_SUBPARAMS];
    uint8_t subparam_total;
    uint8_t subparam_index[SUPRA_ANSI_MAX_PARAMS];
    uint8_t subparam_count[SUPRA_ANSI_MAX_PARAMS];

    uint8_t payload[SUPRA_ANSI_MAX_PAYLOAD];
    uint16_t payload_len;
    size_t payload_full;

    /// Bytes of the in-flight sequence already consumed in earlier calls.
    size_t carried;
} supra_ansi_scanner;

/// Whether this buffer ends the stream.
typedef enum supra_ansi_eof {
    /// More input may follow. A sequence cut by the buffer end is reported as
    /// PARTIAL and its state retained.
    SUPRA_ANSI_MORE = 0,
    /// No more input. An unterminated sequence is reported as MALFORMED rather
    /// than PARTIAL, so a caller cannot wait forever for a terminator that will
    /// never arrive.
    SUPRA_ANSI_FINAL = 1
} supra_ansi_eof;

/// Reset a scanner to its initial state.
void supra_ansi_scanner_init(supra_ansi_scanner* scanner);

/// True when the scanner holds an incomplete sequence.
int supra_ansi_scanner_pending(const supra_ansi_scanner* scanner);

/// Read the next token.
///
/// @param scanner Mutable state; retained across calls for streaming input.
/// @param bytes   Input; may be NULL only when `len` is 0.
/// @param len     Bytes available.
/// @param eof     Whether this buffer ends the stream.
/// @param out     Receives the token. Must not be NULL.
/// @return 1 when a token was produced, 0 when the buffer is exhausted.
int supra_ansi_scan(supra_ansi_scanner* scanner, const uint8_t* bytes, size_t len,
                    supra_ansi_eof eof, supra_ansi_token* out);

/* ------------------------------------------------------------------------- */
/* SGR folding                                                               */
/* ------------------------------------------------------------------------- */

/// Fold an SGR token into `style`.
///
/// Ignores tokens that are not SGR (`final_byte` other than `m`, or any
/// intermediate present), so a caller can pass every token unconditionally.
///
/// @return 1 when `style` changed, 0 otherwise.
int supra_ansi_style_apply(supra_ansi_style* style, const supra_ansi_token* token);

/// Fold an OSC 8 hyperlink token into `style`.
///
/// An OSC 8 with an empty URI closes the hyperlink; anything else opens one.
/// Ignores tokens that are not OSC 8.
///
/// @return 1 when `style` changed, 0 otherwise.
int supra_ansi_style_apply_osc(supra_ansi_style* style, const supra_ansi_token* token);

/* ------------------------------------------------------------------------- */
/* Measurement and stripping                                                 */
/* ------------------------------------------------------------------------- */

/// Cells occupied by `bytes`, ignoring escape sequences.
///
/// Escape sequences are state changes rather than content and contribute
/// nothing. Cell widths come from libsupra_width.
size_t supra_ansi_measure(const uint8_t* bytes, size_t len, supra_width_ambiguous ambiguous);

/// Copy `bytes` with every escape sequence removed.
///
/// C0 controls other than tab and newline are dropped too: they are not
/// content, and passing them through would let tool output move the cursor.
///
/// @return Bytes needed. When it exceeds `cap`, nothing was written; retry with
///         a larger buffer.
size_t supra_ansi_strip(const uint8_t* bytes, size_t len, uint8_t* out, size_t cap);

/* ------------------------------------------------------------------------- */
/* Truncation                                                                */
/* ------------------------------------------------------------------------- */

/// How to truncate a styled line, computed without copying it.
///
/// The caller writes `bytes[0 .. prefix_len]` followed by
/// `trailer[0 .. trailer_len]`. There is no output buffer and no copy, which
/// matters because this runs per line per frame.
typedef struct supra_ansi_truncation {
    /// Bytes of the input to emit. Ends on a grapheme cluster boundary and
    /// never inside an escape sequence.
    size_t prefix_len;
    /// Cells the prefix occupies. May be less than `max_cells` when a two-cell
    /// cluster would have crossed the limit.
    size_t cells;
    /// Bytes to append so the terminal is left in its default state.
    uint8_t trailer[SUPRA_ANSI_MAX_TRAILER];
    uint8_t trailer_len;
    /// 1 when the input was cut, 0 when it fitted whole.
    uint8_t truncated;
    /// Style active at the cut. Useful to a caller that wants to reopen it on
    /// the next line rather than reset.
    supra_ansi_style style_at_cut;
} supra_ansi_truncation;

/// Plan a truncation of `bytes` to at most `max_cells`.
///
/// Guarantees, all test-enforced:
///
/// * `cells <= max_cells` for every input, width, and locale.
/// * The prefix never ends inside an escape sequence, so no fragment is emitted
///   that the terminal would interpret as a command.
/// * The prefix never splits a grapheme cluster.
/// * Prefix plus trailer leaves the terminal in its default state, so styling
///   cannot bleed into unrelated output.
/// * A line with no styling gets an empty trailer, so plain text costs no extra
///   bytes.
void supra_ansi_plan_truncate(const uint8_t* bytes, size_t len, size_t max_cells,
                              supra_width_ambiguous ambiguous, supra_ansi_truncation* out);

/// Plan a truncation that starts from an inherited style.
///
/// For continuation lines, where styling opened on a previous line is still in
/// effect. `initial` may be NULL, which is equivalent to the default style.
void supra_ansi_plan_truncate_from(const uint8_t* bytes, size_t len, size_t max_cells,
                                   supra_width_ambiguous ambiguous,
                                   const supra_ansi_style* initial, supra_ansi_truncation* out);

#ifdef __cplusplus
}  /* extern "C" */
#endif

#endif /* SUPRA_ANSI_H */
