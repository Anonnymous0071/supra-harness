//! Where the four breakpoints fall, and what each one costs to move.
//!
//! # Breakpoints are positions, not copies
//!
//! A [`Plan`] names four offsets into the ledger's segment sequence: end of `tools`,
//! end of `system`, end of the memory index, end of turn n-1. The provider caches the
//! prefix *up to* each offset; nothing is duplicated, and moving BP4 costs a 5-minute
//! write while moving BP1-BP3 costs an hour write each - which is why only BP4 rolls
//! every turn and the first three move on rewrite alone.
//!
//! # Truncation keeps the frozen entries
//!
//! Under a policy carrying fewer than four entries (T13: one key, not four
//! breakpoints), the plan keeps the earliest offsets. The frozen regions are worth
//! more than the rolling one: BP1 has been read hundreds of times and will be read
//! hundreds more, while BP4 will be rewritten next turn regardless.
//!
//! # Lookback bound
//!
//! The cache-read lookback window is 20 blocks, and a consecutive tool run counts as
//! one position (T6's `lookback_positions`). A breakpoint that falls outside the
//! window is a silent miss rather than an error - so the plan refuses to place one
//! there, and reports *which* breakpoint fell off rather than a bare count.

use supra_types::{Breakpoint, LOOKBACK_BLOCKS, Sealed, Segment};

/// Four offsets into the segment sequence, in prefix order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Plan {
    /// Ledger index one past the last `tools` segment (BP1).
    pub bp1_tools: usize,
    /// Ledger index one past the last `system` segment (BP2).
    pub bp2_system: usize,
    /// Ledger index one past the last memory-index segment (BP3).
    pub bp3_memory: usize,
    /// Ledger index one past the last segment of turn n-1 (BP4).
    pub bp4_previous: usize,
}

impl Plan {
    /// Breakpoints in prefix order, for a policy that carries all four.
    #[must_use]
    pub const fn all(&self) -> [Breakpoint; 4] {
        [Breakpoint::Bp1Tools, Breakpoint::Bp2System, Breakpoint::Bp3MemoryIndex, Breakpoint::Bp4PreviousTurn]
    }

    /// Offsets in prefix order.
    #[must_use]
    pub const fn offsets(&self) -> [usize; 4] {
        [self.bp1_tools, self.bp2_system, self.bp3_memory, self.bp4_previous]
    }
}

/// Lay the four breakpoints over a segment sequence.
///
/// Walks the segments once, in order: trailing `ToolManifest` segments end at BP1,
/// trailing `SystemContract` at BP2, trailing `MemoryIndex` at BP3, and everything
/// through the end of turn n-1 is BP4. Regions must appear in prefix order
/// (`tools`, `system`, index, turns) - a region that appears after one it must
/// precede is a ledger defect, refused rather than clamped, because a clamped
/// offset silently moves a cache boundary onto the wrong segment.
///
/// BP4 defaults to the full length: with no turn boundary information the only honest
/// position is "everything so far". T23 narrows it to turn n-1 once the turn loop
/// owns turn tracking; the plan never guesses a boundary it was not given.
///
/// # Errors
///
/// [`crate::error::PromptError::RegionOrder`] when the regions do not appear
/// in prefix order.
pub fn plan_breakpoints(segments: &[Sealed<Segment>]) -> Result<Plan, crate::error::PromptError> {
    use supra_types::SegmentKind;

    let mut bp1_tools = 0;
    let mut bp2_system = 0;
    let mut bp3_memory = 0;

    for (index, entry) in segments.iter().enumerate() {
        match entry.kind() {
            SegmentKind::ToolManifest => {
                if bp2_system > 0 {
                    return Err(region_order("tools", "system"));
                }
                if bp3_memory > 0 {
                    return Err(region_order("tools", "memory"));
                }
                bp1_tools = index + 1;
            }
            SegmentKind::SystemContract => {
                if bp3_memory > 0 {
                    return Err(region_order("system", "memory"));
                }
                bp2_system = index + 1;
            }
            SegmentKind::MemoryIndex(_) => {
                bp3_memory = index + 1;
            }
            SegmentKind::Turn { .. } => {}
        }
    }

    Ok(Plan { bp1_tools, bp2_system, bp3_memory, bp4_previous: segments.len() })
}

fn region_order(earlier: &'static str, later: &'static str) -> crate::error::PromptError {
    crate::error::PromptError::RegionOrder { earlier, later }
}

/// Check a plan against the lookback window.
///
/// Sums `lookback_positions` from each breakpoint to the sequence end: a breakpoint
/// more than 20 positions back is outside the cache-read window, and transmitting it
/// would bill a write for an entry that can never be read. Returns the first
/// offending breakpoint, because the fix is per-breakpoint (rewrite the generation),
/// not per-plan.
///
/// `total_positions` is the caller's to supply rather than recomputed here: the plan
/// sees sealed segments, and position counting is T6's `lookback_positions` summed
/// over a suffix - which the caller computes once for the whole sequence rather than
/// once per breakpoint.
///
/// A `Breakpoint` return is not a `PromptError`: a bad plan is a ledger defect with a
/// specific breakpoint attached, and the caller needs the breakpoint - to emit the
/// `CacheBreak` event - not the error enum.
///
/// Returns the first breakpoint at fault: the fix is per-breakpoint (rewrite the
/// generation), not per-plan, so one name is the actionable answer and a list would
/// imply the caller can fix them independently.
///
/// The `Err` carries a [`Breakpoint`], not a [`crate::error::PromptError`]: the two
/// paragraphs above describe why, and a `# Errors` section naming `PromptError`
/// would be the wrong type. The lint wants the section; the section would lie.
#[allow(
    clippy::missing_errors_doc,
    reason = "the Err type is Breakpoint, documented above; a PromptError section would mislead"
)]
pub fn check_lookback(plan: &Plan, segments: &[Sealed<Segment>]) -> Result<(), Breakpoint> {
    let offsets = plan.offsets();
    let breakpoints = plan.all();
    for (index, offset) in offsets.iter().enumerate() {
        let positions: usize = segments[*offset..].iter().map(|entry| entry.lookback_positions()).sum();
        if positions > LOOKBACK_BLOCKS {
            return Err(breakpoints[index]);
        }
    }
    Ok(())
}

/// Truncate a breakpoint list to what a policy carries, keeping the earliest.
///
/// The earliest entries are the frozen ones - BP1 has been read hundreds of times -
/// while the rolling BP4 will be rewritten next turn regardless. Carrying fewer than
/// planned degrades caching; it does not corrupt the request, which is why this
/// returns a shorter list rather than failing.
#[must_use]
pub fn truncate_for_policy(breakpoints: &[Breakpoint], max_breakpoints: usize) -> Vec<Breakpoint> {
    breakpoints.iter().take(max_breakpoints).copied().collect()
}

/// Diagnose a plan whose offsets run backwards.
///
/// Returns the pair of adjacent breakpoints out of order, for the `CacheBreak` detail:
/// an invisible cost leak becomes a debuggable defect only when the message names the
/// mechanism.
#[must_use]
pub fn find_reversal(plan: &Plan) -> Option<(Breakpoint, Breakpoint)> {
    let offsets = plan.offsets();
    let breakpoints = plan.all();
    for pair in offsets.windows(2).enumerate() {
        let (index, window) = pair;
        if window[0] > window[1] {
            return Some((breakpoints[index], breakpoints[index + 1]));
        }
    }
    None
}

/// Validate a plan before transmitting it.
///
/// Checks offset order (no reversals) and the lookback bound. Unused with `#[must_use]`
/// deliberately: the caller matches on the outcome to emit the `CacheBreak` event, and
/// a `must_use Result` would force a binding whose only use is the match.
///
/// # Errors
///
/// [`crate::PromptError`] is not used here: a bad plan is a ledger defect with a specific
/// breakpoint attached, and the caller needs the breakpoint, not the error enum.
/// The two failure modes return the offending breakpoint directly.
pub fn validate(plan: &Plan, segments: &[Sealed<Segment>]) -> Result<(), Breakpoint> {
    if find_reversal(plan).is_some() {
        let offsets = plan.offsets();
        let breakpoints = plan.all();
        for (index, window) in offsets.windows(2).enumerate() {
            if window[0] > window[1] {
                return Err(breakpoints[index]);
            }
        }
    }
    check_lookback(plan, segments)
}

#[cfg(test)]
mod tests {
    use super::*;
    use supra_types::{Block, Role, SegmentId, SegmentKind, TurnId};

    fn manifest() -> Sealed<Segment> {
        use supra_types::{Sealed, SeqNo};
        Sealed::seal(
            SeqNo::ZERO,
            Segment::new(SegmentId::generate(), SegmentKind::ToolManifest, vec![]).expect("empty"),
        )
    }

    fn system() -> Sealed<Segment> {
        use supra_types::{Sealed, SeqNo};
        Sealed::seal(
            SeqNo::ZERO,
            Segment::new(
                SegmentId::generate(),
                SegmentKind::SystemContract,
                vec![Block::Text("contract".to_owned())],
            )
            .expect("text"),
        )
    }

    fn turn(text: &str) -> Sealed<Segment> {
        use supra_types::{Sealed, SeqNo};
        Sealed::seal(
            SeqNo::ZERO,
            Segment::new(
                SegmentId::generate(),
                SegmentKind::Turn { turn: TurnId::generate(), role: Role::User },
                vec![Block::Text(text.to_owned())],
            )
            .expect("text"),
        )
    }

    fn index_entry(topic: &str) -> Sealed<Segment> {
        use supra_types::{MemoryIndexEntry, Sealed, SeqNo};
        let entry =
            MemoryIndexEntry::new(TurnId::generate(), topic.to_owned(), "g".to_owned()).expect("short");
        Sealed::seal(
            SeqNo::ZERO,
            Segment::new(SegmentId::generate(), SegmentKind::MemoryIndex(entry), vec![]).expect("index"),
        )
    }

    #[test]
    fn regions_in_order_yield_ordered_offsets() {
        let segments = vec![manifest(), system(), index_entry("t"), turn("a"), turn("b")];
        let plan = plan_breakpoints(&segments).expect("ordered ledger");
        assert_eq!((plan.bp1_tools, plan.bp2_system, plan.bp3_memory, plan.bp4_previous), (1, 2, 3, 5));
        assert!(find_reversal(&plan).is_none());
        assert!(validate(&plan, &segments).is_ok());
    }

    #[test]
    fn a_manifest_after_system_is_refused_rather_than_clamped() {
        // The M4 gap: no fixture ever interleaved regions, so the clamp's removal
        // changed nothing observable. A manifest segment after the system contract
        // is a ledger defect, and the plan refuses it - a clamped offset would
        // silently move a cache boundary onto the wrong segment.
        let segments = vec![system(), manifest(), turn("a")];
        let error = plan_breakpoints(&segments).expect_err("regions out of order");
        assert!(matches!(error, crate::error::PromptError::RegionOrder { .. }), "{error}");
    }

    #[test]
    fn a_manifest_after_the_memory_index_is_refused() {
        let segments = vec![system(), index_entry("t"), manifest()];
        let error = plan_breakpoints(&segments).expect_err("regions out of order");
        assert!(matches!(error, crate::error::PromptError::RegionOrder { .. }), "{error}");
    }

    #[test]
    fn a_system_after_the_memory_index_is_refused() {
        let segments = vec![manifest(), index_entry("t"), system()];
        let error = plan_breakpoints(&segments).expect_err("regions out of order");
        assert!(matches!(error, crate::error::PromptError::RegionOrder { .. }), "{error}");
    }

    #[test]
    fn truncation_keeps_the_frozen_entries() {
        let kept = truncate_for_policy(&Breakpoint::ALL, 1);
        assert_eq!(kept, vec![Breakpoint::Bp1Tools]);
        let kept = truncate_for_policy(&Breakpoint::ALL, 0);
        assert!(kept.is_empty());
    }

    #[test]
    fn a_breakpoint_outside_the_lookback_window_is_refused() {
        // 21 prose turns past BP1: each is one lookback position, so BP1 sits 21 back.
        let mut segments = vec![manifest()];
        for index in 0..21 {
            segments.push(turn(&format!("turn {index}")));
        }
        let plan = plan_breakpoints(&segments).expect("ordered ledger");
        assert_eq!(check_lookback(&plan, &segments), Err(Breakpoint::Bp1Tools));
    }

    #[test]
    fn tool_runs_count_once_against_the_window() {
        use supra_types::CanonicalJson;
        // A 21-block tool run is one lookback position, so BP1 stays inside the window
        // where 21 prose turns would fall out of it.
        let tool = Segment::new(
            SegmentId::generate(),
            SegmentKind::Turn { turn: TurnId::generate(), role: Role::Assistant },
            vec![
                Block::ToolUse {
                    call_id: "c".to_owned(),
                    name: "read".to_owned(),
                    input: CanonicalJson::empty_object(),
                },
                Block::ToolResult { call_id: "c".to_owned(), content: "ok".to_owned(), is_error: false },
            ],
        )
        .expect("tool run");
        let mut segments = vec![manifest()];
        for _ in 0..10 {
            use supra_types::{Sealed, SeqNo};
            segments.push(Sealed::seal(SeqNo::ZERO, tool.clone()));
        }
        let plan = plan_breakpoints(&segments).expect("ordered ledger");
        assert!(check_lookback(&plan, &segments).is_ok());
    }
}
