use supra_ffi::width::Ambiguous;

/// A semantic colour role. The TUI asks for roles; the theme answers
/// with bytes. No component embeds an escape sequence.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Token {
    /// Ordinary text.
    Plain,
    /// The user's prompt echo.
    Input,
    /// The model's answer.
    Answer,
    /// Reasoning, hidden behind ctrl+o by default.
    Thinking,
    /// A tool invocation line.
    Tool,
    /// An error or refusal.
    Error,
    /// A muted detail: timestamps, counts.
    Muted,
    /// The status line's accent.
    Accent,
}

impl Token {
    /// All tokens, in a stable order a theme table can be written
    /// against.
    pub const ALL: [Self; 8] = [
        Self::Plain,
        Self::Input,
        Self::Answer,
        Self::Thinking,
        Self::Tool,
        Self::Error,
        Self::Muted,
        Self::Accent,
    ];
}

/// A complete theme: one SGR sequence per token, plus the ambiguous
/// width resolution the glyphs were measured under.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Theme {
    /// The theme's name.
    pub name: &'static str,
    /// How East Asian Ambiguous code points resolve. A theme is a
    /// locale decision as much as a colour decision: the same gauge
    /// doubles its cells under a CJK choice and not otherwise.
    pub ambiguous: Ambiguous,
    /// SGR parameters per token; empty means terminal default.
    sequences: [Vec<u16>; 8],
}

impl Theme {
    /// Build a theme from per-token SGR parameter lists.
    #[must_use]
    pub fn new(name: &'static str, ambiguous: Ambiguous, sequences: [(Token, &[u16]); 8]) -> Self {
        let mut table =
            [Vec::new(), Vec::new(), Vec::new(), Vec::new(), Vec::new(), Vec::new(), Vec::new(), Vec::new()];
        for (token, params) in sequences {
            table[token.index()] = params.to_vec();
        }
        Self { name, ambiguous, sequences: table }
    }

    /// The default dark theme: eight tokens, ANSI colours only, so the
    /// first render never depends on 24-bit support.
    #[must_use]
    pub fn default_dark() -> Self {
        Self::new(
            "dark",
            Ambiguous::Narrow,
            [
                (Token::Plain, &[]),
                (Token::Input, &[36]),
                (Token::Answer, &[]),
                (Token::Thinking, &[90]),
                (Token::Tool, &[33]),
                (Token::Error, &[31]),
                (Token::Muted, &[90]),
                (Token::Accent, &[32]),
            ],
        )
    }

    /// The escape sequence that starts a token's colouring, empty for
    /// the terminal default.
    #[must_use]
    pub fn start(&self, token: Token) -> String {
        let params = &self.sequences[token.index()];
        if params.is_empty() {
            return String::new();
        }
        let joined = params.iter().map(u16::to_string).collect::<Vec<_>>().join(";");
        format!("\x1b[{joined}m")
    }

    /// The reset sequence, shared by every token.
    #[must_use]
    pub fn reset(&self) -> &'static str {
        "\x1b[0m"
    }

    /// Wrap text in a token's colouring. An empty sequence wraps bare -
    /// the terminal default is the plain case, not an error.
    #[must_use]
    pub fn paint(&self, token: Token, text: &str) -> String {
        let start = self.start(token);
        if start.is_empty() {
            return text.to_owned();
        }
        format!("{start}{text}{}\x1b[0m", "")
    }
}

impl Token {
    /// The token's index in the theme table.
    const fn index(self) -> usize {
        match self {
            Self::Plain => 0,
            Self::Input => 1,
            Self::Answer => 2,
            Self::Thinking => 3,
            Self::Tool => 4,
            Self::Error => 5,
            Self::Muted => 6,
            Self::Accent => 7,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_token_paints_or_wraps_bare() {
        let theme = Theme::default_dark();
        for token in Token::ALL {
            let painted = theme.paint(token, "x");
            if theme.start(token).is_empty() {
                assert_eq!(painted, "x", "{token:?} is terminal default");
            } else {
                assert!(painted.starts_with("\x1b["), "{token:?}: {painted:?}");
                assert!(painted.ends_with("\x1b[0m"), "{token:?}: {painted:?}");
            }
        }
    }

    #[test]
    fn the_dark_theme_uses_ansi_only() {
        let theme = Theme::default_dark();
        for token in Token::ALL {
            let start = theme.start(token);
            assert!(
                start.is_empty() || (start.starts_with("\x1b[") && start.ends_with('m')),
                "{token:?}: {start:?}"
            );
            assert!(!start.contains(";38;2;"), "{token:?}: 24-bit in the default theme");
        }
    }

    #[test]
    fn paint_round_trips_the_text() {
        let theme = Theme::default_dark();
        let painted = theme.paint(Token::Error, "boom");
        assert!(painted.contains("boom"));
        assert_eq!(theme.start(Token::Error), "\x1b[31m");
        assert_eq!(theme.reset(), "\x1b[0m");
    }
}
