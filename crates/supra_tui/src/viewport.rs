/// A virtual scroll viewport over `total` lines, showing `visible`
/// cells tall. Position is stored as the first visible index; the
/// renderer clamps it to the bound and formats the gutter.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Viewport {
    /// Total lines the transcript holds.
    pub total: usize,
    /// Lines the gutter can show without scrolling.
    pub visible: usize,
    /// First visible line, zero-based.
    pub offset: usize,
}

impl Viewport {
    /// Create a viewport. `offset` clamps; `visible == 0` means no
    /// viewport and renders the empty prefix, so a zero-size terminal is
    /// not a division-by-zero.
    #[must_use]
    pub fn new(total: usize, visible: usize, offset: usize) -> Self {
        Self { total, visible, offset: offset.min(max_offset(total, visible)) }
    }

    /// The half-open range of visible indices, clamped.
    #[must_use]
    pub fn visible_range(&self) -> std::ops::Range<usize> {
        let start = self.offset.min(self.total);
        let end = self.total.min(start.saturating_add(self.visible));
        start..end
    }

    /// Whether the viewport is scrolled away from the bottom.
    #[must_use]
    pub fn is_scrolled(&self) -> bool {
        self.offset < max_offset(self.total, self.visible)
    }
}

fn max_offset(total: usize, visible: usize) -> usize {
    total.saturating_sub(visible)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_visible_range_clamps_to_the_total() {
        assert_eq!(Viewport::new(100, 10, 5).visible_range(), 5..15);
        assert_eq!(Viewport::new(5, 10, 0).visible_range(), 0..5);
        assert_eq!(Viewport::new(0, 10, 100).visible_range(), 0..0);
        assert_eq!(Viewport::new(100, 0, 7).visible_range(), 7..7);
    }

    #[test]
    fn the_scroll_position_is_clamped_and_reported() {
        let viewport = Viewport::new(100, 10, 0);
        assert_eq!(viewport.visible_range(), 0..10);
        assert!(viewport.is_scrolled(), "a short offset is not the bottom");
        let clamped = Viewport::new(100, 10, 200);
        assert_eq!(clamped.offset, 90, "a far offset clamps, and the clamped position is the bottom");
        assert!(!clamped.is_scrolled());
        assert!(!Viewport::new(100, 10, 90).is_scrolled());
        assert!(!Viewport::new(10, 20, 0).is_scrolled());
        assert!(!Viewport::new(0, 0, 7).is_scrolled());
    }

    #[test]
    fn visible_range_is_clamped_even_when_constructed_directly() {
        let vp = Viewport { total: 5, visible: 2, offset: 10 };
        let range = vp.visible_range();
        assert!(range.start <= vp.total, "start {range:?} must be within total {}", vp.total);
        assert!(range.end <= vp.total, "end {range:?} must be within total {}", vp.total);
        assert!(range.start <= range.end, "range must not be inverted: {range:?}");
        assert_eq!(range, 5..5, "offset beyond total clamps to empty tail");
    }

    #[test]
    fn an_out_of_bounds_offset_does_not_panic_and_reports_scrolled() {
        let vp = Viewport { total: 3, visible: 10, offset: 100 };
        assert_eq!(vp.visible_range(), 3..3);
        assert!(
            vp.is_scrolled() || vp.total <= vp.visible,
            "out of bounds is not the bottom unless total fits"
        );
        let vp2 = Viewport { total: 100, visible: 10, offset: 90 };
        assert!(!vp2.is_scrolled());
        assert_eq!(vp2.visible_range(), 90..100);
    }
}
