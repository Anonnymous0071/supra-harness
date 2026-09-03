# supra_types

The contract layer. **T6** of the stage sequence: the types every later stage holds,
and the place where the architecture's invariants stop being prose and become things
the compiler and CI enforce.

## Modules

| Module | Owns |
|---|---|
| `id` | ULID identifiers, one type per kind so a `TurnId` cannot stand in for an `AgentId` |
| `hash` | `ContentHash` over a length-prefixed canonical encoding, deliberately separate from the wire format |
| `sealed` | **I1**: `Sealed<T>` has no mutation path, and a tampered digest is refused on load |
| `segment` | the ledger's units: four kinds matching the four breakpoint regions |
| `ephemeral` | **I2**: `Sealed<EphemeralBlock>` is unnameable, so volatile state cannot become perpetual |
| `cache` | **I5/I7/I8**: breakpoints, TTLs, the ten-times gradient, minimum cacheable lengths |
| `lineage` | the structural half of the anti-self-spawn guard: depth 1, no cycles |
| `permission` | two axes kept apart: authority is never relaxable, consent is what modes are for |
| `cohort` | rational quorum, byzantine tolerance, shard sizing, incremental tallies, the verdict budget |
| `money` | micro-dollars in integers |
| `event` | 47 variants across 11 topics |

## Four decisions worth stating

**Hashing does not reuse the wire serialiser.** I7 needs canonical JSON for the
request; that is T13's. If the hash were computed over those bytes, any change to the
wire format would change every hash in the store, and I4's promise that a recalled
turn is returned *byte-identical* would be measured against a moving target. The
encoding here is a fixed `tag | length | bytes` form owned by this crate, with no
`serde_json` dependency at all.

**Tool input is held as text, not as a parsed value.** Unstable `tool_use` key
ordering is a documented cache breaker. `CanonicalJson` holds the already-canonical
string, so the bytes that were hashed are the bytes that reach the wire. A parsed
value would be re-serialised on the way out, and any reordering between those two
moments is an invisible cache break.

**There are no floats anywhere.** `MicroUsd` is integers, `Confidence` is an enum,
cache multipliers are integer percentages, and quorum is an integer rational. I7
requires byte-stable serialisation and a float's text form is the one primitive whose
stability is not obvious. Quorum has a second reason: `(0.67 * 3).ceil()` is 3, so the
float spelling silently demands unanimity from a three-peer cohort.

**Constructors are re-run on deserialisation.** Deriving `Deserialize` on a validated
type is a hole straight through its invariant: a session file or a store row could
reintroduce exactly the shapes the constructor rejects. `Sealed`, `Segment`,
`MemoryIndexEntry`, `Lineage`, and `Verdict` all revalidate, and there is a test per
type feeding it a forged value.

## Enforcement, and its limits

Most of it is the type system. `Sealed` simply has no `DerefMut`, `as_mut`, or
`into_inner` - the last one matters most, because moving the value out would allow
edit-and-reseal at the same sequence number, which is a rewrite wearing an append's
clothes. The `Sealable` bound sits on `Sealed`'s *type definition* rather than only on
its impls, which makes `Sealed<EphemeralBlock>` unnameable rather than merely
unconstructible: I2 becomes a type error where someone writes the type.

What a type system cannot assert is an *absence*. `scripts/check-invariants.sh`,
wired into `just lint`, scans shipped code - the whole file minus `cfg(test)` bodies,
comments, and string literals - for the ways each guarded invariant could regress. It
was itself probed: an earlier version truncated at the first `#[cfg(test)]` marker,
leaving everything below the test module unscanned, and a fourteen-case probe showed
it missed seven violations out of eight. The current version tracks brace depth and
detects all fourteen.

## Mutation results

Eleven mutations against the load-bearing invariants, via `scripts/mutate.sh` (which
gained crate inference for this stage). All eleven caught.

| Mutation | Verdict |
|---|---|
| M1 quorum becomes `floor(2k/3)` | CAUGHT |
| M2 byzantine tolerance off by one | CAUGHT |
| M3 shard sizing exceeds 15 req/min | CAUGHT |
| M4 unreachable quorum ignores `pending` | CAUGHT |
| M5 lineage depth allows nesting | CAUGHT |
| M6 `auto` stops prompting on R3 | CAUGHT |
| M7 `yolo` checked before authority | CAUGHT |
| M8 canonical encoder drops its length prefix | CAUGHT *(survived first)* |
| M9 sealed deserialisation stops verifying | CAUGHT |
| M10 I8 padding always returns 0 | CAUGHT |
| M11 interleaved tool traffic accepted | CAUGHT |

M8 is the one worth reading. The original test compared `("ab","c")` against
`("a","bc")` and passed *without* the length prefix, because the two fields carry
different tags and the tags already separate them. The prefix only earns its place
when the same tag repeats - a sequence - and the payload contains that tag byte:
`str(2,"a") str(2,"b")` and `bytes(2, b"a\x02b")` both encode as `02 61 02 62` without
it. Block text is arbitrary model and tool output and `0x02` is a real tag in this
crate, so that is a reachable collision rather than a puzzle. Two tests now cover it
from both directions, and the original is renamed to say what it actually shows.

## Obligations left to later stages

- **T13** owns canonical JSON and is the only sanctioned producer of
  `CanonicalJson::from_canonical`. This crate cannot verify canonicity without
  duplicating that serialiser, which would give I7 two implementations that can
  disagree.
- **T13.5** resolves whether prior-turn thinking blocks must be resent.
  `Block::Thinking::signature` exists so T14 *can* preserve it; until then T14 is
  conservative.
- **T14** owns the ledger, and with it the economic test that I2's arithmetic
  describes: a state block appended perpetually must not appear in the prefix hash.
  This crate proves the arithmetic (202k against 4k over 100 turns); only a ledger can
  prove the behaviour.
- **T16.7** owns reversibility classification from the resolved effect, and owes a
  confirmation of one reading taken here: `resolve` implements "deny always wins"
  literally, so **any** deny beats **every** allow and precedence orders allows only.
  The alternative - deny wins only among rules of equal precedence - would make a
  built-in prohibition session-overridable, which contradicts the guard layers having
  no off switch. The conservative reading is implemented and flagged rather than
  assumed settled.
- **T15.5** consumes `Tier`. Its `k_range` reproduces the architecture table exactly,
  gaps included: k of 6 and 13-15 belong to no tier, and `Tier::containing` reports
  `None` rather than rounding to a neighbour. Tier selection runs tier-to-k, so those
  sizes are unreachable rather than unspecified.
