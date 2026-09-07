//! Deterministic output shaping.
//!
//! The pty hands over raw bytes: escape sequences, carriage returns,
//! progress lines overwriting themselves, a prompt with no trailing
//! newline. The consumer - the transcript, the TUI, the LLM - wants one
//! answer for one byte stream, regardless of how the stream was chunked.
//! That answer is this module's contract:
//!
//! **The same byte stream in, the same shaped output out, whatever chunk
//! boundaries it crossed.**
//!
//! Three mechanisms deliver it, and each has a written reason:
//!
//! 1. **Escape parsing is the C++ scanner's, never a reimplementation.**
//!    T3's verification binds T16.5's output shaping: the C1-positional
//!    rule (`0x80..=0xBF` on a scalar boundary) lives in `libsupra_ansi`,
//!    and this crate routes every byte through `supra_ffi::ansi::Scanner`.
//!    The scanner is resumable, so a sequence straddling two chunks is
//!    parsed as one token, not two halves. [`TokenKind::Partial`] is
//!    bookkeeping, not content - the reference consumer in the C++ suite
//!    skips it, and so does this one.
//! 2. **The line discipline is explicit C0 semantics.** `\n` commits a
//!    line; `\r` returns the cursor to column zero; backspace moves one
//!    column left; text overwrites from the cursor. This is what a
//!    terminal *draws*: a progress line `50%\r75%\r100%` shapes to
//!    `100%`, and a transcript that kept all three fragments would be
//!    lying about what the user saw. Other C0 controls (bell, etc.) are
//!    dropped - they signal, they do not render.
//! 3. **Truncation is per-line and cell-accurate.** A line over
//!    `max_line_cells` is cut by the C++ truncation planner, which never
//!    splits a grapheme and never ends inside a sequence. The committed
//!    transcript is a ring of the last `max_lines` lines; older lines are
//!    dropped and counted, because recent output is what a consumer reads
//!    first and a bounded transcript is a predictable one.
//!
//! # The prompt heuristic
//!
//! [`Shaped::suggests_prompt`] is deliberately heuristic: a short,
//! unterminated line ending in a prompt glyph (`$`, `>`, `#`, `%`, `:`)
//! and not continuing with a backslash *looks* like a prompt. T4's note
//! binds the action: heuristics produce false positives, which is why the
//! caller **asks the user** rather than killing or assuming the session
//! is idle.

use std::collections::VecDeque;

use supra_ffi::ansi::{Eof, Scanner, Token, TokenKind};
use supra_ffi::width::Ambiguous;

/// How much of the stream the shaper has turned into transcript.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Shaped {
    /// Committed lines in the ring right now.
    pub lines: u64,
    /// Cells the committed lines occupy, as the width table counts them.
    pub cells: u64,
    /// Lines cut because they exceeded `max_line_cells`.
    pub truncated_lines: u64,
    /// Lines dropped because the ring was full.
    pub dropped_lines: u64,
    /// The heuristic says the stream currently ends on a prompt-like line.
    pub suggests_prompt: bool,
}

/// A resumable shaper for one pty stream.
///
/// Owns the scanner (large, ~432 bytes - create one per session, not per
/// chunk) and the transcript ring. `push` feeds one chunk; `finish` closes
/// the stream; `render` writes the current transcript out.
pub struct Shaper {
    scanner: Scanner,
    line: Vec<u8>,
    cursor: usize,
    ring: VecDeque<Vec<u8>>,
    truncated_lines: u64,
    dropped_lines: u64,
    max_line_cells: usize,
    max_lines: usize,
    ambiguous: Ambiguous,
    finished: bool,
}

impl Shaper {
    /// A shaper that cuts lines at `max_line_cells` cells and keeps the last
    /// `max_lines` of them.
    #[must_use]
    pub fn new(max_line_cells: usize, max_lines: usize, ambiguous: Ambiguous) -> Self {
        Self {
            scanner: Scanner::new(),
            line: Vec::new(),
            cursor: 0,
            ring: VecDeque::new(),
            truncated_lines: 0,
            dropped_lines: 0,
            max_line_cells,
            max_lines,
            ambiguous,
            finished: false,
        }
    }

    /// Feed one chunk of the stream.
    ///
    /// `out` receives nothing - the transcript lives inside the shaper, and
    /// the caller pulls it out with [`Shaper::render`] when the consumer is
    /// ready. (Output is shaped into the ring on every push regardless.)
    ///
    /// After `finish`, this is a no-op.
    pub fn push(&mut self, bytes: &[u8]) {
        if self.finished {
            return;
        }
        // Tokens are Copy; collecting ends the scanner borrow before any
        // `apply` touches the rest of the shaper.
        let tokens: Vec<Token> = self.scanner.tokens(bytes, Eof::More).collect();
        for token in tokens {
            self.apply(&token, bytes);
        }
    }

    /// Close the stream: an unterminated sequence becomes malformed and the
    /// last line - which may be a prompt with no trailing newline - commits.
    pub fn finish(&mut self) -> Shaped {
        if !self.finished {
            // A final scan with Eof::Final turns any in-flight sequence into
            // a Malformed token; nothing else can be pending at this point.
            let empty: &[u8] = &[];
            let tokens: Vec<Token> = self.scanner.tokens(empty, Eof::Final).collect();
            for token in tokens {
                self.apply(&token, empty);
            }
            self.finished = true;
        }
        self.commit_open_line();
        self.stats()
    }

    /// Render the transcript: committed lines joined by `\n`, oldest first,
    /// plus the open line verbatim after a separator when both exist.
    ///
    /// A trailing `\n` in the source commits its line and leaves the open
    /// line empty, so `"a\n"` renders as `"a"` - the newline is the commit
    /// act, not content. The committed ring joins with `\n` and nothing
    /// more.
    pub fn render(&self, out: &mut Vec<u8>) {
        for (index, line) in self.ring.iter().enumerate() {
            if index > 0 {
                out.push(b'\n');
            }
            out.extend_from_slice(line);
        }
        if self.line.is_empty() {
            return;
        }
        if !self.ring.is_empty() {
            out.push(b'\n');
        }
        out.extend_from_slice(&self.line);
    }

    /// The current stats, as of the last push or finish.
    #[must_use]
    pub fn stats(&self) -> Shaped {
        let cells = self.ring.iter().map(|line| supra_ffi::width::width(line, self.ambiguous) as u64).sum();
        Shaped {
            lines: self.ring.len() as u64,
            cells,
            truncated_lines: self.truncated_lines,
            dropped_lines: self.dropped_lines,
            suggests_prompt: self.suggests_prompt(),
        }
    }

    /// The prompt heuristic.
    ///
    /// True when the open line is short, unterminated (no trailing `\n`),
    /// does not end in a backslash (line continuation), and ends in a
    /// prompt-like glyph (with a trailing space allowed). False positives
    /// are expected (T4's note) and the caller's action is to ask the user,
    /// never to assume.
    #[must_use]
    pub fn suggests_prompt(&self) -> bool {
        if self.finished {
            return false;
        }
        if self.line.is_empty() || self.line.ends_with(b"\n") {
            return false;
        }
        if self.line.len() > 16 {
            return false;
        }
        if self.line.ends_with(b"\\") {
            return false;
        }
        let trimmed = self.line.trim_ascii_end();
        if trimmed.is_empty() {
            return false;
        }
        matches!(trimmed.last(), Some(b'$' | b'>' | b'#' | b'%' | b':'))
    }

    fn apply(&mut self, token: &Token, bytes: &[u8]) {
        match token.kind() {
            // PARTIAL reports bytes consumed so far (the scanner retains the
            // state itself; the reference consumer skips them), MALFORMED is
            // a diagnosis at end of stream, and sequences carry styling and
            // cursor movement rather than content. None of the three shapes
            // transcript text, so one arm covers all of them.
            TokenKind::Partial
            | TokenKind::Malformed
            | TokenKind::Csi
            | TokenKind::Esc
            | TokenKind::Osc
            | TokenKind::Dcs
            | TokenKind::Apc => {}
            // C0 controls: the four that move the cursor have written
            // semantics below; the rest (bell and friends) signal, and a
            // transcript has no use for them.
            TokenKind::Control => match token.final_byte() {
                b'\n' => self.commit_open_line(),
                b'\r' => self.cursor = 0,
                0x08 => self.cursor = self.cursor.saturating_sub(1),
                _ => {}
            },
            TokenKind::Text => {
                let Some(text) = token.text(bytes) else { return };
                // The C++ scanner bundles `\n` inside the text run rather than
                // as a separate control token. The harness's unit is a line,
                // so every newline inside the run commits, piece by piece.
                let mut remaining = text;
                while let Some(at) = remaining.iter().position(|&b| b == b'\n') {
                    let (before, after) = remaining.split_at(at);
                    if !before.is_empty() {
                        self.write_at_cursor(before);
                    }
                    self.commit_open_line();
                    remaining = &after[1..];
                }
                if !remaining.is_empty() {
                    self.write_at_cursor(remaining);
                }
            }
        }
    }

    /// Overwrite from the cursor, extending the line when needed.
    fn write_at_cursor(&mut self, text: &[u8]) {
        if self.cursor == self.line.len() {
            self.line.extend_from_slice(text);
            self.cursor = self.line.len();
            return;
        }
        let end = self.cursor.saturating_add(text.len());
        if end > self.line.len() {
            self.line.resize(end, b' ');
        }
        self.line[self.cursor..end].copy_from_slice(text);
        self.cursor = end;
    }

    /// Commit the open line into the ring, truncating it to the cell budget
    /// first, then start a fresh line.
    fn commit_open_line(&mut self) {
        let line = std::mem::take(&mut self.line);
        self.cursor = 0;
        if line.is_empty() {
            // Nothing to push; the trailing-newline contract lives in render,
            // not in a phantom empty entry.
            return;
        }

        let truncation = supra_ffi::ansi::plan_truncate(&line, self.max_line_cells, self.ambiguous);
        if truncation.was_truncated() {
            self.truncated_lines += 1;
        }
        let kept = line[..truncation.prefix_len()].to_vec();
        if self.max_lines == 0 {
            self.dropped_lines += 1;
            return;
        }
        if self.ring.len() == self.max_lines {
            self.ring.pop_front();
            self.dropped_lines += 1;
        }
        self.ring.push_back(kept);
    }
}

impl core::fmt::Debug for Shaper {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Shaper")
            .field("ring_len", &self.ring.len())
            .field("open_line_len", &self.line.len())
            .field("cursor", &self.cursor)
            .field("truncated_lines", &self.truncated_lines)
            .field("dropped_lines", &self.dropped_lines)
            .field("max_line_cells", &self.max_line_cells)
            .field("max_lines", &self.max_lines)
            .field("finished", &self.finished)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shaped(text: &str, cells: usize, lines: usize) -> String {
        let mut shaper = Shaper::new(cells, lines, Ambiguous::Narrow);
        shaper.push(text.as_bytes());
        shaper.finish();
        let mut out = Vec::new();
        shaper.render(&mut out);
        String::from_utf8(out).expect("shaped output is utf-8")
    }

    #[test]
    fn plain_lines_survive_verbatim() {
        assert_eq!(shaped("hello\nworld\n", 80, 64), "hello\nworld");
        // Unterminated last line is kept without a trailing newline: the
        // stream committed no newline there, so the transcript does not invent
        // one.
        assert_eq!(shaped("hello\nworld", 80, 64), "hello\nworld");
    }

    #[test]
    fn escape_sequences_are_stripped() {
        assert_eq!(shaped("\x1b[31mred\x1b[0m\n", 80, 64), "red");
        assert_eq!(shaped("a\x1b[1mb\x1b[2Jc\n", 80, 64), "abc");
    }

    #[test]
    fn carriage_return_overwrites_from_column_zero() {
        // The progress-line shape: only the last segment survives, because
        // that is what the terminal drew.
        assert_eq!(shaped("50%\r75%\r100%\n", 80, 64), "100%");
        // Overwrite is per-column, like a terminal: "X" over "abc" leaves
        // "bc" standing.
        assert_eq!(shaped("abc\rX\n", 80, 64), "Xbc");
    }

    #[test]
    fn backspace_moves_left() {
        assert_eq!(shaped("ab\x08c\n", 80, 64), "ac");
    }

    #[test]
    fn bell_and_other_controls_are_dropped() {
        assert_eq!(shaped("a\x07b\x0cc\n", 80, 64), "abc");
    }

    #[test]
    fn chunk_boundaries_do_not_change_the_answer() {
        let input = "\x1b[31mred\x1b[0m 50%\r75%\r100%\nok\n";
        let one_chunk = shaped(input, 80, 64);

        let mut shaper = Shaper::new(80, 64, Ambiguous::Narrow);
        // Cut mid-sequence, mid-escape, and mid-word on purpose.
        for chunk in ["\x1b[3", "1mred\x1b", "[0m 5", "0%\r7", "5%\r100%\nok\n"] {
            shaper.push(chunk.as_bytes());
        }
        shaper.finish();
        let mut out = Vec::new();
        shaper.render(&mut out);
        assert_eq!(String::from_utf8(out).expect("utf-8"), one_chunk);
    }

    #[test]
    fn long_lines_are_truncated_per_cell() {
        let mut shaper = Shaper::new(10, 64, Ambiguous::Narrow);
        shaper.push(b"abcdefghijklmnopqrstuvwxyz\nshort\n");
        shaper.finish();

        let mut out = Vec::new();
        shaper.render(&mut out);
        assert_eq!(out, b"abcdefghij\nshort");
        assert_eq!(shaper.stats().truncated_lines, 1);
    }

    #[test]
    fn wide_clusters_are_never_split() {
        // 10 cells with a 2-cell character at the boundary: the cut must
        // fall before the wide cluster, never through it.
        let mut shaper = Shaper::new(9, 64, Ambiguous::Narrow);
        shaper.push("12345678\u{4e2d}\u{6587}x\n".as_bytes());
        shaper.finish();
        let mut out = Vec::new();
        shaper.render(&mut out);
        assert_eq!(out, "12345678".as_bytes());
    }

    #[test]
    fn the_ring_keeps_the_newest_lines() {
        let mut shaper = Shaper::new(80, 3, Ambiguous::Narrow);
        for index in 0..10 {
            shaper.push(format!("line {index}\n").as_bytes());
        }
        shaper.finish();
        let mut out = Vec::new();
        shaper.render(&mut out);
        assert_eq!(out, b"line 7\nline 8\nline 9");
        assert_eq!(shaper.stats().dropped_lines, 7);
    }

    #[test]
    fn push_after_finish_is_a_no_op() {
        let mut shaper = Shaper::new(80, 64, Ambiguous::Narrow);
        shaper.push(b"one\n");
        shaper.finish();
        shaper.push(b"two\n");
        let mut out = Vec::new();
        shaper.render(&mut out);
        assert_eq!(out, b"one");
    }

    #[test]
    fn prompt_heuristic_recognises_a_prompt() {
        let mut shaper = Shaper::new(80, 64, Ambiguous::Narrow);
        shaper.push(b"echo hello\n");
        assert!(!shaper.suggests_prompt(), "a committed line is not a prompt");

        shaper.push(b"$ ");
        assert!(shaper.suggests_prompt(), "short unterminated prompt glyph");

        shaper.push(b"echo hi\n");
        assert!(!shaper.suggests_prompt(), "a newline ends the prompt state");

        shaper.finish();
        assert!(!shaper.suggests_prompt(), "a finished stream never suggests a prompt");
    }

    #[test]
    fn prompt_heuristic_rejects_continuations_and_long_lines() {
        let mut shaper = Shaper::new(80, 64, Ambiguous::Narrow);
        shaper.push(b"some \\");
        assert!(!shaper.suggests_prompt(), "a backslash continuation is not a prompt");

        shaper.push(b"a-very-long-line-that-exceeds-sixteen-bytes > ");
        assert!(!shaper.suggests_prompt(), "a long line is content, not a prompt");
    }
}
