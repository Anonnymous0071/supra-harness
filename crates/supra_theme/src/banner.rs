use supra_ffi::width::{self, Ambiguous};

/// The harness wordmark, rendered responsively: wide terminals get the
/// full block art, narrow ones get the word, and both fit.
const WORDMARK_WIDE: [&str; 3] = [
    " ____                       _   ",
    "/ ___| _   _  ___ _ __ __ _| |_ ",
    "\\___ \\| | | |/ _ \\ '__/ _` | __|",
];

/// Render the banner for a terminal `cols` wide under `ambiguous`,
/// measuring every line: a banner that assumes its own width is the
/// first thing a narrow terminal scrolls off.
///
/// Wide form when every line fits; the plain word otherwise. The word
/// itself is ASCII, so its cell cost never depends on the locale.
#[must_use]
pub fn banner(cols: usize, ambiguous: Ambiguous) -> Vec<String> {
    let wide_fits = WORDMARK_WIDE.iter().all(|line| width::width(line.as_bytes(), ambiguous) <= cols);
    if wide_fits && cols >= 20 {
        return WORDMARK_WIDE.iter().map(|line| (*line).to_owned()).collect();
    }
    vec![String::from("supra")]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_wide_terminal_gets_the_block_art() {
        let lines = banner(80, Ambiguous::Wide);
        assert_eq!(lines.len(), 3);
        assert!(lines[0].starts_with(" ____"), "{lines:?}");
    }

    #[test]
    fn a_narrow_terminal_gets_the_word() {
        let lines = banner(16, Ambiguous::Wide);
        assert_eq!(lines, vec![String::from("supra")]);
    }

    #[test]
    fn every_form_fits_the_terminal_it_was_asked_for() {
        for cols in [5, 10, 16, 20, 34, 40, 80] {
            for ambiguous in [Ambiguous::Narrow, Ambiguous::Wide] {
                for line in &banner(cols, ambiguous) {
                    let cells = width::width(line.as_bytes(), ambiguous);
                    assert!(cells <= cols, "cols {cols}, {ambiguous:?}: {line:?} is {cells} cells");
                }
            }
        }
    }

    #[test]
    fn the_wide_art_measures_exactly_under_narrow() {
        for line in &WORDMARK_WIDE {
            let measured = width::width(line.as_bytes(), Ambiguous::Narrow);
            let chars = line.chars().count();
            assert_eq!(measured, chars, "the art is single-width ASCII plus backslash: {line:?}");
        }
    }
}
