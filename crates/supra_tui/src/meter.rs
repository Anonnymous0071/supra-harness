use supra_ffi::width::Ambiguous;
use supra_theme::gauge_for;

/// A ratio meter built from the theme's stable gauge pair. Never a
/// fixed cell cost: the pair is probed for the locale, and the cell
/// count is measured, not assumed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Meter {
    /// Total cells the bar occupies.
    pub total: usize,
    /// The ambiguous resolution the gauge was selected for.
    pub ambiguous: Ambiguous,
}

impl Meter {
    /// A meter `total` cells wide.
    #[must_use]
    pub const fn new(total: usize, ambiguous: Ambiguous) -> Self {
        Self { total, ambiguous }
    }

    /// Render `part` of `whole` as a bar. The cell cost is measured
    /// from the selected pair, so a CJK locale never changes the bar's
    /// length mid-render.
    #[must_use]
    pub fn render(&self, part: usize, whole: usize) -> String {
        let gauge = gauge_for(self.ambiguous);
        let (filled_cells, empty_cells) = gauge.cells(self.ambiguous);
        let per_cell = filled_cells.max(empty_cells).max(1);
        let slots = self.total / per_cell;
        let Some(non_zero) = std::num::NonZeroUsize::new(whole) else {
            return gauge.render(0, slots);
        };
        #[allow(clippy::manual_checked_ops)]
        let on = (part.min(whole) * slots) / non_zero.get();
        let off = slots.saturating_sub(on);
        gauge.render(on, off)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_half_meter_renders_half_filled() {
        let meter = Meter::new(10, Ambiguous::Narrow);
        let bar = meter.render(5, 10);
        assert_eq!(bar.chars().count(), 10);
        assert_eq!(bar.chars().filter(|c| *c == '\u{2588}').count(), 5);
    }

    #[test]
    fn a_full_meter_is_all_filled() {
        let meter = Meter::new(10, Ambiguous::Narrow);
        let bar = meter.render(10, 10);
        assert!(bar.chars().all(|c| c == '\u{2588}'));
    }

    #[test]
    fn an_empty_whole_renders_all_empty() {
        let meter = Meter::new(10, Ambiguous::Narrow);
        let bar = meter.render(0, 0);
        assert!(bar.chars().all(|c| c == '\u{2591}'));
    }

    #[test]
    fn part_over_whole_never_exceeds_the_bar() {
        let meter = Meter::new(10, Ambiguous::Narrow);
        assert_eq!(meter.render(15, 10).chars().count(), 10);
        assert_eq!(meter.render(100, 7).chars().count(), 10);
    }

    #[test]
    fn the_wide_locale_selects_the_stable_pair_so_length_holds() {
        let meter = Meter::new(20, Ambiguous::Wide);
        let bar = meter.render(10, 20);
        let cells = supra_ffi::width::width(bar.as_bytes(), Ambiguous::Wide);
        assert!(cells <= 20, "wide bar measured {cells} cells");
    }

    #[test]
    fn the_wide_locale_uses_the_portable_pair_not_the_block_pair() {
        let meter = Meter::new(10, Ambiguous::Wide);
        let bar = meter.render(5, 10);
        assert!(bar.contains('#') || bar.contains('.'), "wide must use portable #/. not block: {bar:?}");
        assert!(!bar.contains('\u{2588}'), "wide must not use U+2588 which is Ambiguous: {bar:?}");
        assert!(!bar.contains('\u{2591}'), "wide must not use U+2591: {bar:?}");
        let cells = supra_ffi::width::width(bar.as_bytes(), Ambiguous::Wide);
        assert_eq!(cells, 10, "portable pair is 1 cell each, so width equals slots: {bar:?} {cells}");
    }

    #[test]
    fn narrow_uses_the_block_pair() {
        let meter = Meter::new(10, Ambiguous::Narrow);
        let bar = meter.render(5, 10);
        assert!(bar.contains('\u{2588}'), "narrow uses block: {bar:?}");
    }
}
