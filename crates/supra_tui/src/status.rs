use supra_theme::{Theme, Token};

const FULL_SEPARATOR: &str = "  ";
const COMPACT_SEPARATOR: &str = " ";

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
/// terminal narrows. `NEVER_SHED` segments are never removed by that
/// priority pass; canonical lines use compact forms before the physical
/// width limit truncates the complete protected set.
#[derive(Clone, Debug, Default)]
pub struct StatusLine {
    segments: Vec<StatusSegment>,
    compact_segments: Option<Vec<StatusSegment>>,
}

/// Protect a segment from priority shedding. Physical widths below the
/// compact protected set's minimum still truncate the complete line.
pub const NEVER_SHED: u8 = 0;
/// The first sheddable priority.
pub const FIRST_SHED: u8 = 1;

impl StatusLine {
    /// Build from segments, in display order.
    #[must_use]
    pub fn new(segments: Vec<StatusSegment>) -> Self {
        Self { segments, compact_segments: None }
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
        let mut compact_segments = Vec::new();
        segments.push(StatusSegment {
            shed_at: NEVER_SHED,
            text: format!("ctx {context_percent}%"),
            token: Token::Muted,
        });
        compact_segments.push(StatusSegment {
            shed_at: NEVER_SHED,
            text: format!("c{context_percent}"),
            token: Token::Muted,
        });
        segments.push(StatusSegment {
            shed_at: NEVER_SHED,
            text: format!("cache {cache_percent}%"),
            token: Token::Muted,
        });
        compact_segments.push(StatusSegment {
            shed_at: NEVER_SHED,
            text: format!("h{cache_percent}"),
            token: Token::Muted,
        });
        segments.push(StatusSegment { shed_at: NEVER_SHED, text: cost.format(), token: Token::Accent });
        compact_segments.push(StatusSegment {
            shed_at: NEVER_SHED,
            text: compact_cost(cost),
            token: Token::Accent,
        });
        if cache_broken {
            segments.push(StatusSegment {
                shed_at: NEVER_SHED,
                text: "cache broke".to_owned(),
                token: Token::Error,
            });
            compact_segments.push(StatusSegment {
                shed_at: NEVER_SHED,
                text: "!".to_owned(),
                token: Token::Error,
            });
        }
        segments.push(StatusSegment {
            shed_at: NEVER_SHED,
            text: format!("mode {mode}"),
            token: Token::Input,
        });
        compact_segments.push(StatusSegment {
            shed_at: NEVER_SHED,
            text: compact_mode(mode),
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
        Self { segments, compact_segments: Some(compact_segments) }
    }

    /// Render at `cols` cells wide. Optional segments shed first;
    /// canonical lines then switch to compact protected labels before
    /// ANSI-safe painting and final cell-bounded truncation.
    #[must_use]
    pub fn render(&self, cols: usize, theme: &Theme) -> String {
        if cols == 0 {
            return String::new();
        }
        let full = select_segments(&self.segments, cols, FULL_SEPARATOR, theme);
        if fits(&full, cols, FULL_SEPARATOR) {
            return render_segments(&full, cols, FULL_SEPARATOR, theme);
        }
        let Some(compact) = &self.compact_segments else {
            return render_segments(&full, cols, FULL_SEPARATOR, theme);
        };
        let compact = measured(compact, theme);
        render_segments(&compact, cols, COMPACT_SEPARATOR, theme)
    }

    /// Every segment, unshed - for tests and for the wide render.
    #[must_use]
    pub fn segments(&self) -> &[StatusSegment] {
        &self.segments
    }
}

fn compact_cost(cost: Cost) -> String {
    let suffix = if cost.estimated { "~" } else { "" };
    if cost.mills < 1000 {
        format!("${}m{suffix}", cost.mills)
    } else {
        let dollars = cost.mills / 1000;
        let tenths = cost.mills % 1000 / 100;
        format!("${dollars}.{tenths}{suffix}")
    }
}

fn compact_mode(mode: &str) -> String {
    let label = match mode {
        "plan" => "P",
        "ask" => "A",
        "auto" => "U",
        "yolo" => "Y",
        _ => mode.get(..1).unwrap_or("?"),
    };
    format!("m{label}")
}

fn measured<'a>(segments: &'a [StatusSegment], theme: &Theme) -> Vec<(u8, usize, &'a StatusSegment)> {
    segments.iter().map(|segment| (segment.shed_at, width_of(&segment.text, theme), segment)).collect()
}

fn select_segments<'a>(
    segments: &'a [StatusSegment],
    cols: usize,
    separator: &str,
    theme: &Theme,
) -> Vec<(u8, usize, &'a StatusSegment)> {
    let mut keep = measured(segments, theme);
    while !fits(&keep, cols, separator) && keep.iter().any(|(shed_at, _, _)| *shed_at > NEVER_SHED) {
        let highest =
            keep.iter().filter(|(shed_at, _, _)| *shed_at > NEVER_SHED).map(|(shed_at, _, _)| *shed_at).max();
        let Some(highest) = highest else { break };
        keep.retain(|(shed_at, _, _)| *shed_at != highest);
    }
    keep
}

fn fits(segments: &[(u8, usize, &StatusSegment)], cols: usize, separator: &str) -> bool {
    total_width(segments, separator) <= cols
}

fn total_width(segments: &[(u8, usize, &StatusSegment)], separator: &str) -> usize {
    let separators = segments.len().saturating_sub(1).saturating_mul(separator.len());
    segments.iter().fold(separators, |sum, (_, cells, _)| sum.saturating_add(*cells))
}

fn render_segments(
    segments: &[(u8, usize, &StatusSegment)],
    cols: usize,
    separator: &str,
    theme: &Theme,
) -> String {
    let mut out = String::new();
    let mut remaining = cols;
    for (index, (_, _, segment)) in segments.iter().enumerate() {
        if index > 0 {
            let separator_cells = separator.len();
            if remaining < separator_cells {
                break;
            }
            out.push_str(separator);
            remaining -= separator_cells;
        }
        if remaining == 0 {
            break;
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

    fn plain(rendered: &str) -> String {
        let mut plain = Vec::new();
        supra_ffi::ansi::strip(rendered.as_bytes(), &mut plain);
        String::from_utf8(plain).expect("status output is utf-8")
    }

    #[test]
    fn a_cost_estimate_carries_a_tilde_and_a_reconciled_cost_does_not() {
        assert_eq!(Cost { mills: 14, estimated: true }.format(), "+$0.014~");
        assert_eq!(Cost { mills: 1400, estimated: false }.format(), "+$1.400");
        assert_eq!(Cost { mills: 0, estimated: false }.format(), "+$0.000");
    }

    #[test]
    fn live_contains_the_five_never_shed_segments() {
        let line = canonical(true);
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
    fn compact_never_shed_concepts_share_the_narrow_line() {
        assert_eq!(plain(&canonical(true).render(18, &theme())), "c40 h90 $14m~ ! mU");
        assert_eq!(plain(&canonical(false).render(16, &theme())), "c40 h90 $14m~ mU");
    }

    #[test]
    fn widths_below_the_compact_minimum_truncate_honestly() {
        let line = canonical(true);
        let expected = "c40 h90 $14m~ ! mU";
        for cols in 0..=expected.len() {
            let rendered = line.render(cols, &theme());
            assert!(supra_ffi::ansi::measure(rendered.as_bytes(), Ambiguous::Narrow) <= cols);
            assert_eq!(plain(&rendered), expected[..cols]);
        }
    }

    #[test]
    fn optional_cache_break_changes_the_compact_minimum() {
        assert_eq!(plain(&canonical(false).render(16, &theme())), "c40 h90 $14m~ mU");
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
        assert_eq!(plain(&narrow), "c40 h90 $14m~ mU");
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
                let th = unstyled(ambiguous);
                let rendered = StatusLine::new(segments.clone()).render(cols, &th);
                let cells = supra_ffi::ansi::measure(rendered.as_bytes(), ambiguous);
                assert!(cells <= cols, "cols {cols} amb {ambiguous:?}: {rendered:?} is {cells}");
            }
        }
    }

    #[test]
    fn single_wide_segments_never_exceed_their_cols() {
        for cols in [10, 20, 40, 80] {
            for ambiguous in [Ambiguous::Narrow, Ambiguous::Wide] {
                let line = StatusLine::new(vec![StatusSegment {
                    shed_at: NEVER_SHED,
                    text: "x".repeat(cols * 2),
                    token: Token::Plain,
                }]);
                let rendered = line.render(cols, &unstyled(ambiguous));
                let cells = supra_ffi::ansi::measure(rendered.as_bytes(), ambiguous);
                assert!(cells <= cols, "cols {cols} amb {ambiguous:?} rendered {cells} cells: {rendered:?}");
            }
        }
    }

    fn canonical(cache_broken: bool) -> StatusLine {
        StatusLine::live(
            "auto",
            40,
            90,
            Cost { mills: 14, estimated: true },
            cache_broken,
            "claude-sonnet-4",
            false,
            0,
            0,
        )
    }

    fn unstyled(ambiguous: Ambiguous) -> Theme {
        Theme::new(
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
        )
    }
}
