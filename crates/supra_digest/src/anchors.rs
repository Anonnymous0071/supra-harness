//! Anchors: ~10 precise pointers, ~300 tokens, appended in the suffix.
//!
//! # What an anchor is
//!
//! A symbol's locator (`path:start-end`) plus the one-line gist T14's index entry
//! needs. Not the symbol's text: the suffix carries pointers, and the model
//! `recall`s the bodies it actually needs. Ten pointers at ~30 tokens each is the
//! ~300-token budget; ten bodies would be ten thousand.
//!
//! # Where anchors come from
//!
//! T11's hybrid retrieval over the symbol corpus: the semantic lane finds the
//! function that *does* what the task asks without sharing a word with it, the
//! lexical lane finds the identifier the task names outright, and rank fusion
//! unions the two competences. The digest owns *what* is indexed (symbols with
//! gist text) and *how many* survive (the budget); T11 owns *how* they rank.
//!
//! # The gist, and why the digest writes it
//!
//! T14 refuses over-budget index entries rather than truncating them, "and only
//! the caller (T15's digest, which knows the turn's symbols) knows how to say it
//! more briefly". This module is that caller. A gist names the symbol, its parent,
//! and its kind in one line - `Store::open (method of Store, rust): construct a
//! store` - built deterministically from the symbol record, with zero LLM calls.
//! Deterministic matters twice: the same symbol always yields the same gist (so
//! the suffix is stable across turns, which is what the cache reads), and no
//! model call means retrieval stays on the per-turn overhead budget, not the
//! provider bill.
//!
//! # Budgets are refused, never truncated
//!
//! [`MAX_ANCHORS`] is 10, [`SUFFIX_TOKENS`] is 300. An anchor set that exceeds
//! either is [`DigestError::OverBudget`]: truncation would silently turn the
//! budget into a suggestion, and only T23 (which knows the turn's remaining
//! window) knows which anchors matter least. Same rule as T14's index entries.

use crate::error::DigestError;
use crate::symbol::{Symbol, SymbolKind};

/// Most anchors in one suffix. Ten pointers: enough to orient, few enough to read.
pub const MAX_ANCHORS: usize = 10;

/// Suffix budget in tokens. ~300 tokens of precise pointers, per the stage map.
pub const SUFFIX_TOKENS: usize = 300;

/// Bytes per token for anchor text. English prose runs about four bytes per token;
/// identifiers tokenise worse than prose, so estimates divide by three rather than
/// four - over-estimating cost is safe (refuse early), under-estimating silently
/// exceeds the provider's window.
pub const BYTES_PER_TOKEN: usize = 3;

/// One selected pointer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Anchor {
    /// `path:start-end`, pointing at the symbol's bytes.
    pub locator: String,
    /// One-line gist: name, parent, kind. Deterministic from the symbol record.
    pub gist: String,
    /// The symbol's kind, for suffix grouping.
    pub kind: SymbolKind,
    /// Fused rank position (0-based) from T11's hybrid search. Lower is better.
    /// `None` when the anchor came from a path other than retrieval (a turn's own
    /// symbols at eviction time, which are relevant by construction, not by rank).
    pub rank: Option<usize>,
}

impl Anchor {
    /// Estimated tokens: locator plus gist, over-estimated by construction.
    #[must_use]
    pub fn tokens(&self) -> usize {
        (self.locator.len() + self.gist.len()).div_ceil(BYTES_PER_TOKEN)
    }
}

/// Build a deterministic gist from a symbol record. Zero LLM calls.
///
/// `name (kind[ of parent], language): first line of the declaration's context`.
/// The context is the symbol's own first source line when available (a signature
/// reads better than any generated description), else the kind alone. Bounded:
/// callers needing T14's 80-byte entry use [`gist_for_entry`], which enforces it.
#[must_use]
pub fn gist_for_symbol(symbol: &Symbol, first_line: Option<&str>) -> String {
    let mut gist = String::with_capacity(64);
    gist.push_str(&symbol.name);
    gist.push_str(" (");
    gist.push_str(symbol.kind.name());
    if let Some(parent) = &symbol.parent {
        gist.push_str(" of ");
        gist.push_str(parent);
    }
    gist.push_str(", ");
    gist.push_str(symbol.language.name());
    gist.push(')');
    if let Some(line) = first_line.map(str::trim).filter(|line| !line.is_empty()) {
        gist.push_str(": ");
        gist.push_str(line);
    }
    gist
}

/// Build the topic and gist pair T14's index entry needs, within budget.
///
/// Topic is `name (kind)`; gist is the parent plus first line, or the first line
/// alone. Both together must fit [`supra_types::MemoryIndexEntry::MAX_CONTENT_BYTES`]
/// (80); when they do not, the first line is shortened at a word boundary, then the
/// parent is dropped, then the gist is dropped to the topic alone - in that order,
/// because each step discards the least identifying information first. When even the
/// topic alone exceeds budget (a 70-character generated name), the error names the
/// symbol: only a human can abbreviate what nothing abbreviates.
///
/// # Errors
///
/// [`supra_prompt::PromptError::IndexEntryTooLong`] when no shortening fits. Never
/// truncated silently: see the module documentation.
pub fn gist_for_entry(
    symbol: &Symbol,
    first_line: Option<&str>,
    turn: supra_types::TurnId,
) -> Result<(String, String), supra_prompt::PromptError> {
    const BUDGET: usize = supra_types::MemoryIndexEntry::MAX_CONTENT_BYTES;

    let topic = format!("{} ({})", symbol.name, symbol.kind.name());
    let line = first_line.map(str::trim).unwrap_or_default();
    let parent = symbol.parent.as_deref().unwrap_or("");

    // Candidate gists, most informative first.
    let mut candidates = Vec::with_capacity(4);
    if !parent.is_empty() && !line.is_empty() {
        candidates.push(format!("{parent}: {line}"));
    }
    if !line.is_empty() {
        candidates.push(line.to_owned());
    }
    if !parent.is_empty() {
        candidates.push(format!("in {parent}"));
    }
    candidates.push(symbol.kind.name().to_owned());

    for gist in &candidates {
        if topic.len() + gist.len() <= BUDGET {
            // `topic` moves into `index_entry` on success and is needed for the next
            // candidate on failure: clone per attempt. The topic is tens of bytes;
            // the alternative (rebuilding it per iteration) trades clarity for
            // nothing measurable.
            let built = supra_prompt::index_entry(turn, topic.clone(), gist.clone());
            match built {
                Ok(_) => return Ok((topic, gist.clone())),
                Err(error) => return Err(error),
            }
        }
        // Shorten the line-bearing candidate at a word boundary before giving up on it.
        if gist.len() > 16 && gist.contains(' ') {
            let room = BUDGET.saturating_sub(topic.len());
            if room > 20 {
                let mut shortened = String::new();
                for word in gist.split_whitespace() {
                    if shortened.len() + word.len() + 1 > room {
                        break;
                    }
                    if !shortened.is_empty() {
                        shortened.push(' ');
                    }
                    shortened.push_str(word);
                }
                if !shortened.is_empty() && topic.len() + shortened.len() <= BUDGET {
                    let built = supra_prompt::index_entry(turn, topic.clone(), shortened.clone());
                    match built {
                        Ok(_) => return Ok((topic, shortened)),
                        Err(error) => return Err(error),
                    }
                }
            }
        }
    }
    // Even the topic alone exceeds budget. Report the topic length: the caller
    // needs to know by how much the irreducible minimum overshoots, not the
    // length of a candidate gist that was never the problem.
    Err(supra_prompt::PromptError::IndexEntryTooLong { turn, bytes: topic.len() })
}

/// Check an anchor set against both budgets.
///
/// Anchors beyond [`MAX_ANCHORS`] are refused even when their tokens fit: ten is a
/// readability bound as well as a token bound - an eleven-pointer suffix is not
/// orientation, it is a listing. Tokens beyond [`SUFFIX_TOKENS`] are refused even
/// when the count fits: same rule as T14's entries.
///
/// # Errors
///
/// [`DigestError::OverBudget`] naming the violated bound.
pub fn check_budget(anchors: &[Anchor]) -> Result<(), DigestError> {
    if anchors.len() > MAX_ANCHORS {
        let tokens: usize = anchors.iter().map(Anchor::tokens).sum();
        return Err(DigestError::OverBudget { tokens, budget: SUFFIX_TOKENS });
    }
    let tokens: usize = anchors.iter().map(Anchor::tokens).sum();
    if tokens > SUFFIX_TOKENS {
        return Err(DigestError::OverBudget { tokens, budget: SUFFIX_TOKENS });
    }
    Ok(())
}

/// Render an anchor set as suffix text: one `locator — gist` per line.
///
/// Deterministic byte order (input order, which is fused-rank order from T11):
/// the suffix is part of the prefix T14 hashes, so two renders of the same anchors
/// must be byte-identical or the cache breaks on nothing.
#[must_use]
pub fn render_suffix(anchors: &[Anchor]) -> String {
    let mut out = String::new();
    for anchor in anchors {
        out.push_str(&anchor.locator);
        out.push_str(" — ");
        out.push_str(&anchor.gist);
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::symbol::Language;
    use std::path::PathBuf;

    fn symbol(name: &str) -> Symbol {
        Symbol {
            path: PathBuf::from("src/turns.rs"),
            language: Language::Rust,
            kind: SymbolKind::Function,
            name: name.to_owned(),
            parent: None,
            start_byte: 118,
            end_byte: 145,
            start_line: 9,
        }
    }

    #[test]
    fn a_gist_names_kind_parent_and_language() {
        let mut method = symbol("open");
        method.parent = Some("Store".to_owned());
        let gist = gist_for_symbol(&method, Some("pub fn open(path: &str) -> Store {"));
        assert_eq!(gist, "open (function of Store, rust): pub fn open(path: &str) -> Store {");
    }

    #[test]
    fn a_gist_without_context_is_still_identifying() {
        let gist = gist_for_symbol(&symbol("recall_turn"), None);
        assert_eq!(gist, "recall_turn (function, rust)");
    }

    #[test]
    fn the_same_symbol_always_yields_the_same_gist() {
        // Deterministic matters: the suffix is part of the prefix T14 hashes, so an
        // unstable gist would break the cache on nothing.
        let first = gist_for_symbol(&symbol("open"), Some("pub fn open() {"));
        let second = gist_for_symbol(&symbol("open"), Some("pub fn open() {"));
        assert_eq!(first, second);
    }

    #[test]
    fn eleven_anchors_are_refused_even_when_their_tokens_fit() {
        let anchors: Vec<Anchor> = (0..11)
            .map(|index| Anchor {
                locator: format!("a.rs:{index}"),
                gist: "g".to_owned(),
                kind: SymbolKind::Function,
                rank: Some(index),
            })
            .collect();
        let error = check_budget(&anchors).expect_err("eleven exceeds ten");
        assert!(matches!(error, DigestError::OverBudget { .. }), "{error}");
    }

    #[test]
    fn ten_fat_anchors_are_refused_on_tokens() {
        let anchors: Vec<Anchor> = (0..10)
            .map(|index| Anchor {
                locator: format!("src/very/long/path/module_{index}.rs:1000-2000"),
                gist: "x".repeat(100),
                kind: SymbolKind::Function,
                rank: Some(index),
            })
            .collect();
        let error = check_budget(&anchors).expect_err("tokens exceed 300");
        assert!(matches!(error, DigestError::OverBudget { .. }), "{error}");
    }

    #[test]
    fn ten_lean_anchors_pass_both_budgets() {
        let anchors: Vec<Anchor> = (0..10)
            .map(|index| Anchor {
                locator: format!("a.rs:{index}"),
                gist: "short".to_owned(),
                kind: SymbolKind::Function,
                rank: Some(index),
            })
            .collect();
        check_budget(&anchors).expect("lean anchors fit");
    }

    #[test]
    fn rendering_is_one_pointer_per_line_in_input_order() {
        let anchors = vec![
            Anchor {
                locator: "b.rs:1-2".to_owned(),
                gist: "second".to_owned(),
                kind: SymbolKind::Function,
                rank: Some(1),
            },
            Anchor {
                locator: "a.rs:1-2".to_owned(),
                gist: "first".to_owned(),
                kind: SymbolKind::Function,
                rank: Some(0),
            },
        ];
        // Input order, not sorted: T11's fusion already ordered them, and re-sorting
        // here would second-guess the ranking.
        assert_eq!(render_suffix(&anchors), "b.rs:1-2 — second\na.rs:1-2 — first\n");
    }

    #[test]
    fn tokens_overestimate_by_construction() {
        // 3 bytes per token, not 4: identifiers tokenise worse than prose. A 6-byte
        // anchor estimates 2 tokens; at 4 bytes it would claim 1 (rounding down via
        // integer division would claim 1 at div_ceil too for 6/4=2 - pick the case
        // that separates: 9 bytes is 3 tokens estimated, 2 at prose rate).
        let anchor = Anchor {
            locator: "a.rs:12".to_owned(),
            gist: "g".to_owned(),
            kind: SymbolKind::Function,
            rank: None,
        };
        assert_eq!(anchor.tokens(), (anchor.locator.len() + anchor.gist.len()).div_ceil(3));
    }
}
