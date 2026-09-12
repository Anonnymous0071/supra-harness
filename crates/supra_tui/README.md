# supra_tui

The terminal surface. **T29** of the stage sequence.

## What this stage is for

Pure rendering: bytes in, strings out, no terminal I/O. T30 owns the
raw-mode terminal and the event pump; this crate owns what the screen
shows.

| Module | Owns |
|---|---|
| `status` | `StatusLine`, priority shed, `Cost` with tilde |
| `meter` | `Meter`, the ratio bar over the probed gauge |
| `thinking` | `ThinkingDisplay`, `∵`/`∴` read-only states |
| `viewport` | `Viewport`, clamped scroll range |
| `spinner` | `Spinner`, ten braille frames |
| `panel` | `Panel`, title plus body lines |

## Decisions

**Five concepts are protected from priority shedding.** Context %, cache %,
session spend, the conditional cache-break marker, and permission mode
switch to compact labels (`c`, `h`, `$…m`, `!`, `m`) before any protected
concept is clipped. With representative two-digit percentages and a live
sub-dollar estimate, all five fit in 18 cells (16 without a cache break).
A narrower single line cannot identify every concept simultaneously, so
it truncates the complete compact line to the physical cell budget rather
than pretending `NEVER_SHED` can create cells. Everything else sheds first.

**Cost is mills, not cents.** `Cost { mills }` renders `+$0.014~`
while estimated, `+$0.014` once reconciled - the §7 shape with three
decimals. The tilde is the design: usage fields are final only after
the stream ends, so an unreconciled estimate says so.

**Every gauge goes through `gauge_for`.** The meter never assumes a
fixed cell cost: the T2 finding (U+2588 Ambiguous, U+2591 Neutral)
means the block pair doubles under a CJK locale, so Wide renders the
portable `#`/`.` pair instead.

**The thinking display carries no cost.** `∵ Thinking…` while
streaming, `∴ Thought for Ns (ctrl+o to …)` after - read-only,
toggled by `ctrl+o` alone. Completed details are retained externally,
shown only while expanded, stripped of terminal controls, and bounded by
both caller-supplied rows and display cells. Thinking tokens are billed
either way; a preview would price the wrong surface.

**The viewport clamps twice.** `new` clamps the offset, and
`visible_range` clamps again - a struct built field-by-field skips
the first, so the range must not trust it.

## Mutation results

Seven mutations; six caught, control survived.

| Mutation | Verdict |
|---|---|
| M1: status line never-sheds bypassed | CAUGHT |
| M2: gauge probing treats the pair as one class | CAUGHT |
| M3: thinking display carries a cost preview | CAUGHT *(survived first: the probe injected dead code `let _ =`, which no test could observe; closed by injecting a visible `$0.01`)* |
| M4: viewport range not clamped | CAUGHT |
| M5: status line never truncates single segment | CAUGHT |
| M6: spinner returns same frame | CAUGHT |
| M7 control: comment only | SURVIVED (control) |

Six guards in `check-invariants.sh`, probed 6/6 (the cost-preview
guard needs `scan_sql` - the `$` lives inside a format string, which
plain `scan` blanks; the T28.7 FORBIDDEN lesson restated).

## Obligations left to later stages

- **T30** owns the raw-mode terminal, the event pump, and the
  `ctrl+o` binding; this crate only renders the states. The status
  line's `live` constructor takes the spend, cache, and mode the
  runtime supplies.
