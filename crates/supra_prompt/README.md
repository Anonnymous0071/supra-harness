# supra_prompt

Append-only prompt ledger. **T14** of the stage sequence: the ordered segments that
form the prompt prefix, the four breakpoints over them, lossless eviction into T10,
generation rewrites, and the hash guard that turns an invisible cost leak into a
debuggable defect.

## Modules

| Module | Owns |
|---|---|
| `ledger` | append-only sequence, gap-free positions, prefix hash, generation seals |
| `breakpoints` | four offsets over the sequence, lookback bound, policy truncation |
| `evict` | store-first ordering, T13.5 thinking disposition, byte-identical recall |
| `generation` | one rewrite at 92-95% while idle, order-preserving, auditable |
| `error` | failures split by who must act |

## What this stage is for

> Every turn recomputes the prefix hash locally and compares. An unexpected change
> emits `Event::CacheBreak` with the **causing diff**.

T6 owns the segment types and their canonical encodings; this crate owns the
*order* - the sequence, its hash, and the proof that neither moved. I1
(append-only), I4 (lossless eviction), I5 (four breakpoints), and I7 (hash guard)
are the invariants; the modules are where each one is honoured.

## Decisions

**Sealing is the ledger's act.** `append` takes an unsealed `Segment`, never a
sealed one. If callers sealed their own entries they could reserve, skip, or reuse
positions - gap-free sequencing exists only because one function assigns every
position. The return is the assigned `SeqNo` (Copy, stays valid), not a borrow tied
to the ledger's storage (unusable past the next append).

**The prefix hash covers positions, not just content.** A rewrite re-seals every
segment at new positions, so a position-blind hash equates two generations with
different cache lifetimes - and would verify a rewrite against the wrong
generation. The hash runs over `(SeqNo, ContentHash)` pairs under a ledger domain
separator (`0x20`, downstream past T10's `0x10`), never over segment bytes
directly: the pairs are what ordering means.

**Eviction commits before the prefix forgets.** T10 states the rule; this module
honours it: `evict_turn` writes the body to the store, and only on success does the
caller build the index entry. A failed commit leaves the ledger exactly as it was -
the turn stays live. The body is the turn's canonical encoding, not a re-rendering:
recall's byte-identical promise is measured against deterministic bytes, and a
renderer drifting across versions would break recall without breaking any test here.

**Thinking disposition follows T13.5 exactly.** Tool turns keep thinking verbatim
(signature included) or the API 400s; prose turns drop it (silently accepted).
The decision keys on `ToolUse` presence across the whole turn (`.any()`), not
first position: `Segment::new` forbids prose *between* tool blocks but allows
leading prose, so anything narrower lets a caller smuggle thinking out of a tool
turn. Order survives either disposition - a recalled turn is a message again.

**Index entries are pointers, refused when over budget.** Topic plus gist in 80
content bytes (~15 tokens). Truncation would silently turn the budget into a
suggestion; only the caller (T15's digest, which knows the turn's symbols) knows
how to say it more briefly. `index_segment` returns `None` for a hand-edited
over-budget entry rather than crashing the turn loop.

**Rewrite at 92%, while idle, moving content never rewording.** Not 80%: every
avoided rewrite is one full 1-hour-TTL write not paid for. The old seal and the new
seal are both kept - the pair proves the rewrite moved content, and
`verify_rewrite` refuses a reorder as an I1 violation wearing a rewrite's clothes.
Evicted turns stay evicted; their index entries travel with the rewrite,
re-appended not re-committed. `rewrite` returns refused segments rather than
skipping silently: a shorter prefix must fail the pair loudly, not vanish quietly.

**Recall verifies, and missing is not corrupt.** `RecallMissing` (never evicted -
read the live prefix, do not retry) is distinct from `RecallCorrupt` (bytes changed
- no bytes returned, like T10's own refusal). A corrupt body with a warning attached
would be worse than nothing: the model would carry on with altered content and
nothing downstream could tell.

## Mutation results

Eight mutations, all caught. Two survived first.

| Mutation | Verdict |
|---|---|
| M1 tool turns drop thinking | CAUGHT |
| M2 Drop keeps thinking | CAUGHT |
| M3 sequence never advances | CAUGHT |
| M4 monotonic clamp removed | CAUGHT *(survived first)* |
| M5 reorder accepted | CAUGHT |
| M6 positions excluded from hash | CAUGHT *(survived first)* |
| M7 lookback always zero | CAUGHT |
| M8 evict skips store (control) | SKIP (site not found - no such shape) |

**M4** survived because no fixture ever interleaved regions: deleting the clamp
changed nothing observable. Closed with a system-before-manifest fixture asserting
monotonicity - a defective ledger must still yield a transmittable plan, with the
defect surfacing at the ledger rather than as a cache break.

**M6** survived because order-sensitivity and position-sensitivity are different
properties: reordered content already hashes differently, so excluding positions
changed nothing any test observed. Closed by building the collision directly - one
segment sealed at 0 and at 5, content hashes agreeing by design, sequences required
to differ.

## Guard checks

Seven structural checks, each probed. Two were blind before shipping, both familiar
shapes:

- The store-commit check matched `store.evict_turn` on one line; the call spans two
  (`store` newline `.evict_turn`). Now matches the method name.
- The disposition check matched `Block::ToolUse` anywhere; a `blocks.first()`
  narrowing kept the name while shrinking the meaning. Now asserts `.any()` -
  presence anywhere, not first position.

## Obligations left to later stages

- **T15** writes the gist: only the digest knows the turn's symbols well enough to
  say them briefly, which is why over-budget entries are refused rather than
  truncated here.
- **T23** commits evictions before appending index segments (the order this crate
  returns them in), narrows BP4 to turn n-1 once it owns turn tracking, and rewrites
  at 92-95% while idle.
- **T29** renders `CacheBreak` from the breakpoint this crate names: the fix is
  per-breakpoint (rewrite the generation), not per-plan.
