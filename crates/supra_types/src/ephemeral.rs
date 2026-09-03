//! Invariant I2: volatile data never enters the prefix.
//!
//! > `system` holds identity and output contract only. Everything volatile is
//! > rendered as an `EphemeralBlock` in the current request suffix and **discarded
//! > at seal time** - it never enters the BP4 delta, so it never becomes
//! > perpetual.
//!
//! # The enforcement
//!
//! [`EphemeralBlock`] does not implement [`crate::Sealable`], and
//! [`crate::Sealed`] carries that bound on its type definition. So
//! `Sealed<EphemeralBlock>` is not a type that can be *written down*, let alone
//! constructed - the mistake is rejected where it is spelled rather than where it
//! would have been executed. `scripts/check-invariants.sh` additionally fails the
//! build if a `Sealable` impl for this type ever appears.
//!
//! # The deliberate asymmetry
//!
//! This type is freely mutable while [`crate::Sealed`] is rigidly not. That
//! contrast is the design: the prefix is a ledger, the suffix is a scratch pad, and
//! conflating them is precisely the mistake that makes existing harnesses expensive.
//! Rebuilding this block fifty times in one turn costs nothing at all.
//!
//! # Why it matters arithmetically
//!
//! A 40 token state block appended to the prefix every turn is re-read on every
//! later turn, so its cost is quadratic: over 100 turns it accumulates about 202
//! thousand re-read tokens. The same block rendered ephemerally costs 40 tokens per
//! turn and nothing afterwards - 4 thousand over the same session. That 50x gap is
//! reproduced as a test in this module, so the justification for the type is
//! executable rather than a claim in a comment.

use serde::{Deserialize, Serialize};

/// A slot in the volatile state block.
///
/// A closed enum rather than free-form strings, so the render order is fixed and
/// the TUI and the prompt cannot disagree about what state exists.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum EphemeralKey {
    /// Wall-clock time. The canonical example of a value that must never be
    /// sealed: it differs on every request, so a prefix containing it is a
    /// guaranteed cache miss every single turn.
    Timestamp,
    /// Current working directory.
    WorkingDir,
    /// Checked-out git branch.
    GitBranch,
    /// Whether the working tree is clean, which determines whether an R1
    /// classification is honest.
    GitClean,
    /// Context window usage, as a percentage.
    ContextPercent,
    /// Active permission mode. Changing it is free precisely because it lives
    /// here: `Shift+Tab` fifty times produces zero cache writes.
    PermissionMode,
    /// Which sandbox tier is actually enforcing, as reported by T4's probe.
    SandboxTier,
    /// Current TODO list state.
    TodoState,
    /// Number of peers in the current cohort.
    CohortSize,
}

impl EphemeralKey {
    /// Every slot, in render order.
    pub const ALL: [Self; 9] = [
        Self::Timestamp,
        Self::WorkingDir,
        Self::GitBranch,
        Self::GitClean,
        Self::ContextPercent,
        Self::PermissionMode,
        Self::SandboxTier,
        Self::TodoState,
        Self::CohortSize,
    ];

    /// The label used when rendering.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Timestamp => "time",
            Self::WorkingDir => "cwd",
            Self::GitBranch => "branch",
            Self::GitClean => "clean",
            Self::ContextPercent => "context",
            Self::PermissionMode => "mode",
            Self::SandboxTier => "sandbox",
            Self::TodoState => "todo",
            Self::CohortSize => "peers",
        }
    }
}

/// Volatile per-request state, rendered into the suffix and then dropped.
///
/// Deliberately **not** [`crate::Sealable`]. See the module documentation.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct EphemeralBlock {
    /// Sorted by key so rendering is deterministic. Determinism here is not about
    /// caching - this content is never cached - but about making a diff between
    /// two turns' suffixes readable when debugging a cache break.
    entries: Vec<(EphemeralKey, String)>,
}

impl EphemeralBlock {
    /// An empty block.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Set a slot, replacing any previous value.
    ///
    /// Returns `&mut Self` for chaining. Mutability is intentional and safe: this
    /// value cannot reach the prefix.
    pub fn set(&mut self, key: EphemeralKey, value: impl Into<String>) -> &mut Self {
        let value = value.into();
        match self.entries.binary_search_by_key(&key, |(existing, _)| *existing) {
            Ok(at) => self.entries[at].1 = value,
            Err(at) => self.entries.insert(at, (key, value)),
        }
        self
    }

    /// Read a slot.
    #[must_use]
    pub fn get(&self, key: EphemeralKey) -> Option<&str> {
        self.entries
            .binary_search_by_key(&key, |(existing, _)| *existing)
            .ok()
            .map(|at| self.entries[at].1.as_str())
    }

    /// Remove a slot.
    pub fn clear(&mut self, key: EphemeralKey) -> &mut Self {
        if let Ok(at) = self.entries.binary_search_by_key(&key, |(existing, _)| *existing) {
            self.entries.remove(at);
        }
        self
    }

    /// How many slots are set.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether no slot is set.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Render for the request suffix.
    ///
    /// One `key: value` pair per line, in [`EphemeralKey::ALL`] order. Compact
    /// because it is paid every turn, and stable because a readable diff is what
    /// makes an unexpected cache break debuggable.
    #[must_use]
    pub fn render(&self) -> String {
        let mut out = String::new();
        for (key, value) in &self.entries {
            out.push_str(key.label());
            out.push_str(": ");
            out.push_str(value);
            out.push('\n');
        }
        out
    }

    /// Rough token cost of the rendered form.
    ///
    /// Four bytes per token, rounded up. An estimate by construction: exact
    /// tokenisation is per-model and belongs to T13. It is here because the T29
    /// context gauge needs a number before the first request of a turn is sent,
    /// and a live estimate that is reconciled afterwards is more useful than no
    /// number at all.
    #[must_use]
    pub fn estimated_tokens(&self) -> usize {
        self.render().len().div_ceil(4)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What a state block costs over `turns` if it is appended to the prefix every
    /// turn and therefore re-read by every later turn.
    fn perpetual_reread_tokens(per_turn: u64, turns: u64) -> u64 {
        // Turn i re-reads i copies of the block: the arithmetic series.
        per_turn * (turns * (turns + 1) / 2)
    }

    /// What the same block costs if it is rendered into the suffix and dropped.
    fn ephemeral_tokens(per_turn: u64, turns: u64) -> u64 {
        per_turn * turns
    }

    #[test]
    fn i2_arithmetic_is_reproducible() {
        // The figures invariant I2 rests on. If these ever stop matching the
        // architecture document, one of the two is wrong.
        assert_eq!(perpetual_reread_tokens(40, 100), 202_000);
        assert_eq!(ephemeral_tokens(40, 100), 4_000);

        let ratio = perpetual_reread_tokens(40, 100) / ephemeral_tokens(40, 100);
        assert_eq!(ratio, 50, "the gap grows with session length, hence O(n^2)");

        // And it keeps growing, quadratically: doubling the session very nearly
        // quadruples the waste. (The series n(n+1)/2 lands at 3.98x for doubling,
        // approaching 4x from below - the arithmetic is stated exactly rather than
        // rounded to a slogan.)
        let doubled = perpetual_reread_tokens(40, 200);
        assert_eq!(doubled, 804_000);
        assert!(doubled > 3 * 202_000, "nearly 4x, certainly over 3x");
    }

    #[test]
    fn slots_render_in_a_fixed_order_regardless_of_insertion_order() {
        let mut forward = EphemeralBlock::new();
        forward
            .set(EphemeralKey::Timestamp, "2026-09-03T12:00:00Z")
            .set(EphemeralKey::PermissionMode, "auto")
            .set(EphemeralKey::ContextPercent, "42");

        let mut backward = EphemeralBlock::new();
        backward
            .set(EphemeralKey::ContextPercent, "42")
            .set(EphemeralKey::PermissionMode, "auto")
            .set(EphemeralKey::Timestamp, "2026-09-03T12:00:00Z");

        assert_eq!(forward.render(), backward.render());
        assert_eq!(forward, backward);
    }

    #[test]
    fn render_order_follows_the_declared_key_order() {
        let mut block = EphemeralBlock::new();
        for (index, key) in EphemeralKey::ALL.iter().rev().enumerate() {
            block.set(*key, index.to_string());
        }

        let rendered = block.render();
        let labels: Vec<&str> =
            rendered.lines().map(|line| line.split(':').next().unwrap_or_default()).collect();
        let expected: Vec<&str> = EphemeralKey::ALL.iter().map(|key| key.label()).collect();
        assert_eq!(labels, expected);
    }

    #[test]
    fn setting_a_slot_twice_replaces_it() {
        let mut block = EphemeralBlock::new();
        block.set(EphemeralKey::PermissionMode, "ask");
        block.set(EphemeralKey::PermissionMode, "yolo");
        assert_eq!(block.len(), 1);
        assert_eq!(block.get(EphemeralKey::PermissionMode), Some("yolo"));
    }

    #[test]
    fn mode_changes_cost_nothing_but_a_rerender() {
        // Fifty mode changes in one session produce fifty different suffixes and
        // zero prefix bytes, which is why Shift+Tab is free.
        let mut block = EphemeralBlock::new();
        let mut rendered = Vec::new();
        for mode in ["plan", "ask", "auto", "yolo"].iter().cycle().take(50) {
            block.set(EphemeralKey::PermissionMode, *mode);
            rendered.push(block.render());
        }
        assert_eq!(block.len(), 1, "the block never grows");
        assert_eq!(rendered.len(), 50);
    }

    #[test]
    fn clearing_removes_only_the_named_slot() {
        let mut block = EphemeralBlock::new();
        block.set(EphemeralKey::GitBranch, "main").set(EphemeralKey::GitClean, "true");
        block.clear(EphemeralKey::GitBranch);
        assert_eq!(block.get(EphemeralKey::GitBranch), None);
        assert_eq!(block.get(EphemeralKey::GitClean), Some("true"));
        block.clear(EphemeralKey::TodoState);
        assert_eq!(block.len(), 1, "clearing an unset slot is a no-op");
    }

    #[test]
    fn an_empty_block_renders_to_nothing() {
        let block = EphemeralBlock::new();
        assert!(block.is_empty());
        assert_eq!(block.render(), "");
        assert_eq!(block.estimated_tokens(), 0);
    }

    #[test]
    fn a_realistic_block_stays_within_its_budget() {
        // The I2 arithmetic assumes roughly 40 tokens. A block carrying every slot
        // with plausible values should land near that, or the premise is wrong.
        let mut block = EphemeralBlock::new();
        block
            .set(EphemeralKey::Timestamp, "2026-09-03T12:34:56Z")
            .set(EphemeralKey::WorkingDir, "/home/user/project")
            .set(EphemeralKey::GitBranch, "feature/prompt-ledger")
            .set(EphemeralKey::GitClean, "true")
            .set(EphemeralKey::ContextPercent, "37")
            .set(EphemeralKey::PermissionMode, "auto")
            .set(EphemeralKey::SandboxTier, "landlock")
            .set(EphemeralKey::TodoState, "2/5 done")
            .set(EphemeralKey::CohortSize, "3");

        let tokens = block.estimated_tokens();
        assert!(tokens <= 60, "a full state block estimated at {tokens} tokens");
    }
}
