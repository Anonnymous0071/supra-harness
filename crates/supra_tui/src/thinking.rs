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

/// Renders the thinking display per state.
#[derive(Clone, Copy, Debug)]
pub struct ThinkingDisplay {
    /// Seconds the stream took, for the `Thought for Ns` line.
    pub seconds: u64,
}

impl ThinkingDisplay {
    /// Render one line for the state.
    #[must_use]
    pub fn render(&self, state: ThinkingState) -> String {
        match state {
            ThinkingState::Streaming => String::from("\u{2235} Thinking\u{2026}"),
            ThinkingState::Collapsed => {
                format!("\u{2234} Thought for {}s (ctrl+o to expand)", self.seconds)
            }
            ThinkingState::Expanded => {
                format!("\u{2234} Thought for {}s (ctrl+o to collapse)", self.seconds)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn streaming_shows_the_thinking_glyph() {
        let display = ThinkingDisplay { seconds: 0 };
        assert_eq!(display.render(ThinkingState::Streaming), "\u{2235} Thinking\u{2026}");
    }

    #[test]
    fn collapsed_shows_the_thought_glyph_and_the_expand_hint() {
        let display = ThinkingDisplay { seconds: 84 };
        let line = display.render(ThinkingState::Collapsed);
        assert!(line.starts_with("\u{2234} Thought for 84s"), "{line}");
        assert!(line.contains("(ctrl+o to expand)"), "{line}");
    }

    #[test]
    fn expanded_shows_the_collapse_hint() {
        let display = ThinkingDisplay { seconds: 3 };
        let line = display.render(ThinkingState::Expanded);
        assert!(line.starts_with("\u{2234} Thought for 3s"), "{line}");
        assert!(line.contains("(ctrl+o to collapse)"), "{line}");
    }

    #[test]
    fn the_display_never_carries_a_cost() {
        let display = ThinkingDisplay { seconds: 100 };
        for state in [ThinkingState::Streaming, ThinkingState::Collapsed, ThinkingState::Expanded] {
            assert!(!display.render(state).contains('$'), "no cost preview");
        }
    }
}
