//! Prompt segments: what the ledger is made of.
//!
//! A segment is the unit that gets sealed, positioned, and hashed. The four kinds
//! correspond exactly to the four cache breakpoint regions of invariant I5, in
//! prefix order:
//!
//! | Kind | Region | Breakpoint |
//! | ---- | ------ | ---------- |
//! | [`SegmentKind::ToolManifest`] | `tools` | BP1 |
//! | [`SegmentKind::SystemContract`] | `system` | BP2 |
//! | [`SegmentKind::MemoryIndex`] | evicted-turn index | BP3 |
//! | [`SegmentKind::Turn`] | `messages` | BP4 |
//!
//! # Why tool input is a string
//!
//! [`Block::ToolUse`] holds its arguments as [`CanonicalJson`], not as a parsed
//! value. Provider documentation names unstable `tool_use` key ordering as a cache
//! breaker, so the bytes that were hashed must be the bytes that reach the wire. A
//! parsed value would be re-serialised on the way out, and any reordering between
//! those two moments is an invisible cache break. Holding the canonical text makes
//! that impossible rather than merely unlikely.

use serde::{Deserialize, Serialize};

use crate::hash::CanonicalWriter;
use crate::id::{SegmentId, TurnId};
use crate::sealed::Sealable;

/// JSON whose keys are already in canonical order.
///
/// # Producer contract
///
/// T13's canonicalising serialiser is the only sanctioned producer. This crate
/// cannot verify canonicity - doing so would mean parsing JSON here, duplicating
/// the very serialiser whose output this represents, and giving I7 two
/// implementations that can disagree. The constructor is therefore named for the
/// obligation it transfers: whoever calls it is asserting the text is canonical.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct CanonicalJson(String);

impl CanonicalJson {
    /// Wrap text that is already canonical.
    ///
    /// See the producer contract above: the caller is asserting the property.
    #[must_use]
    pub const fn from_canonical(text: String) -> Self {
        Self(text)
    }

    /// The empty object, for a tool taking no arguments.
    #[must_use]
    pub fn empty_object() -> Self {
        Self("{}".to_owned())
    }

    /// The underlying text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Length in bytes.
    #[must_use]
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Whether there is no text at all.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

/// Who authored a turn segment.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Role {
    /// The person at the terminal.
    User,
    /// The model.
    Assistant,
}

impl Role {
    const fn discriminant(self) -> u8 {
        match self {
            Self::User => 0,
            Self::Assistant => 1,
        }
    }
}

/// One piece of a turn.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Block {
    /// Prose.
    Text(String),
    /// Model reasoning.
    ///
    /// Billed as output and never cached, which is why it is a distinct variant:
    /// the T29 renderer hides it behind `ctrl+o` and the T14 ledger needs to know
    /// which blocks carry a provider signature.
    Thinking {
        /// The reasoning text.
        text: String,
        /// Provider-issued signature, when one was supplied.
        ///
        /// Whether prior-turn thinking blocks must be resent, and what omitting
        /// the signature does, is the open question T13.5 resolves. The field
        /// exists so T14 *can* preserve it; until T13.5 lands, T14 is
        /// conservative and keeps thinking blocks on turns containing a
        /// [`Block::ToolUse`].
        signature: Option<String>,
    },
    /// A tool invocation requested by the model.
    ToolUse {
        /// Provider-scoped call identifier, echoed by the matching result.
        call_id: String,
        /// Registered tool name.
        name: String,
        /// Arguments, already canonical.
        input: CanonicalJson,
    },
    /// The outcome of a tool invocation.
    ToolResult {
        /// The `call_id` this answers.
        call_id: String,
        /// Result payload as the model will see it.
        content: String,
        /// Whether the tool failed. A failure is still a result: it is appended,
        /// never dropped, because dropping it would leave a `tool_use` unanswered
        /// and every provider rejects that.
        is_error: bool,
    },
}

impl Block {
    /// Whether this block participates in a tool-call run.
    ///
    /// Invariant I5 renders tool calls consecutively, because the cache-read
    /// lookback window is 20 blocks and a consecutive `tool_use`/`tool_result` run
    /// counts as one position. Interleaving prose between them spends lookback
    /// positions for nothing.
    #[must_use]
    pub const fn is_tool_traffic(&self) -> bool {
        matches!(self, Self::ToolUse { .. } | Self::ToolResult { .. })
    }

    const fn discriminant(&self) -> u8 {
        match self {
            Self::Text(_) => 0,
            Self::Thinking { .. } => 1,
            Self::ToolUse { .. } => 2,
            Self::ToolResult { .. } => 3,
        }
    }
}

impl Sealable for Block {
    const CANONICAL_KIND: u8 = 0x02;

    fn write_canonical(&self, writer: &mut CanonicalWriter) {
        writer.u8(0, self.discriminant());
        match self {
            Self::Text(text) => writer.str(1, text),
            Self::Thinking { text, signature } => {
                writer.str(1, text);
                writer.opt_str(2, signature.as_deref());
            }
            Self::ToolUse { call_id, name, input } => {
                writer.str(1, call_id);
                writer.str(2, name);
                writer.str(3, input.as_str());
            }
            Self::ToolResult { call_id, content, is_error } => {
                writer.str(1, call_id);
                writer.str(2, content);
                writer.bool(3, *is_error);
            }
        }
    }
}

/// The ~15 token stand-in that a losslessly evicted turn leaves in the prefix.
///
/// Invariant I4 evicts old turns verbatim to SQLite rather than summarising them.
/// What stays behind is this: enough for the model to know the turn happened and
/// to decide whether to `recall` it, and nothing more.
///
/// Deserialisation goes back through [`MemoryIndexEntry::new`], so a stored entry
/// cannot carry a budget the constructor would have refused.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "MemoryIndexEntryRepr")]
pub struct MemoryIndexEntry {
    turn: TurnId,
    topic: String,
    gist: String,
}

impl MemoryIndexEntry {
    /// Combined byte budget for `topic` and `gist`.
    ///
    /// I4 sets the index entry at roughly 15 tokens. English prose runs about four
    /// bytes per token, so 15 tokens is about 60 bytes of content; 80 leaves room
    /// for a technical identifier, which tokenises worse than prose, without
    /// letting an entry drift into being a summary. Exact tokenisation is T13's;
    /// this is the schema-level bound, which is the one that cannot be forgotten.
    pub const MAX_CONTENT_BYTES: usize = 80;

    /// Build an entry, refusing one that would exceed the budget.
    ///
    /// # Errors
    ///
    /// [`SegmentError::IndexEntryTooLong`] when `topic` and `gist` together exceed
    /// [`Self::MAX_CONTENT_BYTES`]. It refuses rather than truncating: truncation
    /// would silently turn the budget into a suggestion, and only the caller knows
    /// how to say the same thing more briefly.
    pub fn new(turn: TurnId, topic: String, gist: String) -> Result<Self, SegmentError> {
        let bytes = topic.len() + gist.len();
        if bytes > Self::MAX_CONTENT_BYTES {
            return Err(SegmentError::IndexEntryTooLong { bytes });
        }
        Ok(Self { turn, topic, gist })
    }

    /// The turn this entry stands in for.
    #[must_use]
    pub const fn turn(&self) -> TurnId {
        self.turn
    }

    /// Short subject line.
    #[must_use]
    pub fn topic(&self) -> &str {
        &self.topic
    }

    /// One-line description of what happened.
    #[must_use]
    pub fn gist(&self) -> &str {
        &self.gist
    }
}

impl Sealable for MemoryIndexEntry {
    const CANONICAL_KIND: u8 = 0x03;

    fn write_canonical(&self, writer: &mut CanonicalWriter) {
        writer.u64(1, self.turn.timestamp_ms());
        writer.bytes(2, &self.turn.to_u128().to_le_bytes());
        writer.str(3, &self.topic);
        writer.str(4, &self.gist);
    }
}

/// Staging area for [`MemoryIndexEntry`] deserialisation.
#[derive(Deserialize)]
struct MemoryIndexEntryRepr {
    turn: TurnId,
    topic: String,
    gist: String,
}

impl TryFrom<MemoryIndexEntryRepr> for MemoryIndexEntry {
    type Error = SegmentError;

    fn try_from(repr: MemoryIndexEntryRepr) -> Result<Self, Self::Error> {
        Self::new(repr.turn, repr.topic, repr.gist)
    }
}

/// Which prefix region a segment belongs to.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum SegmentKind {
    /// The frozen, sorted tool manifest. Ends at BP1.
    ///
    /// Invariant I3: tools are registered once at startup and never removed.
    /// Disabling one uses `allowed_tools` or `tool_choice`.
    ToolManifest,
    /// Identity and output contract, at most ~400 tokens. Ends at BP2.
    SystemContract,
    /// One evicted turn's index entry. The last of these ends at BP3.
    MemoryIndex(MemoryIndexEntry),
    /// Part of the live conversation. The end of turn n-1 is BP4.
    Turn {
        /// Which turn.
        turn: TurnId,
        /// Who authored it.
        role: Role,
    },
}

impl SegmentKind {
    /// Whether this region is frozen for the session's lifetime.
    ///
    /// Frozen regions sit behind a 1 hour breakpoint; the rolling region behind a
    /// 5 minute one. See [`crate::Breakpoint`].
    #[must_use]
    pub const fn is_session_frozen(&self) -> bool {
        matches!(self, Self::ToolManifest | Self::SystemContract)
    }

    const fn discriminant(&self) -> u8 {
        match self {
            Self::ToolManifest => 0,
            Self::SystemContract => 1,
            Self::MemoryIndex(_) => 2,
            Self::Turn { .. } => 3,
        }
    }
}

impl Sealable for SegmentKind {
    const CANONICAL_KIND: u8 = 0x04;

    fn write_canonical(&self, writer: &mut CanonicalWriter) {
        writer.u8(0, self.discriminant());
        match self {
            Self::ToolManifest | Self::SystemContract => {}
            Self::MemoryIndex(entry) => writer.nested(1, entry),
            Self::Turn { turn, role } => {
                writer.bytes(1, &turn.to_u128().to_le_bytes());
                writer.u8(2, role.discriminant());
            }
        }
    }
}

/// One ledger entry's contents.
///
/// Construct through [`Segment::new`], which enforces the two structural rules a
/// later stage would otherwise have to remember. Deserialisation goes back through
/// the same constructor, so a stored segment cannot carry a shape the constructor
/// would have refused - which matters because a segment read from a session file is
/// about to be sealed into a prefix and paid for on every subsequent turn.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "SegmentRepr")]
pub struct Segment {
    id: SegmentId,
    kind: SegmentKind,
    blocks: Vec<Block>,
}

impl Segment {
    /// Build a segment.
    ///
    /// # Errors
    ///
    /// [`SegmentError::IndexSegmentHasBlocks`] when a
    /// [`SegmentKind::MemoryIndex`] is given blocks: its content lives in the
    /// structured entry so T11 can index it, and a parallel free-text copy would
    /// be a second source of truth.
    ///
    /// [`SegmentError::InterleavedToolTraffic`] when prose sits between tool
    /// blocks, which spends I5's 20-block lookback window for nothing.
    pub fn new(id: SegmentId, kind: SegmentKind, blocks: Vec<Block>) -> Result<Self, SegmentError> {
        if matches!(kind, SegmentKind::MemoryIndex(_)) && !blocks.is_empty() {
            return Err(SegmentError::IndexSegmentHasBlocks { blocks: blocks.len() });
        }
        if let Some(at) = first_interleaving(&blocks) {
            return Err(SegmentError::InterleavedToolTraffic { at });
        }
        Ok(Self { id, kind, blocks })
    }

    /// Identity.
    #[must_use]
    pub const fn id(&self) -> SegmentId {
        self.id
    }

    /// Which prefix region this belongs to.
    #[must_use]
    pub const fn kind(&self) -> &SegmentKind {
        &self.kind
    }

    /// The blocks, in wire order.
    #[must_use]
    pub fn blocks(&self) -> &[Block] {
        &self.blocks
    }

    /// Lookback positions this segment consumes.
    ///
    /// A consecutive run of tool traffic counts as one position, per the documented
    /// 20-block cache-read lookback window. T14 uses this to decide when a
    /// breakpoint would fall outside the window, which is a silent cache miss
    /// rather than an error.
    #[must_use]
    pub fn lookback_positions(&self) -> usize {
        let mut positions = 0;
        let mut in_run = false;
        for block in &self.blocks {
            if block.is_tool_traffic() {
                if !in_run {
                    positions += 1;
                    in_run = true;
                }
            } else {
                positions += 1;
                in_run = false;
            }
        }
        positions
    }
}

/// Byte offset of the first block that breaks the consecutive-tool-traffic rule.
///
/// Tool traffic may form at most one run: once a run has ended, another tool block
/// means prose was interleaved.
fn first_interleaving(blocks: &[Block]) -> Option<usize> {
    let mut seen_run = false;
    let mut in_run = false;
    for (index, block) in blocks.iter().enumerate() {
        if block.is_tool_traffic() {
            if !in_run {
                if seen_run {
                    return Some(index);
                }
                in_run = true;
                seen_run = true;
            }
        } else {
            in_run = false;
        }
    }
    None
}

impl Sealable for Segment {
    const CANONICAL_KIND: u8 = 0x01;

    fn write_canonical(&self, writer: &mut CanonicalWriter) {
        writer.bytes(1, &self.id.to_u128().to_le_bytes());
        writer.nested(2, &self.kind);
        writer.count(3, self.blocks.len());
        for block in &self.blocks {
            writer.nested(4, block);
        }
    }
}

/// Staging area for [`Segment`] deserialisation.
#[derive(Deserialize)]
struct SegmentRepr {
    id: SegmentId,
    kind: SegmentKind,
    blocks: Vec<Block>,
}

impl TryFrom<SegmentRepr> for Segment {
    type Error = SegmentError;

    fn try_from(repr: SegmentRepr) -> Result<Self, Self::Error> {
        Self::new(repr.id, repr.kind, repr.blocks)
    }
}

/// Why a segment could not be built.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum SegmentError {
    /// A memory index entry exceeded its byte budget.
    #[error(
        "a memory index entry may hold {max} bytes of topic and gist, found {bytes}",
        max = MemoryIndexEntry::MAX_CONTENT_BYTES
    )]
    IndexEntryTooLong {
        /// Bytes the caller supplied.
        bytes: usize,
    },
    /// A memory index segment carried blocks.
    #[error("a memory index segment carries its content in the entry, not in {blocks} block(s)")]
    IndexSegmentHasBlocks {
        /// How many blocks were supplied.
        blocks: usize,
    },
    /// Prose sat between tool blocks.
    #[error("tool traffic must be consecutive; block {at} resumes it after prose")]
    InterleavedToolTraffic {
        /// Index of the offending block.
        at: usize,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hash::ContentHash;

    fn text(body: &str) -> Block {
        Block::Text(body.to_owned())
    }

    fn tool_use(call: &str) -> Block {
        Block::ToolUse {
            call_id: call.to_owned(),
            name: "read_file".to_owned(),
            input: CanonicalJson::from_canonical(r#"{"path":"a.rs"}"#.to_owned()),
        }
    }

    fn tool_result(call: &str) -> Block {
        Block::ToolResult { call_id: call.to_owned(), content: "contents".to_owned(), is_error: false }
    }

    fn turn_kind() -> SegmentKind {
        SegmentKind::Turn { turn: TurnId::generate(), role: Role::Assistant }
    }

    #[test]
    fn consecutive_tool_traffic_is_accepted() {
        let blocks = vec![text("calling"), tool_use("a"), tool_use("b"), tool_result("a")];
        let segment = Segment::new(SegmentId::generate(), turn_kind(), blocks).expect("valid");
        // Prose is one position, the whole tool run is one more.
        assert_eq!(segment.lookback_positions(), 2);
    }

    #[test]
    fn interleaved_tool_traffic_is_refused() {
        // The shape that quietly spends I5's lookback window: prose between two
        // tool runs turns one position into three.
        let blocks = vec![tool_use("a"), text("thinking out loud"), tool_use("b")];
        assert_eq!(
            Segment::new(SegmentId::generate(), turn_kind(), blocks),
            Err(SegmentError::InterleavedToolTraffic { at: 2 })
        );
    }

    #[test]
    fn lookback_counts_a_run_as_one_position() {
        let many: Vec<Block> = (0..30).map(|i| tool_use(&i.to_string())).collect();
        let segment = Segment::new(SegmentId::generate(), turn_kind(), many).expect("valid");
        assert_eq!(segment.lookback_positions(), 1, "30 tool blocks are one position");

        let prose: Vec<Block> = (0..30).map(|i| text(&i.to_string())).collect();
        let segment = Segment::new(SegmentId::generate(), turn_kind(), prose).expect("valid");
        assert_eq!(segment.lookback_positions(), 30, "30 prose blocks are 30 positions");
    }

    #[test]
    fn an_empty_segment_consumes_nothing() {
        let segment =
            Segment::new(SegmentId::generate(), SegmentKind::ToolManifest, Vec::new()).expect("valid");
        assert_eq!(segment.lookback_positions(), 0);
    }

    #[test]
    fn index_entries_respect_their_byte_budget() {
        let turn = TurnId::generate();
        let ok = MemoryIndexEntry::new(
            turn,
            "auth refactor".to_owned(),
            "moved token check into middleware".to_owned(),
        );
        assert!(ok.is_ok(), "a realistic entry must fit");

        let long = "x".repeat(MemoryIndexEntry::MAX_CONTENT_BYTES + 1);
        assert_eq!(
            MemoryIndexEntry::new(turn, String::new(), long),
            Err(SegmentError::IndexEntryTooLong { bytes: MemoryIndexEntry::MAX_CONTENT_BYTES + 1 })
        );
    }

    #[test]
    fn an_index_segment_carries_no_blocks() {
        let entry = MemoryIndexEntry::new(TurnId::generate(), "t".to_owned(), "g".to_owned()).expect("entry");
        assert_eq!(
            Segment::new(SegmentId::generate(), SegmentKind::MemoryIndex(entry), vec![text("duplicate")]),
            Err(SegmentError::IndexSegmentHasBlocks { blocks: 1 })
        );
    }

    #[test]
    fn frozen_regions_are_the_two_that_i5_freezes() {
        assert!(SegmentKind::ToolManifest.is_session_frozen());
        assert!(SegmentKind::SystemContract.is_session_frozen());
        assert!(!turn_kind().is_session_frozen());

        let entry = MemoryIndexEntry::new(TurnId::generate(), "t".to_owned(), "g".to_owned()).expect("entry");
        // BP3 advances on a new generation, so the index is not session-frozen.
        assert!(!SegmentKind::MemoryIndex(entry).is_session_frozen());
    }

    #[test]
    fn tool_input_bytes_reach_the_hash_verbatim() {
        // The I7 property: two orderings of the same object are different content,
        // because the ordering is what the provider caches on.
        let id = SegmentId::generate();
        let kind = turn_kind();

        let sorted = Block::ToolUse {
            call_id: "c".to_owned(),
            name: "n".to_owned(),
            input: CanonicalJson::from_canonical(r#"{"a":1,"b":2}"#.to_owned()),
        };
        let shuffled = Block::ToolUse {
            call_id: "c".to_owned(),
            name: "n".to_owned(),
            input: CanonicalJson::from_canonical(r#"{"b":2,"a":1}"#.to_owned()),
        };

        let left = Segment::new(id, kind.clone(), vec![sorted]).expect("valid");
        let right = Segment::new(id, kind, vec![shuffled]).expect("valid");
        assert_ne!(ContentHash::of(&left), ContentHash::of(&right));
    }

    #[test]
    fn absent_and_empty_signatures_are_different_content() {
        // Relevant to the T13.5 open question: "no signature" and "empty
        // signature" must not become the same segment.
        let id = SegmentId::generate();
        let kind = turn_kind();
        let absent = Block::Thinking { text: "t".to_owned(), signature: None };
        let empty = Block::Thinking { text: "t".to_owned(), signature: Some(String::new()) };

        let left = Segment::new(id, kind.clone(), vec![absent]).expect("valid");
        let right = Segment::new(id, kind, vec![empty]).expect("valid");
        assert_ne!(ContentHash::of(&left), ContentHash::of(&right));
    }

    #[test]
    fn block_reordering_changes_the_hash() {
        let id = SegmentId::generate();
        let kind = turn_kind();
        let left = Segment::new(id, kind.clone(), vec![text("a"), text("b")]).expect("valid");
        let right = Segment::new(id, kind, vec![text("b"), text("a")]).expect("valid");
        assert_ne!(ContentHash::of(&left), ContentHash::of(&right));
    }

    #[test]
    fn canonical_json_reports_its_shape() {
        let empty = CanonicalJson::empty_object();
        assert_eq!(empty.as_str(), "{}");
        assert!(!empty.is_empty());
        assert_eq!(empty.len(), 2);
        assert!(CanonicalJson::from_canonical(String::new()).is_empty());
    }

    #[test]
    fn a_stored_segment_is_revalidated_on_load() {
        let blocks = vec![text("prose"), tool_use("a"), tool_result("a")];
        let segment = Segment::new(SegmentId::generate(), turn_kind(), blocks).expect("valid");
        let json = serde_json::to_string(&segment).expect("serialise");
        assert_eq!(serde_json::from_str::<Segment>(&json).expect("deserialise"), segment);
    }

    #[test]
    fn a_tampered_segment_is_refused_on_load() {
        // Deriving Deserialize would bypass `Segment::new` entirely, letting a
        // session file reintroduce exactly the shapes the constructor rejects -
        // and this content is about to be sealed and paid for on every later turn.
        let id = SegmentId::generate();
        let turn = TurnId::generate();
        let interleaved = format!(
            r#"{{"id":"{id}","kind":{{"Turn":{{"turn":"{turn}","role":"Assistant"}}}},"blocks":[
                {{"ToolUse":{{"call_id":"a","name":"n","input":"{{}}"}}}},
                {{"Text":"interleaved"}},
                {{"ToolUse":{{"call_id":"b","name":"n","input":"{{}}"}}}}
            ]}}"#
        );
        let error = serde_json::from_str::<Segment>(&interleaved).expect_err("interleaved tool traffic");
        assert!(error.to_string().contains("must be consecutive"), "got: {error}");
    }

    #[test]
    fn a_tampered_index_entry_is_refused_on_load() {
        let turn = TurnId::generate();
        let long = "x".repeat(MemoryIndexEntry::MAX_CONTENT_BYTES + 1);
        let json = format!(r#"{{"turn":"{turn}","topic":"","gist":"{long}"}}"#);
        let error = serde_json::from_str::<MemoryIndexEntry>(&json).expect_err("over budget");
        assert!(error.to_string().contains("bytes of topic and gist"), "got: {error}");
    }
}
