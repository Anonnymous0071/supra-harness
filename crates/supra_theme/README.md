# supra_theme

Semantic tokens, per-glyph width probing, and a responsive banner.
**T28.5** of the stage sequence.

## What this stage is for

The TUI's palette, measured. Three pieces, one crate, because they
share the width table and the locale decision.

| Module | Owns |
|---|---|
| `theme` | eight semantic tokens, themes, SGR sequences |
| `gauge` | per-glyph width probing, stable pair selection |
| `banner` | the responsive wordmark |

## Decisions

**The TUI asks for roles, never embeds an escape sequence.** `Error`,
`Muted`, `Accent` - the theme answers with bytes. A theme also carries
its East Asian Ambiguous resolution, because a theme is a locale
decision as much as a colour decision: the same gauge doubles its cells
under a CJK choice and not otherwise.

**The default theme is ANSI-only.** Eight tokens, no 24-bit sequences -
the first render never depends on true-colour support. The test asserts
no `;38;2;` appears in the default's sequences.

**Per-glyph probing, per the T2 finding.** The Block Elements range is
not one width class: U+2588 is Ambiguous, U+2591 is Neutral - so the
obvious `█`/`░` pairing mixes classes and a gauge silently changes
length under a CJK locale. `GaugeGlyphs::cells` measures each glyph
individually; `is_stable` holds only when the pair shares one class
under both resolutions; `gauge_for` picks the block pair when stable
and the same-class portable `#`/`.` pair otherwise.

**The banner measures every line against the terminal it was asked
for.** Wide terminals get the block art, narrow ones get the word, and
the fits test asserts `cells <= cols` strictly - the first version of
that assertion used `cols.max(cells)`, a truism that no mutation could
break (M3 proved it), closed by making the assertion mean what it says.

## Mutation results

Six mutations; five caught, control survived.

| Mutation | Verdict |
|---|---|
| M1: gauge probing treats the pair as one class | CAUGHT |
| M2: gauge_for always returns block/shade | CAUGHT |
| M3: banner ignores the terminal width | CAUGHT *(survived first: the fits assertion was a truism)* |
| M4: paint drops the colouring | CAUGHT |
| M5: default theme carries 24-bit sequences | CAUGHT |
| M6 control: comment only | SURVIVED (control) |

M3 is the recurring lesson in its fifth and sharpest statement: the
mutation survived not because the code was untestable but because the
assertion could not fail - `cells <= cols.max(cells)` is always true.
A test that cannot fail is not a test.

## Obligations left to later stages

- **T29** renders every surface through these tokens; the status line's
  cost/cache/context segments use `Muted`/`Accent`, the thinking block
  uses `Thinking`, and every gauge goes through `gauge_for` - never an
  embedded pairing.
- **T30** loads theme selection from configuration; a theme table is
  written against `Token::ALL`, in order.
