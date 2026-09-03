# libsupra_width

Terminal cell width and grapheme cluster segmentation. **T2** of the
supra-harness stage sequence.

The sole arbiter of every layout decision in the TUI. If a component computes a
width any other way, that component is wrong.

## Why this is a separate library

Nearly every glyph a terminal UI wants is East Asian **Ambiguous**: one cell in
a Latin locale, two under a CJK locale. Baking in either answer corrupts layout
for half the world's terminals.

The width class is not a property of the text; it is a property of the terminal
and its locale. So `supra_width_ambiguous` is a parameter, and
`supra_width_probe` exists so the TUI can measure its own glyph set at startup
and fall back to an ASCII tier when a glyph does not measure one cell.

Verified against Unicode 17.0.0, with a case that matters more than the theory:

| Glyph | Class | Narrow | Wide |
| ----- | ----- | ------ | ---- |
| U+2588 FULL BLOCK | Ambiguous | 1 | 2 |
| U+2591 LIGHT SHADE | **Neutral** | 1 | **1** |
| U+2593 DARK SHADE | Ambiguous | 1 | 2 |
| U+2235 BECAUSE | Ambiguous | 1 | 2 |
| U+28xx Braille | Neutral | 1 | 1 |

The obvious gauge pairing - `█` filled against `░` empty - **mixes width
classes**. Under a CJK locale the filled cells double while the empty ones do
not, and the bar silently changes length. Neither glyph is wrong; the pairing
is. This is exactly what the startup probe catches, and it is why the spinner
uses Braille, which is Neutral in every locale.

## Contract

Flat C ABI, declared in `include/supra/width.h`.

- **`noexcept` by construction.** Compiled with `-fno-exceptions`: an exception
  unwinding into Rust is undefined behaviour, so the machinery is removed rather
  than merely unused.
- **No allocation, no retained pointers, fully reentrant.** All state lives in
  immutable static tables.
- **Invalid input never traps.** Malformed UTF-8 yields U+FFFD and advances
  exactly one byte. That forward-progress guarantee is what makes every caller's
  scan loop terminate on arbitrary bytes, including adversarial tool output. A
  width function that aborted would take down a session that had been running
  for hours.
- **`SUPRA_WIDTH_NONPRINTABLE` is distinct from zero width.** The caller must be
  able to tell "renders as nothing" (a combining mark) from "must not be sent"
  (a control character).

## Layout

```
include/supra/width.h    public ABI
src/utf8.cpp             decoding, with every malformed class rejected
src/grapheme.cpp         UAX #29 extended grapheme clusters
src/width.cpp            cell width, measurement, truncation, probe
src/internal.hpp         shared inline helpers, property lookup
src/tables.hpp           table declarations, binary search, Hangul arithmetic
src/tables_generated.cpp GENERATED - do not edit
data/                    vendored, trimmed UCD extracts
tools/fetch_ucd.py       refresh data/ for a new Unicode version
tools/gen_width_tables.py    regenerate tables_generated.cpp
tests/                   five CTest suites
```

## Design notes

**Tables are generated, committed, and asserted.** The build needs no Python and
no network. Every table is sorted, non-overlapping, and coalesced, and the
generator checks all three before writing - binary search over an unsorted table
returns a *plausible wrong answer* rather than failing, which is far worse than
a crash.

**Hangul LV/LVT carry no table.** The 11 172 precomposed syllables form one
contiguous block with period 28, so both classes are derived arithmetically. As
tables they would cost 798 ranges, 31% of the total, for what a modulo already
knows. The generator verifies the derivation against the UCD on every run, and
`hangul_arithmetic_test` checks it observably at build time.

**A cluster's width is its widest constituent, not the sum.** A family emoji ZWJ
sequence is five code points and two cells; summing would give eight and wreck
the line.

**Truncation never splits a cluster.** A two-cell cluster that would cross the
limit is excluded whole, so the result may measure one cell short. Half a wide
glyph is a corrupted cell that persists until the next full repaint.

**Truncation here is escape-unaware by design.** Cutting inside an SGR sequence
leaks escape bytes to the terminal. Text carrying escapes is the job of
`supra_ansi_truncate` in T3, which preserves and reissues the active style.

## Tests

Plain executables asserting on exit code. No test framework enters the
dependency graph.

| Suite | Covers |
| ----- | ------ |
| `utf8_test` | every malformed class; forward progress exhaustive over all 256 lead bytes; round-trip over all 1 112 064 scalars |
| `width_test` | ASCII, controls, zero width, East Asian Wide, the ambiguous contract, Braille neutrality, ZWJ sequences, variation selectors |
| `truncate_test` | cluster atomicity; never exceeding the limit across 7 inputs x 21 limits x 2 locales; validation; probe rejection |
| `hangul_arithmetic_test` | all 11 172 syllables; the LV/LVT discriminator at all 28 residues; block boundaries; jamo composition |
| `grapheme_conformance_test` | 766 official UAX #29 cases from the vendored `GraphemeBreakTest.txt` |

The conformance suite is the one that matters. Hand-written cases check what the
author thought of; the UCD file checks what the standard requires, including rule
interactions nobody enumerates by hand.

It also refuses to pass vacuously: fewer than 500 parsed cases exits 2, because
a fixture that moved or a parser that broke must not look like a green run.

### Mutation-verified

Five deliberate defects were introduced and each was caught:

| Mutation | Caught by |
| -------- | --------- |
| GB12/GB13 flag pairing always joins | conformance, 3 cases |
| GB11 pictographic ZWJ disabled | conformance |
| GB9c Indic conjunct disabled | conformance |
| emoji presentation width 2 to 1 | `width_test` |
| ambiguous resolution ignores locale | `width_test` |
| truncation splits a wide cluster | `truncate_test` |

## Building

Built as part of the workspace:

```sh
just build-cpp    # configure + build
just test-cpp     # ctest
just tidy         # clang-tidy
```

Also runs under ASan+UBSan in CI. The tables are indexed from untrusted byte
input, so an out-of-bounds read must fail loudly rather than return a wrong
width.

## Moving to a new Unicode version

```sh
python3 tools/fetch_ucd.py --version 18.0.0
python3 tools/gen_width_tables.py
just test-cpp
```

Review the generated diff. A table change is a rendering change, and the
`data/` extracts are committed so the diff is auditable rather than implicit.
