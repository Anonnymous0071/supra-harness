use supra_theme::{Theme, Token};

/// The thinking display states. Read-only: `ctrl+o` toggles collapse,
/// nothing else changes it, and there is no cost preview - the tokens
/// are billed either way.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ThinkingState {
    /// The stream is in progress.
    Streaming,
    /// The stream ended; the block is collapsed.
    Collapsed,
    /// The stream ended; the block is expanded.
    Expanded,
}

/// Renders a bounded thinking summary and, only while expanded, its body.
#[derive(Clone, Copy, Debug)]
pub struct ThinkingDisplay<'a> {
    /// Seconds the stream took, for the `Thought for Ns` line.
    pub seconds: u64,
    /// Completed thinking text. Streaming and collapsed states never reveal it.
    pub body: &'a str,
}

impl ThinkingDisplay<'_> {
    /// Render the state at no more than `cols` cells per line and `body_rows`
    /// detail rows. Input escapes and cursor-moving controls are stripped;
    /// theme styling is applied only after cell-safe truncation.
    #[must_use]
    pub fn render(&self, state: ThinkingState, cols: usize, body_rows: usize, theme: &Theme) -> String {
        let header = match state {
            ThinkingState::Streaming => "\u{2235} Thinking\u{2026}".to_owned(),
            ThinkingState::Collapsed => {
                format!("\u{2234} Thought for {}s (ctrl+o to expand)", self.seconds)
            }
            ThinkingState::Expanded => {
                format!("\u{2234} Thought for {}s (ctrl+o to collapse)", self.seconds)
            }
        };
        let mut lines = Vec::with_capacity(body_rows.saturating_add(1));
        lines.push(render_line(&header, cols, theme));
        if state == ThinkingState::Expanded {
            lines.extend(body_lines(self.body, cols, body_rows, theme));
        }
        lines.join("\n")
    }
}

fn body_lines(body: &str, cols: usize, body_rows: usize, theme: &Theme) -> Vec<String> {
    let mut plain = Vec::new();
    supra_ffi::ansi::strip(body.as_bytes(), &mut plain);
    String::from_utf8_lossy(&plain)
        .lines()
        .take(body_rows)
        .map(|line| render_line(line, cols, theme))
        .collect()
}

fn render_line(line: &str, cols: usize, theme: &Theme) -> String {
    let truncated = supra_ffi::width::truncate(line.as_bytes(), cols, theme.ambiguous);
    let text = String::from_utf8_lossy(&line.as_bytes()[..truncated.bytes]);
    theme.paint(Token::Thinking, &text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use supra_ffi::width::Ambiguous;

    fn display() -> ThinkingDisplay<'static> {
        ThinkingDisplay { seconds: 84, body: "first detail\nsecond detail\nthird detail" }
    }

    fn plain(rendered: &str) -> String {
        let mut plain = Vec::new();
        supra_ffi::ansi::strip(rendered.as_bytes(), &mut plain);
        String::from_utf8(plain).expect("thinking output is utf-8")
    }

    #[test]
    fn streaming_shows_only_the_thinking_glyph() {
        let rendered = display().render(ThinkingState::Streaming, 80, 3, &Theme::default_dark());
        assert_eq!(plain(&rendered), "\u{2235} Thinking\u{2026}");
        assert!(!rendered.contains("first detail"));
    }

    #[test]
    fn collapsed_hides_the_body_and_shows_the_expand_hint() {
        let rendered = display().render(ThinkingState::Collapsed, 80, 3, &Theme::default_dark());
        let plain = plain(&rendered);
        assert!(plain.starts_with("\u{2234} Thought for 84s"), "{plain}");
        assert!(plain.contains("(ctrl+o to expand)"), "{plain}");
        assert!(!plain.contains("first detail"), "{plain}");
        assert_eq!(plain.lines().count(), 1);
    }

    #[test]
    fn expanded_renders_only_the_bounded_body_rows() {
        let rendered = display().render(ThinkingState::Expanded, 80, 2, &Theme::default_dark());
        let plain = plain(&rendered);
        assert!(plain.starts_with("\u{2234} Thought for 84s (ctrl+o to collapse)"), "{plain}");
        assert!(plain.contains("first detail\nsecond detail"), "{plain}");
        assert!(!plain.contains("third detail"), "{plain}");
        assert_eq!(plain.lines().count(), 3);
    }

    #[test]
    fn every_state_is_cell_bounded_in_narrow_and_wide_modes() {
        for ambiguous in [Ambiguous::Narrow, Ambiguous::Wide] {
            let theme = themed(ambiguous);
            for cols in 0..=20 {
                for state in [ThinkingState::Streaming, ThinkingState::Collapsed, ThinkingState::Expanded] {
                    let rendered = ThinkingDisplay {
                        seconds: u64::MAX,
                        body: "emoji \u{1f468}\u{200d}\u{1f469}\u{200d}\u{1f467} and text\n\u{4e2d}\u{6587}\u{5b57}",
                    }
                    .render(state, cols, 2, &theme);
                    for line in rendered.lines() {
                        let cells = supra_ffi::ansi::measure(line.as_bytes(), ambiguous);
                        assert!(cells <= cols, "{state:?}, {ambiguous:?}, {cols}: {line:?} is {cells}");
                    }
                }
            }
        }
    }

    #[test]
    fn expanded_body_strips_ansi_and_cursor_controls_before_painting() {
        let display = ThinkingDisplay { seconds: 1, body: "\x1b[31mred\x1b[0m\x07 safe\nnext" };
        let rendered = display.render(ThinkingState::Expanded, 80, 2, &Theme::default_dark());
        let plain = plain(&rendered);
        assert!(plain.ends_with("red safe\nnext"), "{plain:?}");
        assert!(!plain.contains('\x07'));
    }

    #[test]
    fn zero_body_rows_preserves_only_the_completed_header() {
        let rendered = display().render(ThinkingState::Expanded, 80, 0, &Theme::default_dark());
        assert_eq!(plain(&rendered).lines().count(), 1);
    }

    #[test]
    fn the_display_never_carries_a_cost() {
        for state in [ThinkingState::Streaming, ThinkingState::Collapsed, ThinkingState::Expanded] {
            let rendered = display().render(state, 80, 3, &Theme::default_dark());
            assert!(!rendered.contains('$'), "no cost preview");
        }
    }

    fn themed(ambiguous: Ambiguous) -> Theme {
        Theme::new(
            "probe",
            ambiguous,
            [
                (Token::Plain, &[]),
                (Token::Input, &[]),
                (Token::Answer, &[]),
                (Token::Thinking, &[90]),
                (Token::Tool, &[]),
                (Token::Error, &[]),
                (Token::Muted, &[]),
                (Token::Accent, &[]),
            ],
        )
    }
}
