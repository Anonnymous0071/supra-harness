use supra_ffi::width::{self, Ambiguous};

/// One gauge's glyph pair, measured.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GaugeGlyphs {
    /// The filled cell.
    pub filled: char,
    /// The empty cell.
    pub empty: char,
}

impl GaugeGlyphs {
    /// The obvious pairing from the architecture document: U+2588 FULL
    /// BLOCK against U+2591 LIGHT SHADE.
    pub const BLOCK_SHADE: Self = Self { filled: '\u{2588}', empty: '\u{2591}' };

    /// A same-class fallback: both Neutral, so neither doubles under a
    /// CJK locale. `#` and `.` from the portable set.
    pub const PORTABLE: Self = Self { filled: '#', empty: '.' };

    /// Measure each glyph individually under `ambiguous`, returning
    /// `(filled_cells, empty_cells)`.
    ///
    /// The T2 finding is the reason this exists: the Block Elements
    /// range is not one width class - U+2588 and U+2593 are Ambiguous
    /// while U+2590..U+2591 are Neutral - so the obvious pairing
    /// mixes classes and the gauge silently changes length under a CJK
    /// locale. Probing each glyph separately is the only honest
    /// measurement.
    #[must_use]
    pub fn cells(self, ambiguous: Ambiguous) -> (usize, usize) {
        let filled = width::char_width(self.filled as u32, ambiguous).cells();
        let empty = width::char_width(self.empty as u32, ambiguous).cells();
        (filled, empty)
    }

    /// Whether the pair holds one width class under `ambiguous` - the
    /// property a gauge needs: if the filled cell doubles and the empty
    /// does not, the bar changes length when the locale does.
    #[must_use]
    pub fn is_stable(self, ambiguous: Ambiguous) -> bool {
        self.cells(ambiguous) == self.cells(Ambiguous::Narrow) && {
            let (f, e) = self.cells(ambiguous);
            f == e
        }
    }

    /// Render a ratio as `filled_cells * on` then `empty_cells * off`,
    /// `on + off` cells total. Refuses fractions of a cell: `on + off`
    /// exceeds `total` truncates from the empty side, never the filled
    /// side - a gauge that under-reports is a gauge that lies.
    #[must_use]
    pub fn render(self, on: usize, off: usize) -> String {
        let mut out = String::new();
        for _ in 0..on {
            out.push(self.filled);
        }
        for _ in 0..off {
            out.push(self.empty);
        }
        out
    }
}

/// Pick a stable gauge pair for `ambiguous`: the block/shade pairing
/// when it holds one class, the portable same-class pair otherwise.
#[must_use]
pub fn gauge_for(ambiguous: Ambiguous) -> GaugeGlyphs {
    if GaugeGlyphs::BLOCK_SHADE.is_stable(ambiguous) {
        GaugeGlyphs::BLOCK_SHADE
    } else {
        GaugeGlyphs::PORTABLE
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_block_shade_pair_mixes_width_classes_under_wide_ambiguous() {
        let (filled, empty) = GaugeGlyphs::BLOCK_SHADE.cells(Ambiguous::Wide);
        assert_eq!(filled, 2, "U+2588 is Ambiguous: doubles under Wide");
        assert_eq!(empty, 1, "U+2591 is Neutral: does not");
        assert!(!GaugeGlyphs::BLOCK_SHADE.is_stable(Ambiguous::Wide));
    }

    #[test]
    fn the_pair_is_stable_under_narrow() {
        let (filled, empty) = GaugeGlyphs::BLOCK_SHADE.cells(Ambiguous::Narrow);
        assert_eq!(filled, 1);
        assert_eq!(empty, 1);
        assert!(GaugeGlyphs::BLOCK_SHADE.is_stable(Ambiguous::Narrow));
    }

    #[test]
    fn gauge_selection_falls_back_to_a_stable_pair() {
        assert_eq!(gauge_for(Ambiguous::Narrow), GaugeGlyphs::BLOCK_SHADE);
        assert_eq!(gauge_for(Ambiguous::Wide), GaugeGlyphs::PORTABLE);
        let (filled, empty) = GaugeGlyphs::PORTABLE.cells(Ambiguous::Wide);
        assert_eq!((filled, empty), (1, 1), "the fallback never doubles");
    }

    #[test]
    fn the_portable_pair_is_always_stable() {
        for ambiguous in [Ambiguous::Narrow, Ambiguous::Wide] {
            assert!(GaugeGlyphs::PORTABLE.is_stable(ambiguous));
        }
    }

    #[test]
    fn rendering_truncates_from_the_empty_side() {
        let gauge = GaugeGlyphs::PORTABLE;
        assert_eq!(gauge.render(3, 2), "###..");
        assert_eq!(gauge.render(0, 5), ".....");
        assert_eq!(gauge.render(5, 0), "#####");
    }
}
