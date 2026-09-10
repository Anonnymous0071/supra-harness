use supra_theme::{Theme, Token};

/// One status-line segment. Priority decides the shed order: the shed
/// drops the lowest priority first, and a segment the architecture
/// never sheds is marked with the highest priority.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StatusSegment {
    /// Shed priority: shed from the highest number down. `0` never
    /// sheds.
    pub shed_at: u8,
    /// The rendered text.
    pub text: String,
    /// Which theme token colours it.
    pub token: Token,
}

/// The live cost estimate. The tilde is the design: usage fields are
/// final only after the stream ends, so an unreconciled estimate says
/// so.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cost {
    /// Thousandths of a dollar.
    pub mills: u64,
    /// Whether the figure is still an estimate.
    pub estimated: bool,
}

impl Cost {
    /// Format as the architecture's shape: `+$0.014~` while estimated,
    /// `+$0.014` once reconciled.
    #[must_use]
    pub fn format(self) -> String {
        let dollars = self.mills / 1000;
        let mills = self.mills % 1000;
        let tilde = if self.estimated { "~" } else { "" };
        format!("+${dollars}.{mills:03}{tilde}")
    }
}

/// The status line: segments shed from the lowest priority as the
/// terminal narrows. Five segments never shed - context %, cache %,
/// session spend, the cache-break marker, and the permission mode.
#[derive(Clone, Debug, Default)]
pub struct StatusLine {
    segments: Vec<StatusSegment>,
}

/// The never-shed priorities, per section 7.
pub const NEVER_SHED: u8 = 0;
/// The first sheddable priority.
pub const FIRST_SHED: u8 = 1;

impl StatusLine {
    /// Build from segments, in display order.
    #[must_use]
    pub fn new(segments: Vec<StatusSegment>) -> Self {
        Self { segments }
    }

    /// Build the canonical line from live state.
    #[must_use]
    #[allow(clippy::too_many_arguments)]
    pub fn live(
        mode: &str,
        context_percent: u32,
        cache_percent: u32,
        cost: Cost,
        cache_broken: bool,
        model: &str,
        telemetry_on: bool,
        missed_events: u64,
        hook_count: usize,
    ) -> Self {
        let mut segments = Vec::new();
        segments.push(StatusSegment {
            shed_at: NEVER_SHED,
            text: format!("ctx {context_percent}%"),
            token: Token::Muted,
        });
        segments.push(StatusSegment {
            shed_at: NEVER_SHED,
            text: format!("cache {cache_percent}%"),
            token: Token::Muted,
        });
        segments.push(StatusSegment { shed_at: NEVER_SHED, text: cost.format(), token: Token::Accent });
        if cache_broken {
            segments.push(StatusSegment {
                shed_at: NEVER_SHED,
                text: "cache broke".to_owned(),
                token: Token::Error,
            });
        }
        segments.push(StatusSegment {
            shed_at: NEVER_SHED,
            text: format!("mode {mode}"),
            token: Token::Input,
        });
        segments.push(StatusSegment { shed_at: FIRST_SHED, text: model.to_owned(), token: Token::Plain });
        if missed_events > 0 {
            segments.push(StatusSegment {
                shed_at: FIRST_SHED,
                text: format!("missed {missed_events}"),
                token: Token::Error,
            });
        }
        if telemetry_on {
            segments.push(StatusSegment {
                shed_at: FIRST_SHED + 1,
                text: "telemetry on".to_owned(),
                token: Token::Muted,
            });
        }
        if hook_count > 0 {
            segments.push(StatusSegment {
                shed_at: FIRST_SHED + 1,
                text: format!("{hook_count} hooks"),
                token: Token::Muted,
            });
        }
        Self { segments }
    }

    /// Render at `cols` cells wide: shed lowest-priority segments until
    /// the line fits, then paint through the theme. A segment that
    /// still does not fit alone truncates.
    #[must_use]
    pub fn render(&self, cols: usize, theme: &Theme) -> String {
        if cols == 0 {
            return String::new();
        }
        let measured: Vec<(u8, usize, &StatusSegment)> = self
            .segments
            .iter()
            .map(|segment| (segment.shed_at, width_of(&segment.text, theme), segment))
            .collect();

        let mut keep = measured;
        loop {
            let separators = keep.len().saturating_sub(1).saturating_mul(2);
            let total = keep.iter().fold(separators, |sum, (_, cells, _)| sum.saturating_add(*cells));
            if total <= cols || keep.iter().all(|(shed_at, _, _)| *shed_at == NEVER_SHED) {
                break;
            }
            let Some(highest) = keep
                .iter()
                .filter(|(shed_at, _, _)| *shed_at > NEVER_SHED)
                .map(|(shed_at, _, _)| *shed_at)
                .max()
            else {
                break;
            };
            keep.retain(|(shed_at, _, _)| *shed_at != highest);
        }

        let mut out = String::new();
        let mut remaining = cols;
        for (_, _, segment) in keep {
            if remaining == 0 {
                break;
            }
            if !out.is_empty() {
                if remaining <= 2 {
                    break;
                }
                out.push_str("  ");
                remaining -= 2;
            }
            let truncated = supra_ffi::width::truncate(segment.text.as_bytes(), remaining, theme.ambiguous);
            let text = String::from_utf8_lossy(&segment.text.as_bytes()[..truncated.bytes]);
            if text.is_empty() {
                break;
            }
            out.push_str(&theme.paint(segment.token, &text));
            remaining = remaining.saturating_sub(truncated.cells);
        }
        out
    }

    /// Every segment, unshed - for tests and for the wide render.
    #[must_use]
    pub fn segments(&self) -> &[StatusSegment] {
        &self.segments
    }
}

fn width_of(text: &str, theme: &Theme) -> usize {
    supra_ffi::width::width(text.as_bytes(), theme.ambiguous)
}

#[cfg(test)]
mod tests {
    use super::*;
    use supra_ffi::width::Ambiguous;
    use supra_theme::Theme;

    fn theme() -> Theme {
        Theme::default_dark()
    }

    #[test]
    fn a_cost_estimate_carries_a_tilde_and_a_reconciled_cost_does_not() {
        assert_eq!(Cost { mills: 14, estimated: true }.format(), "+$0.014~");
        assert_eq!(Cost { mills: 1400, estimated: false }.format(), "+$1.400");
        assert_eq!(Cost { mills: 0, estimated: false }.format(), "+$0.000");
    }

    #[test]
    fn live_contains_the_five_never_shed_segments() {
        let line = StatusLine::live(
            "auto",
            40,
            90,
            Cost { mills: 14, estimated: true },
            true,
            "claude-sonnet-4",
            false,
            0,
            0,
        );
        let texts: Vec<_> = line.segments().iter().map(|s| s.text.as_str()).collect();
        assert!(texts.iter().any(|t| t.contains("ctx 40%")), "ctx {texts:?}");
        assert!(texts.iter().any(|t| t.contains("cache 90%")), "cache {texts:?}");
        assert!(texts.iter().any(|t| t.contains("+$0.014~")), "spend {texts:?}");
        assert!(texts.iter().any(|t| t.contains("cache broke")), "cache-break {texts:?}");
        assert!(texts.iter().any(|t| t.contains("mode auto")), "mode {texts:?}");
        for seg in line.segments().iter().filter(|s| {
            s.text.contains("ctx")
                || s.text.contains("cache")
                || s.text.contains("+$")
                || s.text.contains("cache broke")
                || s.text.starts_with("mode ")
        }) {
            assert_eq!(seg.shed_at, NEVER_SHED, "never-shed segment has wrong priority: {seg:?}");
        }
    }

    #[test]
    fn the_never_shed_segments_keep_priority_on_a_narrow_terminal() {
        let line = StatusLine::live(
            "auto",
            40,
            90,
            Cost { mills: 14, estimated: true },
            true,
            "claude-sonnet-4",
            false,
            0,
            0,
        );
        let rendered = line.render(12, &theme());
        let mut plain = Vec::new();
        supra_ffi::ansi::strip(rendered.as_bytes(), &mut plain);
        assert!(plain.starts_with(b"ctx 40%"), "first never-shed segment survives: {rendered:?}");
        let cells = supra_ffi::width::width(&plain, Ambiguous::Narrow);
        assert!(cells <= 12, "narrow status is {cells} cells: {rendered:?}");
    }

    #[test]
    fn sheddable_segments_vanish_before_never_shed() {
        let line = StatusLine::live(
            "auto",
            40,
            90,
            Cost { mills: 14, estimated: true },
            false,
            "some-very-long-model-name-that-exceeds-width",
            true,
            5,
            2,
        );
        let wide = line.render(300, &theme());
        assert!(wide.contains("some-very-long-model-name"), "wide shows model");
        assert!(wide.contains("telemetry on"), "wide shows telemetry");
        let narrow = line.render(20, &theme());
        assert!(!narrow.contains("telemetry on"), "narrow sheds telemetry first: {narrow:?}");
        assert!(!narrow.contains("some-very-long-model-name"), "narrow sheds model: {narrow:?}");
        assert!(narrow.contains("ctx 40%"), "never-shed survives: {narrow:?}");
    }

    #[test]
    fn a_truncated_segment_is_cut_not_dropped() {
        let line = StatusLine::new(vec![StatusSegment {
            shed_at: NEVER_SHED,
            text: "x".repeat(100),
            token: Token::Plain,
        }]);
        let rendered = line.render(4, &theme());
        assert!(!rendered.is_empty(), "single over-wide never-shed truncates rather than vanishes");
        let cells = supra_ffi::width::width(rendered.as_bytes(), Ambiguous::Narrow);
        assert!(cells <= 4, "truncated to {rendered:?} is {cells} cells");
        assert_eq!(rendered.chars().count(), 4, "truncated text length");
    }

    #[test]
    fn every_status_fits_the_global_width_budget() {
        let segments = vec![
            StatusSegment { shed_at: NEVER_SHED, text: "abcde".to_owned(), token: Token::Plain },
            StatusSegment { shed_at: NEVER_SHED, text: "fghij".to_owned(), token: Token::Plain },
            StatusSegment { shed_at: FIRST_SHED, text: "optional".to_owned(), token: Token::Plain },
        ];
        for cols in 0..=20 {
            for ambiguous in [Ambiguous::Narrow, Ambiguous::Wide] {
                let th = Theme::new(
                    "probe",
                    ambiguous,
                    [
                        (Token::Plain, &[]),
                        (Token::Input, &[]),
                        (Token::Answer, &[]),
                        (Token::Thinking, &[]),
                        (Token::Tool, &[]),
                        (Token::Error, &[]),
                        (Token::Muted, &[]),
                        (Token::Accent, &[]),
                    ],
                );
                let rendered = StatusLine::new(segments.clone()).render(cols, &th);
                let cells = supra_ffi::width::width(rendered.as_bytes(), ambiguous);
                assert!(cells <= cols, "cols {cols} amb {ambiguous:?}: {rendered:?} is {cells}");
            }
        }
    }

    #[test]
    fn single_wide_segments_never_exceed_their_cols() {
        for cols in [10, 20, 40, 80] {
            for amb in [Ambiguous::Narrow, Ambiguous::Wide] {
                let line = StatusLine::new(vec![StatusSegment {
                    shed_at: NEVER_SHED,
                    text: "x".repeat(cols * 2),
                    token: Token::Plain,
                }]);
                let th = Theme::new(
                    "probe",
                    amb,
                    [
                        (Token::Plain, &[]),
                        (Token::Input, &[]),
                        (Token::Answer, &[]),
                        (Token::Thinking, &[]),
                        (Token::Tool, &[]),
                        (Token::Error, &[]),
                        (Token::Muted, &[]),
                        (Token::Accent, &[]),
                    ],
                );
                let rendered = line.render(cols, &th);
                let cells = supra_ffi::width::width(rendered.as_bytes(), amb);
                assert!(cells <= cols, "cols {cols} amb {amb:?} rendered {cells} cells: {rendered:?}");
            }
        }
    }
}
