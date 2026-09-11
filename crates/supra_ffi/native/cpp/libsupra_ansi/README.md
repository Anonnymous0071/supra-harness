# libsupra_ansi

Escape sequence parsing, SGR state, and style-safe truncation. **T3** of the
supra-harness stage sequence.

Depends on `libsupra_width` (T2) for every cell-width decision; this library
never computes a width itself.

## Why not a regex

The usual approach is `/\x1b\[[0-9;]*m/`. It is wrong on all of the following,
each of which occurs in real terminal output:

| Case | What breaks |
| ---- | ----------- |
| **OSC terminators** | Two are legal: `ESC \` and `BEL`. A parser knowing only one runs past the end of a hyperlink and eats the text after it. |
| **Sub-parameters** | `ESC[4:3m` (curly underline) and `ESC[38:2::255:0:0m` (RGB) separate arguments with `:`. Splitting on `;` reads one giant parameter and renders the wrong colour. |
| **8-bit C1 controls** | `0x9B` *is* CSI and `0x9D` *is* OSC, with no ESC byte, so `\x1b\[` never fires. |
| **DCS and APC** | Terminated like OSC, and their payloads may contain bytes that look like other sequences. |
| **Split sequences** | Shell output arrives in chunks. A stateless matcher sees two fragments and mangles both. |

So this is a state machine over the DEC STD 070 / VT500 grammar, which handles
all of the above by construction.

## The UTF-8 trap

The C1 control range `0x80..0x9F` is a **subset** of the UTF-8 continuation range
`0x80..0xBF`. A scanner that tests raw bytes for C1 membership tears multi-byte
characters apart:

| Character | Encoding | Collides with |
| --------- | -------- | ------------- |
| U+6587 文 | `E6 96 87` | `0x96` = C1 START OF GUARDED AREA |
| U+200D ZWJ | `E2 80 8D` | `0x80` = C1 PAD |
| U+1F600 😀 | `F0 9F 98 80` | three C1-range bytes |
| U+2235 ∵ | `E2 88 B5` | `0x88` = C1 HTS |

The disambiguation is **positional, not value-based**: no byte in `0x80..0xBF` is
a valid UTF-8 lead, so such a byte is a C1 control exactly when it falls on a
scalar boundary. The scanner carries the count of continuation bytes still owed
(`utf8_expect`), which keeps the distinction correct even when a chunk boundary
lands mid-character.

That last part is the subtle half. Within one buffer the inner loop tracks the
expectation, so whole-buffer tests never exercise the entry condition. Only a
split between a lead byte and a C1-range continuation byte does — and a mutation
test proved that gap was real before it was closed.

## Truncation

`supra_ansi_plan_truncate` is the function this library exists for. It returns a
plan — prefix length plus a trailer — with **no copy**, because it runs per line
per frame.

Four guarantees, every one test-enforced:

1. `cells <= max_cells`, for every input, width, and locale.
2. Never cut inside an escape sequence.
3. Never split a grapheme cluster.
4. Prefix plus trailer leaves the terminal in its default state.

Guarantees 2 and 4 exist because both failure modes outlive the line that caused
them. A sequence fragment is interpreted as a command over whatever follows;
open styling bleeds colour into unrelated output. Both persist until the next
full repaint.

An unstyled line gets an **empty** trailer, so plain text costs no extra bytes.

### One decision worth stating

`style_at_cut` reports the style after **every sequence inside the prefix**,
including a trailing one that covers no cell. Given `ESC[31m red ESC[32m`
truncated to 3 cells, the prefix contains the trailing green and the trailer
closes green.

The alternative — reporting the style in force over the last accepted *text* —
initially seemed more intuitive, and is wrong: the trailing sequence is still
emitted, so tracking a style that excludes it would leave it unclosed and bleed
into every following line. The style must be folded in step with the prefix
advancing, and the test says so explicitly.

## Contract

Flat C ABI in `include/supra/ansi.h`.

- **`noexcept` by construction** (`-fno-exceptions`): an exception unwinding into
  Rust is undefined behaviour, so the machinery is removed rather than unused.
- **No allocation, no retained pointers, reentrant.** All mutable state is
  caller-owned.
- **Total over its input.** Every byte is consumed by some transition, every scan
  advances at least one byte, no input is rejected. A parser that could stall
  would hang the renderer on adversarial tool output rather than mis-render it.
- **Two-phase sizing.** Serialisers report the required length when passed a null
  buffer, so a caller allocates exactly once.

### Streaming

The scanner is resumable. `SUPRA_ANSI_MORE` reports a sequence cut by a buffer
end as `PARTIAL` and retains its state; `SUPRA_ANSI_FINAL` reports it as
`MALFORMED`, so a caller is never left waiting for a terminator that will not
arrive.

`buffer_done` defers the cursor reset by one call. Resetting at the point of
exhaustion looks natural and breaks two things: the caller's drain loop rescans
the same buffer forever, and a following chunk longer than the current one has
its leading bytes skipped by a stale cursor.

## Layout

```
include/supra/ansi.h     public ABI
src/scanner.cpp          resumable state machine
src/style.cpp            SGR folding and serialisation
src/truncate.cpp         measurement, stripping, truncation planning
src/internal.hpp         grammar predicates, C1 constants
tests/                   four CTest suites
```

## Tests

| Suite | Covers |
| ----- | ------ |
| `scanner_test` | every grammar case above; UTF-8 containing C1 bytes; total over all 256 single bytes; token spans tile the input exactly |
| `style_test` | attributes, all colour forms, underline sub-parameters, hyperlinks; the fold/serialise round trip over 12 styles |
| `ansi_truncate_test` | the four guarantees across 12 inputs x 26 limits x 2 locales, each checked for balance and absence of fragments |
| `streaming_test` | chunk-size invariance at **every** size from 1 up, for 10 inputs; split before a C1-range continuation byte; style folding across boundaries |

All pass under RelWithDebInfo and ASan+UBSan, and clean under clang-tidy.

### Mutation-verified

Eight deliberate defects introduced, all eight caught:

| Mutation | Caught by |
| -------- | --------- |
| BEL not treated as an OSC terminator | `scanner_test` |
| `0x9B` not treated as CSI | `scanner_test` |
| byte-wise C1 test (ignores UTF-8 structure) | `streaming_test` — **survived until the split-boundary test was added** |
| omitted parameter reported as `0` | `scanner_test` |
| colon sub-parameters ignored | `style_test` |
| hyperlink not closed on reset | `style_test` |
| trailer omitted entirely | `ansi_truncate_test` |
| truncation splits a wide cluster | `ansi_truncate_test` |

The third row is the reason this section exists. The implementation was correct,
the suite passed, and the mutation survived — which meant nothing was actually
checking the invariant. A passing suite is not evidence until something has tried
to break it.

## Building

```sh
just build-cpp
just test-cpp
just tidy
```
