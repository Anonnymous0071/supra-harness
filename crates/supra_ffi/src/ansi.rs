//! Escape sequence parsing, SGR state, and style-safe truncation.
//!
//! Safe surface over `libsupra_ansi` (T3).

use crate::sys;
use crate::width::Ambiguous;

// ---------------------------------------------------------------------------
// Style
// ---------------------------------------------------------------------------

/// A colour.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Color {
    /// Terminal default.
    #[default]
    Default,
    /// Palette index 0-255.
    Indexed(u8),
    /// Direct colour.
    Rgb {
        /// Red channel.
        r: u8,
        /// Green channel.
        g: u8,
        /// Blue channel.
        b: u8,
    },
}

impl Color {
    fn from_raw(raw: sys::supra_ansi_color) -> Self {
        match raw.kind {
            sys::ANSI_COLOR_INDEXED => Self::Indexed(raw.index),
            sys::ANSI_COLOR_RGB => Self::Rgb { r: raw.r, g: raw.g, b: raw.b },
            _ => Self::Default,
        }
    }

    const fn to_raw(self) -> sys::supra_ansi_color {
        match self {
            Self::Default => {
                sys::supra_ansi_color { kind: sys::ANSI_COLOR_DEFAULT, index: 0, r: 0, g: 0, b: 0 }
            }
            Self::Indexed(index) => {
                sys::supra_ansi_color { kind: sys::ANSI_COLOR_INDEXED, index, r: 0, g: 0, b: 0 }
            }
            Self::Rgb { r, g, b } => sys::supra_ansi_color { kind: sys::ANSI_COLOR_RGB, index: 0, r, g, b },
        }
    }
}

/// Underline style.
///
/// Separate from the attribute bits because SGR 4 takes a sub-parameter
/// (`ESC[4:3m` for curly) and SGR 21 means double underline.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Underline {
    /// No underline.
    #[default]
    None,
    /// SGR 4.
    Single,
    /// SGR 21, or `ESC[4:2m`.
    Double,
    /// `ESC[4:3m`.
    Curly,
    /// `ESC[4:4m`.
    Dotted,
    /// `ESC[4:5m`.
    Dashed,
}

impl Underline {
    const fn from_raw(raw: u8) -> Self {
        match raw {
            1 => Self::Single,
            2 => Self::Double,
            3 => Self::Curly,
            4 => Self::Dotted,
            5 => Self::Dashed,
            _ => Self::None,
        }
    }

    const fn to_raw(self) -> u8 {
        match self {
            Self::None => 0,
            Self::Single => 1,
            Self::Double => 2,
            Self::Curly => 3,
            Self::Dotted => 4,
            Self::Dashed => 5,
        }
    }
}

/// Text attribute bits.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Attrs(u32);

impl Attrs {
    /// SGR 1.
    pub const BOLD: Self = Self(1 << 0);
    /// SGR 2. Cleared by SGR 22 along with bold, since they share one off-code.
    pub const DIM: Self = Self(1 << 1);
    /// SGR 3.
    pub const ITALIC: Self = Self(1 << 2);
    /// SGR 5, with rapid blink (SGR 6) folded in: no terminal in use
    /// distinguishes them.
    pub const BLINK: Self = Self(1 << 3);
    /// SGR 7.
    pub const INVERSE: Self = Self(1 << 4);
    /// SGR 8.
    pub const HIDDEN: Self = Self(1 << 5);
    /// SGR 9.
    pub const STRIKE: Self = Self(1 << 6);
    /// SGR 53.
    pub const OVERLINE: Self = Self(1 << 7);

    /// No attributes set.
    #[must_use]
    pub const fn empty() -> Self {
        Self(0)
    }

    /// Whether every bit in `other` is set.
    #[must_use]
    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }

    /// Whether no bits are set.
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }
}

impl core::ops::BitOr for Attrs {
    type Output = Self;
    fn bitor(self, rhs: Self) -> Self {
        Self(self.0 | rhs.0)
    }
}

impl core::ops::BitOrAssign for Attrs {
    fn bitor_assign(&mut self, rhs: Self) {
        self.0 |= rhs.0;
    }
}

/// Complete SGR state, plus hyperlink tracking.
///
/// `hyperlink_open` is part of the style because an unterminated OSC 8 makes every
/// subsequent cell clickable, so truncation has to close it - and SGR reset does
/// not, since only OSC 8 can.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Style {
    /// Attribute bits.
    pub attrs: Attrs,
    /// Foreground colour.
    pub fg: Color,
    /// Background colour.
    pub bg: Color,
    /// Underline colour, a Kitty and VTE extension (SGR 58).
    pub underline_color: Color,
    /// Underline style.
    pub underline: Underline,
    /// Whether an OSC 8 hyperlink is open. Not affected by SGR reset, because only
    /// OSC 8 can close one.
    pub hyperlink_open: bool,
}

impl Default for Style {
    /// The terminal's default state.
    ///
    /// Delegates to the C ABI rather than reconstructing the value here: two
    /// definitions of "default" would be two things to keep in agreement, and
    /// `is_default` compares against the C one.
    fn default() -> Self {
        // SAFETY: takes no arguments and returns a plain value; no memory access.
        Self::from_raw(unsafe { sys::supra_ansi_style_default() })
    }
}

impl Style {
    fn from_raw(raw: sys::supra_ansi_style) -> Self {
        Self {
            attrs: Attrs(raw.attrs),
            fg: Color::from_raw(raw.fg),
            bg: Color::from_raw(raw.bg),
            underline_color: Color::from_raw(raw.underline_color),
            underline: Underline::from_raw(raw.underline),
            hyperlink_open: raw.hyperlink_open != 0,
        }
    }

    const fn to_raw(self) -> sys::supra_ansi_style {
        sys::supra_ansi_style {
            attrs: self.attrs.0,
            fg: self.fg.to_raw(),
            bg: self.bg.to_raw(),
            underline_color: self.underline_color.to_raw(),
            underline: self.underline.to_raw(),
            hyperlink_open: self.hyperlink_open as u8,
        }
    }

    /// Whether this is the terminal's default state.
    #[must_use]
    pub fn is_default(self) -> bool {
        let raw = self.to_raw();
        // SAFETY: `raw` is a live local of the correct layout; the callee reads it
        // and returns an int.
        unsafe { sys::supra_ansi_style_is_default(&raw const raw) == 1 }
    }

    /// Fold an SGR or OSC 8 token into this style.
    ///
    /// Tokens that are neither are ignored, so a caller can pass every token
    /// unconditionally without filtering.
    ///
    /// Returns whether the style changed.
    pub fn apply(&mut self, token: &Token) -> bool {
        let mut raw = self.to_raw();
        // SAFETY: both pointers reference live locals with the correct layout.
        let sgr = unsafe { sys::supra_ansi_style_apply(&raw mut raw, &raw const token.raw) };
        // SAFETY: as above.
        let osc = unsafe { sys::supra_ansi_style_apply_osc(&raw mut raw, &raw const token.raw) };
        *self = Self::from_raw(raw);
        sgr == 1 || osc == 1
    }

    /// Serialise as a single SGR sequence, appending to `out`.
    ///
    /// Emits nothing for a default style: a spurious four-byte reset per line is
    /// measurable waste on the render path.
    pub fn emit(self, out: &mut Vec<u8>) {
        self.emit_with(out, |style, buffer, cap| {
            // SAFETY: `style` is a live local; `buffer` is valid for `cap` bytes,
            // and the callee writes at most `cap`.
            unsafe { sys::supra_ansi_style_emit(style, buffer, cap) }
        });
    }

    /// Append the sequence returning the terminal from this style to default.
    ///
    /// Closes an open hyperlink before resetting SGR, because a link left open
    /// while its styling disappears is worse than either alone.
    pub fn emit_reset(self, out: &mut Vec<u8>) {
        self.emit_with(out, |style, buffer, cap| {
            // SAFETY: as in `emit`.
            unsafe { sys::supra_ansi_style_emit_reset(style, buffer, cap) }
        });
    }

    /// Shared two-phase write: ask for the length, reserve, then fill.
    fn emit_with(
        self,
        out: &mut Vec<u8>,
        emit: impl Fn(*const sys::supra_ansi_style, *mut u8, usize) -> usize,
    ) {
        let raw = self.to_raw();

        // Phase one with a null buffer, which the ABI answers with the required
        // length. A caller on the render path allocates exactly once.
        let needed = emit(&raw const raw, core::ptr::null_mut(), 0);
        if needed == 0 {
            return;
        }

        let start = out.len();
        out.resize(start + needed, 0);
        let written = emit(&raw const raw, out[start..].as_mut_ptr(), needed);

        // The ABI reports a stable length between the two calls. Truncating to
        // what was actually written keeps a library bug from leaving zero bytes
        // in the buffer, which the terminal would render as NULs.
        out.truncate(start + written.min(needed));
    }
}

// ---------------------------------------------------------------------------
// Tokens
// ---------------------------------------------------------------------------

/// What a token is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TokenKind {
    /// A run of printable bytes. Contributes cells.
    Text,
    /// A standalone C0 or C1 control.
    Control,
    /// A complete CSI sequence.
    Csi,
    /// A complete two- or three-byte escape sequence, such as `ESC ( B`.
    Esc,
    /// A complete OSC string.
    Osc,
    /// A complete DCS string.
    Dcs,
    /// A complete SOS, PM, or APC string.
    Apc,
    /// Input ended mid-sequence; the caller must carry these bytes forward.
    /// Never produced with [`Eof::Final`].
    Partial,
    /// A sequence that violated the grammar. Consumed and reported so it can be
    /// logged; safe to ignore.
    Malformed,
}

impl TokenKind {
    // The MALFORMED arm and the wildcard share a body on purpose: the explicit
    // arm documents the ABI's own constant, the wildcard is the fail-safe for a
    // value it does not document, and both degrade to "ignore this".
    #[allow(clippy::match_same_arms)]
    const fn from_raw(raw: u8) -> Self {
        match raw {
            sys::ANSI_TOKEN_TEXT => Self::Text,
            sys::ANSI_TOKEN_CONTROL => Self::Control,
            sys::ANSI_TOKEN_CSI => Self::Csi,
            sys::ANSI_TOKEN_ESC => Self::Esc,
            sys::ANSI_TOKEN_OSC => Self::Osc,
            sys::ANSI_TOKEN_DCS => Self::Dcs,
            sys::ANSI_TOKEN_APC => Self::Apc,
            sys::ANSI_TOKEN_PARTIAL => Self::Partial,
            sys::ANSI_TOKEN_MALFORMED => Self::Malformed,
            // Unreachable per the ABI. Reported as Malformed rather than
            // panicking, since a new token kind should degrade to "ignore this"
            // rather than take down a session.
            _ => Self::Malformed,
        }
    }
}

/// One token from the scanner.
///
/// Holds the raw structure by value. That is a ~432 byte copy per token, and the
/// alternative - borrowing from the scanner's internal slot - cannot be expressed
/// safely without lending iterators, since two tokens would alias the same slot.
/// Accessors return slices borrowed from `&self`, so reading a payload or
/// parameter list is allocation-free.
#[derive(Clone, Copy)]
pub struct Token {
    raw: sys::supra_ansi_token,
}

impl Token {
    /// What kind of token this is.
    #[must_use]
    pub const fn kind(&self) -> TokenKind {
        TokenKind::from_raw(self.raw.kind)
    }

    /// Byte offset within the buffer this token came from.
    #[must_use]
    pub const fn offset(&self) -> usize {
        self.raw.offset
    }

    /// Bytes consumed. At least 1 unless the buffer was empty.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.raw.length
    }

    /// Whether this token consumed no bytes.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.raw.length == 0
    }

    /// The control byte, or a sequence's final byte.
    #[must_use]
    pub const fn final_byte(&self) -> u8 {
        self.raw.final_byte
    }

    /// Private-mode and intermediate bytes, such as the `?` of `ESC [ ? 25 h`.
    #[must_use]
    pub fn intermediates(&self) -> &[u8] {
        let count = (self.raw.intermediate_count as usize).min(self.raw.intermediates.len());
        &self.raw.intermediates[..count]
    }

    /// Numeric parameters.
    ///
    /// A parameter the sender omitted is `-1`, which is distinct from an explicit
    /// `0` and changes the meaning of several sequences.
    #[must_use]
    pub fn params(&self) -> &[i32] {
        let count = (self.raw.param_count as usize).min(self.raw.params.len());
        &self.raw.params[..count]
    }

    /// Whether parameters beyond the ABI limit were discarded.
    #[must_use]
    pub const fn params_dropped(&self) -> bool {
        self.raw.params_dropped != 0
    }

    /// Sub-parameters of parameter `index`, as in the `4:3` of `ESC[4:3m`.
    #[must_use]
    pub fn subparams(&self, index: usize) -> &[i32] {
        if index >= self.params().len() {
            return &[];
        }
        let start = self.raw.subparam_index[index] as usize;
        let count = self.raw.subparam_count[index] as usize;
        let end = start.saturating_add(count).min(self.raw.subparams.len());
        if start >= end {
            return &[];
        }
        &self.raw.subparams[start..end]
    }

    /// OSC, DCS, or APC body, without introducer or terminator.
    ///
    /// Truncated to the ABI limit; [`Token::payload_full_len`] reports the real
    /// length.
    #[must_use]
    pub fn payload(&self) -> &[u8] {
        let len = (self.raw.payload_len as usize).min(self.raw.payload.len());
        &self.raw.payload[..len]
    }

    /// Real payload length, which may exceed what [`Token::payload`] returns.
    #[must_use]
    pub const fn payload_full_len(&self) -> usize {
        self.raw.payload_full
    }

    /// The bytes this token covers, given the buffer it was scanned from.
    ///
    /// Returns `None` when the span does not fit `buffer`, which can only happen
    /// if a different buffer is passed than the one scanned.
    #[must_use]
    pub fn text<'b>(&self, buffer: &'b [u8]) -> Option<&'b [u8]> {
        let end = self.raw.offset.checked_add(self.raw.length)?;
        buffer.get(self.raw.offset..end)
    }
}

impl core::fmt::Debug for Token {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Token")
            .field("kind", &self.kind())
            .field("offset", &self.offset())
            .field("len", &self.len())
            .field("final_byte", &self.final_byte())
            .field("params", &self.params())
            .field("payload_len", &self.payload().len())
            .finish()
    }
}

// ---------------------------------------------------------------------------
// Scanner
// ---------------------------------------------------------------------------

/// Whether a buffer ends the stream.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Eof {
    /// More input may follow. A sequence cut by the buffer end is reported as
    /// [`TokenKind::Partial`] and its state retained.
    More,
    /// No more input. An unterminated sequence becomes [`TokenKind::Malformed`],
    /// so a caller is never left waiting for a terminator that will not arrive.
    Final,
}

impl Eof {
    const fn raw(self) -> core::ffi::c_int {
        match self {
            Self::More => sys::ANSI_EOF_MORE,
            Self::Final => sys::ANSI_EOF_FINAL,
        }
    }
}

/// Resumable escape sequence scanner.
///
/// Resumability is the point. Shell output arrives in chunks and a sequence can
/// straddle a boundary, so a stateless matcher mangles exactly the output that
/// matters - the progress line, the coloured error - at the moment it is written.
///
/// The struct is large (~432 bytes) because it carries the in-flight sequence
/// state. Create one per stream and reuse it across chunks; boxing it is
/// reasonable if it would otherwise sit on a hot stack frame.
#[derive(Clone)]
pub struct Scanner {
    raw: sys::supra_ansi_scanner,
}

impl Default for Scanner {
    fn default() -> Self {
        Self::new()
    }
}

impl Scanner {
    /// A scanner in its initial state.
    #[must_use]
    pub fn new() -> Self {
        // Zeroed is the documented initial state, but going through the ABI's own
        // initialiser keeps this correct if that ever stops being true.
        let mut raw = core::mem::MaybeUninit::<sys::supra_ansi_scanner>::zeroed();
        // SAFETY: the pointer references the local's storage, which is large
        // enough and correctly aligned; the callee fully overwrites it.
        unsafe {
            sys::supra_ansi_scanner_init(raw.as_mut_ptr());
            Self { raw: raw.assume_init() }
        }
    }

    /// Whether an incomplete sequence is being held.
    ///
    /// Distinguishes "no more tokens in this buffer" from "waiting for more
    /// input".
    #[must_use]
    pub fn is_pending(&self) -> bool {
        // SAFETY: the raw-borrowed pointer references a live structure; read-only.
        unsafe { sys::supra_ansi_scanner_pending(&raw const self.raw) == 1 }
    }

    /// Read the next token from `bytes`.
    ///
    /// Call repeatedly with the same buffer until it returns `None`, then move to
    /// the next chunk. State carries across calls.
    pub fn next_token(&mut self, bytes: &[u8], eof: Eof) -> Option<Token> {
        let mut token = core::mem::MaybeUninit::<sys::supra_ansi_token>::zeroed();

        // SAFETY: `&mut self.raw` is unaliased for the call's duration; the slice
        // pointer and length agree; `token` is valid, aligned storage the callee
        // fully initialises before reporting success.
        let produced = unsafe {
            sys::supra_ansi_scan(
                &raw mut self.raw,
                bytes.as_ptr(),
                bytes.len(),
                eof.raw(),
                token.as_mut_ptr(),
            )
        };

        if produced == 1 {
            // SAFETY: the callee initialised the structure, as signalled by
            // returning 1.
            Some(Token { raw: unsafe { token.assume_init() } })
        } else {
            None
        }
    }

    /// Iterate the tokens in one buffer.
    ///
    /// The scanner is borrowed for the iterator's lifetime, so state still carries
    /// into the next chunk.
    pub fn tokens<'s, 'b>(&'s mut self, bytes: &'b [u8], eof: Eof) -> Tokens<'s, 'b> {
        Tokens { scanner: self, bytes, eof, done: false }
    }
}

/// Iterator over the tokens in one buffer.
pub struct Tokens<'s, 'b> {
    scanner: &'s mut Scanner,
    bytes: &'b [u8],
    eof: Eof,
    done: bool,
}

impl Iterator for Tokens<'_, '_> {
    type Item = Token;

    fn next(&mut self) -> Option<Self::Item> {
        if self.done {
            return None;
        }

        let token = self.scanner.next_token(self.bytes, self.eof)?;

        // A zero-length Malformed token is the terminal report for an unterminated
        // sequence at end of stream. Yielding it and then stopping prevents an
        // infinite loop while still surfacing the diagnosis.
        if token.is_empty() && token.kind() == TokenKind::Malformed {
            self.done = true;
        }

        Some(token)
    }
}

// ---------------------------------------------------------------------------
// Measurement, stripping, truncation
// ---------------------------------------------------------------------------

/// Cells occupied by `bytes`, ignoring escape sequences.
#[must_use]
pub fn measure(bytes: &[u8], ambiguous: Ambiguous) -> usize {
    // SAFETY: slice pointer and length agree; the callee only reads.
    unsafe { sys::supra_ansi_measure(bytes.as_ptr(), bytes.len(), raw_ambiguous(ambiguous)) }
}

/// Copy `bytes` with every escape sequence removed, appending to `out`.
///
/// C0 controls other than tab and newline are dropped too: they are not content,
/// and passing them through would let tool output move the cursor.
pub fn strip(bytes: &[u8], out: &mut Vec<u8>) {
    // SAFETY: slice pointer and length agree; a null output asks for the length.
    let needed = unsafe { sys::supra_ansi_strip(bytes.as_ptr(), bytes.len(), core::ptr::null_mut(), 0) };
    if needed == 0 {
        return;
    }

    let start = out.len();
    out.resize(start + needed, 0);
    // SAFETY: the destination is valid for `needed` bytes, which is what the
    // first call reported.
    let written =
        unsafe { sys::supra_ansi_strip(bytes.as_ptr(), bytes.len(), out[start..].as_mut_ptr(), needed) };
    out.truncate(start + written.min(needed));
}

/// A planned truncation.
///
/// Write `bytes[..prefix_len]` followed by `trailer()`. No output buffer and no
/// copy, which matters because this runs per line per frame.
#[derive(Clone, Copy)]
pub struct Truncation {
    raw: sys::supra_ansi_truncation,
}

impl Truncation {
    /// Bytes of the input to emit. Never ends inside an escape sequence or splits
    /// a grapheme cluster.
    #[must_use]
    pub const fn prefix_len(&self) -> usize {
        self.raw.prefix_len
    }

    /// Cells the prefix occupies. May be less than the limit when a two-cell
    /// cluster would have crossed it.
    #[must_use]
    pub const fn cells(&self) -> usize {
        self.raw.cells
    }

    /// Bytes to append so the terminal is left in its default state.
    ///
    /// Empty for an unstyled line, so plain text costs no extra bytes.
    #[must_use]
    pub fn trailer(&self) -> &[u8] {
        let len = (self.raw.trailer_len as usize).min(self.raw.trailer.len());
        &self.raw.trailer[..len]
    }

    /// Whether the input was cut.
    #[must_use]
    pub const fn was_truncated(&self) -> bool {
        self.raw.truncated != 0
    }

    /// Style in force at the cut.
    ///
    /// Reflects every sequence inside the prefix, including a trailing one that
    /// covers no cell - because that sequence is still emitted, so whatever it
    /// opened is live and the trailer must close it.
    #[must_use]
    pub fn style_at_cut(&self) -> Style {
        Style::from_raw(self.raw.style_at_cut)
    }

    /// Assemble prefix and trailer into `out`.
    ///
    /// Convenience for callers not managing their own buffer.
    pub fn render(&self, input: &[u8], out: &mut Vec<u8>) {
        let prefix = self.raw.prefix_len.min(input.len());
        out.extend_from_slice(&input[..prefix]);
        out.extend_from_slice(self.trailer());
    }
}

impl core::fmt::Debug for Truncation {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Truncation")
            .field("prefix_len", &self.prefix_len())
            .field("cells", &self.cells())
            .field("trailer_len", &self.trailer().len())
            .field("truncated", &self.was_truncated())
            .finish()
    }
}

/// Plan a truncation of `bytes` to at most `max_cells`.
///
/// Guarantees, all enforced by the C++ suites: the result never exceeds
/// `max_cells`, never ends inside an escape sequence, never splits a grapheme
/// cluster, and prefix plus trailer leaves the terminal in its default state.
#[must_use]
pub fn plan_truncate(bytes: &[u8], max_cells: usize, ambiguous: Ambiguous) -> Truncation {
    let mut raw = core::mem::MaybeUninit::<sys::supra_ansi_truncation>::zeroed();
    // SAFETY: slice pointer and length agree; `raw` is valid, aligned storage the
    // callee fully initialises.
    unsafe {
        sys::supra_ansi_plan_truncate(
            bytes.as_ptr(),
            bytes.len(),
            max_cells,
            raw_ambiguous(ambiguous),
            raw.as_mut_ptr(),
        );
        Truncation { raw: raw.assume_init() }
    }
}

/// Plan a truncation starting from an inherited style.
///
/// For continuation lines, where styling opened on a previous line is still in
/// effect and must still be closed.
#[must_use]
pub fn plan_truncate_from(
    bytes: &[u8],
    max_cells: usize,
    ambiguous: Ambiguous,
    initial: Style,
) -> Truncation {
    let initial_raw = initial.to_raw();
    let mut raw = core::mem::MaybeUninit::<sys::supra_ansi_truncation>::zeroed();
    // SAFETY: as above, plus `initial_raw` is a live local read by the callee.
    unsafe {
        sys::supra_ansi_plan_truncate_from(
            bytes.as_ptr(),
            bytes.len(),
            max_cells,
            raw_ambiguous(ambiguous),
            &raw const initial_raw,
            raw.as_mut_ptr(),
        );
        Truncation { raw: raw.assume_init() }
    }
}

/// Bridge `Ambiguous` into the raw discriminant.
///
/// Duplicated from `width` rather than made public there: the conversion is an
/// implementation detail of the FFI boundary, and exposing it would invite callers
/// to pass raw integers.
const fn raw_ambiguous(ambiguous: Ambiguous) -> core::ffi::c_int {
    match ambiguous {
        Ambiguous::Narrow => sys::AMBIGUOUS_NARROW,
        Ambiguous::Wide => sys::AMBIGUOUS_WIDE,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn collect(input: &[u8]) -> Vec<Token> {
        let mut scanner = Scanner::new();
        scanner.tokens(input, Eof::Final).collect()
    }

    fn fold(input: &[u8]) -> Style {
        let mut style = Style::default();
        for token in collect(input) {
            style.apply(&token);
        }
        style
    }

    #[test]
    fn scans_a_styled_line() {
        let tokens = collect(b"\x1b[31mred\x1b[0m");
        assert_eq!(tokens.len(), 3);
        assert_eq!(tokens[0].kind(), TokenKind::Csi);
        assert_eq!(tokens[0].final_byte(), b'm');
        assert_eq!(tokens[0].params(), &[31]);
        assert_eq!(tokens[1].kind(), TokenKind::Text);
        assert_eq!(tokens[1].text(b"\x1b[31mred\x1b[0m"), Some(&b"red"[..]));
    }

    #[test]
    fn omitted_parameter_is_not_zero() {
        // The distinction changes the meaning of several sequences, so the wrapper
        // must not normalise it away.
        let tokens = collect(b"\x1b[m");
        assert_eq!(tokens[0].params(), &[-1]);

        let tokens = collect(b"\x1b[1;;5m");
        assert_eq!(tokens[0].params(), &[1, -1, 5]);
    }

    #[test]
    fn subparams_are_addressable_per_parameter() {
        let tokens = collect(b"\x1b[4:3m");
        assert_eq!(tokens[0].params(), &[4]);
        assert_eq!(tokens[0].subparams(0), &[3]);
        assert_eq!(tokens[0].subparams(1), &[] as &[i32], "out of range yields empty");
    }

    #[test]
    fn payload_is_borrowed_not_copied() {
        let tokens = collect(b"\x1b]8;;https://example.com\x1b\\");
        assert_eq!(tokens[0].kind(), TokenKind::Osc);
        assert_eq!(tokens[0].payload(), b"8;;https://example.com");
    }

    #[test]
    fn style_round_trips_through_emit() {
        // The property the truncation trailer depends on: folding a serialised
        // style must reproduce the style, or the trailer would fail to close what
        // the prefix opened.
        let inputs: &[&[u8]] = &[
            b"\x1b[1m",
            b"\x1b[1;3;4;7m",
            b"\x1b[38;5;196m",
            b"\x1b[38;2;255;128;0m",
            b"\x1b[4:3m",
            b"\x1b[58;5;42m",
            b"\x1b[1;38;2;10;20;30;48;5;9;4:3;53m",
        ];

        for input in inputs {
            let original = fold(input);
            let mut serialised = Vec::new();
            original.emit(&mut serialised);
            let refolded = fold(&serialised);
            assert_eq!(refolded, original, "round trip for {input:?}");
        }
    }

    #[test]
    fn default_style_emits_nothing() {
        let mut out = Vec::new();
        Style::default().emit(&mut out);
        assert!(out.is_empty(), "a spurious reset per line is measurable waste");

        Style::default().emit_reset(&mut out);
        assert!(out.is_empty());
    }

    #[test]
    fn sgr_reset_does_not_close_a_hyperlink() {
        // Only OSC 8 can, which is why the style tracks it separately.
        let style = fold(b"\x1b]8;;u\x1b\\\x1b[0m");
        assert!(style.hyperlink_open);

        let closed = fold(b"\x1b]8;;u\x1b\\\x1b]8;;\x1b\\");
        assert!(!closed.hyperlink_open);
    }

    #[test]
    fn chunking_does_not_change_the_result() {
        // The reason the scanner carries state. A sequence split across reads must
        // produce the same tokens as one delivered whole.
        let input = b"\x1b[1;38;2;255;128;0mstyled \xE4\xB8\xAD text\x1b[0m";

        let whole = fold(input);

        for chunk in 1..=input.len() {
            let mut scanner = Scanner::new();
            let mut style = Style::default();
            let mut offset = 0;

            while offset < input.len() {
                let end = (offset + chunk).min(input.len());
                let eof = if end >= input.len() { Eof::Final } else { Eof::More };
                let slice = &input[offset..end];

                while let Some(token) = scanner.next_token(slice, eof) {
                    style.apply(&token);
                }
                offset = end;
            }

            assert_eq!(style, whole, "chunk size {chunk}");
        }
    }

    #[test]
    fn truncation_is_style_balanced() {
        let input = b"\x1b[31mred text here";
        let plan = plan_truncate(input, 5, Ambiguous::Narrow);

        assert_eq!(plan.cells(), 5);
        assert!(plan.was_truncated());
        assert!(!plan.trailer().is_empty(), "a cut inside styling needs a trailer");

        let mut rendered = Vec::new();
        plan.render(input, &mut rendered);
        assert!(fold(&rendered).is_default(), "rendered output ends in the default style");
    }

    #[test]
    fn unstyled_line_needs_no_trailer() {
        let plan = plan_truncate(b"hello world", 5, Ambiguous::Narrow);
        assert_eq!(plan.prefix_len(), 5);
        assert!(plan.trailer().is_empty(), "plain text costs no extra bytes");
    }

    #[test]
    fn truncation_never_exceeds_the_limit() {
        let inputs: &[&[u8]] = &[
            b"plain text",
            b"\x1b[31mred\x1b[0m",
            "\u{4E2D}\u{6587}\u{5B57}".as_bytes(),
            "\u{1F600}\u{1F601}".as_bytes(),
            b"\x1b]8;;https://example.com\x1b\\link\x1b]8;;\x1b\\",
            b"\x1b[38:2::255:128:0mcolon form",
        ];

        for input in inputs {
            for limit in 0..=15 {
                for ambiguous in [Ambiguous::Narrow, Ambiguous::Wide] {
                    let plan = plan_truncate(input, limit, ambiguous);
                    assert!(plan.cells() <= limit, "{input:?} at limit {limit}");

                    let mut rendered = Vec::new();
                    plan.render(input, &mut rendered);
                    assert!(fold(&rendered).is_default(), "{input:?} at limit {limit} left styling open");
                }
            }
        }
    }

    #[test]
    fn inherited_style_is_closed_even_on_an_empty_line() {
        let initial = Style { fg: Color::Indexed(1), attrs: Attrs::BOLD, ..Style::default() };

        let plan = plan_truncate_from(&[], 10, Ambiguous::Narrow, initial);
        assert!(!plan.trailer().is_empty(), "an inherited style must not leak past the line");
    }

    #[test]
    fn strip_keeps_layout_controls_only() {
        let mut out = Vec::new();
        strip(b"\x1b[31mred\x1b[0m", &mut out);
        assert_eq!(out, b"red");

        out.clear();
        strip(b"a\nb\tc", &mut out);
        assert_eq!(out, b"a\nb\tc", "tab and newline are content");

        out.clear();
        strip(b"a\x07b", &mut out);
        assert_eq!(out, b"ab", "BEL can move the cursor, so it is dropped");
    }

    #[test]
    fn measure_ignores_escapes() {
        assert_eq!(measure(b"\x1b[31mhello\x1b[0m", Ambiguous::Narrow), 5);
        assert_eq!(measure("\u{4E2D}\u{6587}".as_bytes(), Ambiguous::Narrow), 4);
        assert_eq!(measure(&[], Ambiguous::Narrow), 0);
    }

    #[test]
    fn pending_state_is_observable() {
        let mut scanner = Scanner::new();
        while scanner.next_token(b"\x1b[31", Eof::More).is_some() {}
        assert!(scanner.is_pending());

        let token = scanner.next_token(b"m", Eof::Final).expect("sequence completes");
        assert_eq!(token.kind(), TokenKind::Csi);
        assert_eq!(token.params(), &[31]);
        assert!(!scanner.is_pending());
    }

    #[test]
    fn token_iterator_terminates_on_unterminated_input() {
        // A zero-length Malformed report must not loop forever.
        let tokens = collect(b"\x1b[31");
        assert!(!tokens.is_empty());
        assert!(tokens.iter().any(|t| t.kind() == TokenKind::Malformed));
    }

    #[test]
    fn empty_input_is_handled() {
        assert_eq!(collect(&[]).len(), 0);
        assert_eq!(measure(&[], Ambiguous::Narrow), 0);

        let plan = plan_truncate(&[], 10, Ambiguous::Narrow);
        assert_eq!(plan.prefix_len(), 0);
        assert_eq!(plan.cells(), 0);
    }
}
