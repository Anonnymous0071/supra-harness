/// Ten braille frames, the throb the TUI shows while waiting.
/// Update every ~80ms; a different frame every tick.
pub const SPINNER_FRAMES: [&str; 10] = [
    "\u{280B}", "\u{2819}", "\u{2839}", "\u{2838}", "\u{283C}", "\u{2834}", "\u{2826}", "\u{2827}",
    "\u{2807}", "\u{280F}",
];

/// A spinner advancing one frame per tick.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Spinner {
    tick: usize,
}

impl Spinner {
    /// Reset to the first frame.
    #[must_use]
    pub const fn new() -> Self {
        Self { tick: 0 }
    }

    /// Advance; return the next frame string.
    #[must_use]
    #[allow(clippy::should_implement_trait)]
    pub fn next(&mut self) -> &'static str {
        let frame = SPINNER_FRAMES[self.tick % SPINNER_FRAMES.len()];
        self.tick = self.tick.wrapping_add(1);
        frame
    }
}

impl Default for Spinner {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ten_frames_cycle_and_never_repeat_consecutively() {
        let mut spinner = Spinner::new();
        let first = spinner.next();
        for _ in 1..SPINNER_FRAMES.len() {
            assert_ne!(spinner.next(), first);
        }
        assert_eq!(spinner.next(), first);
    }

    #[test]
    fn wrapping_advances_deterministically() {
        let mut spinner = Spinner { tick: usize::MAX };
        let frame = spinner.next();
        assert!(!frame.is_empty());
    }
}
