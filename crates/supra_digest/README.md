# supra_digest

Repo digest. **T15** of the stage sequence: orientation with zero LLM calls. A
tree-sitter symbol index, a dependency graph, and git churn, maintained
incrementally by a file watcher; a local hybrid BM25 + vector retrieval selects
~10 anchors, or ~300 tokens of precise pointers, appended in the suffix.

## Modules

| Module | Owns |
|---|---|
| `symbol` | what a symbol is: name, kind, byte range, fingerprint |
| `parse` | bytes into symbols, one grammar per language, errors refused |
| `index` | what is defined where: scan, fingerprint skip, watcher patch |
| `graph` | what imports what: edges, blast radius, churn |
| `anchors` | ~10 pointers, ~300 tokens: gists, budget, suffix rendering |
| `digest` | the four parts composed: open, retrieve, watcher events |
| `error` | failures split by who must act |

## What this stage is for

> Turn step 1 is `digest.retrieve(task) -> ~300 token anchors (0 LLM calls)`.

Everything here serves that: fast enough to disappear inside a turn (per-turn
overhead, no LLM: < 50 ms), cheap enough in memory to sit beside a coding
session, and exact enough that an anchor is the one the corpus would have
chosen. The cohort (T15.5) consumes the anchor count and the blast radius as
tier signals; T14 renders the anchors into the suffix it hashes.

## Decisions

**A real parser, not line patterns.** `grep -n '^fn '` reports symbols that do
not exist (comments, strings, macros, `cfg`-ed blocks). The digest feeds the
cohort's blast-radius estimate; a phantom symbol misdirects scrutiny, a missing
one hides it. Seven compiled-in grammars (Rust, TypeScript, Python, JavaScript,
Go, C, C++) cover the working set; anything else is `UnsupportedLanguage` by
design. Verified per grammar against real declaration shapes - including Go's
`receiver` field and C's declarator-carried identifier, the two shapes a naive
`name`-field lookup misses.

**Error nodes yield nothing.** tree-sitter recovers around broken syntax, and
the ranges around the error are guesses. A guess indexed as fact is how an
anchor points at the wrong lines - so a file with an error node contributes no
symbols and names itself via `HasErrors`. Stale entries die with the breakage:
re-indexing a broken file removes its last good parse rather than serving it.

**The index is a derivative, so it lives in memory.** The corpus owns its text;
persisting the derivative beside the source creates the invalidation problem
twice. A restart rescans (cold start < 10 s for 5k files, the budget). The
watcher patches between rescans: fingerprint-compared (blake3, not mtime -
mtimes lie under skew), symlinks never followed, hidden and build directories
never descended.

**Import edges, not call edges.** Blast radius over-approximates by construction:
a file importing a module need not touch the changed symbol. Over-approximation
is the safe direction for scrutiny - a cohort sized too large wastes money, one
sized too small misses the break. Call-graph precision needs cross-file name
resolution through renames and re-exports; that is T15.7's query machinery.

**`crate::` is rewritten at record time.** The graph keys modules by
file-relative path (`src::turns`), but Rust importers write `crate::turns`.
`normalise_import` bridges them using the importer's own directory - plus
`super::` climbing and `self::` staying. An early version keyed modules at
`crate::` and left imports raw, so nothing resolved; the probe-free suite passed
because no test asserted a cross-file radius. Closed with a two-file fixture
(`main.rs` importing `turns.rs`) asserting both directions.

**Churn is git, not timestamps.** `git log --follow --format=%ct`: committer
dates, rename-tracking, zero where git cannot answer (unknown reads as stable,
not alarming - an error would make every offline machine unmeasurable). Unsigned
throughout: future timestamps count as recent (wrong clock, safe side).

**Gists are deterministic strings, zero LLM calls.** `name (kind[ of parent],
language): first line`. The same symbol always yields the same gist - the suffix
is part of the prefix T14 hashes, so an unstable gist would break the cache on
nothing. `gist_for_entry` shortens at word boundaries (line, then parent, then
kind alone) and refuses when even the topic overshoots: only a human abbreviates
what nothing abbreviates. This is the caller T14's refusal message names.

**Budgets are refused, never truncated.** Ten pointers (readability as well as
tokens - eleven is a listing, not orientation), ~300 tokens estimated at 3
bytes/token (identifiers tokenise worse than prose; over-estimating is safe).
Rendering is input order - T11's fusion already ordered them, and re-sorting
here would second-guess the ranking.

**Pool entries die with the retrieval.** The scratch namespace (`digest#<n>`)
is drained at every exit, including `?` exits (drain first, propagate second).
A leaked entry ranks deleted symbols next turn. Explicit `drain_pool` calls, not
a `Drop` guard: items after statements confuse, and explicit reads as what it
is - cleanup the caller must not forget.

**Degrade, never fail, on embedding trouble.** A `None` embedder, a wrong-width
vector, a degenerate one: skip semantically, keep lexically. The lexical
fallback (substring counting with inverse pool frequency) answers alone when
the semantic lane is absent entirely.

## Mutation results

Eight mutations, all caught. Two survived first.

| Mutation | Verdict |
|---|---|
| M1 error nodes harvested as symbols | CAUGHT |
| M2 eleven anchors accepted | CAUGHT |
| M3 token budget unchecked | CAUGHT |
| M4 pool entries leak into later retrievals | CAUGHT *(survived first)* |
| M5 broken files keep stale symbols | CAUGHT |
| M6 external edges followed into blast radius | SURVIVED *(defence in depth)* |
| M7 rust functions invisible | CAUGHT |
| M8 token estimate at prose rate | CAUGHT |

**M4** survived because the no-leak test asserted the count, not the
consequence: namespaced locators return the count to baseline either way.
Closed with two retrievals sequenced around a deletion - the second must find
nothing, which only the drain guarantees.

**M6** survives through the public API: `record_imports` derives `internal`
from `resolve`, and `resolve` returns `None` for every external string - so the
`continue` after it skips the same edges the guard skips. The guard is defence
in depth against a future `resolve` that matches external strings. Same class
as T13's M1 (the redundant sort): the line stays because the guarantee must not
depend on one function keeping its contract. The test pins the observable
behaviour (recorded, never walked), not the guard's firing.

## Guard checks

Six structural checks, each probed. One was blind before shipping, familiar
shape: the token-bound check matched the constant's mere presence, but the
constant also names the budget in the error - presence proves nothing. Now
asserts the `tokens > SUFFIX_TOKENS` comparison.

## Obligations left to later stages

- **T15.5** consumes anchor count, blast radius, and churn as tier signals, and
  declares no dependency on `supra_llm` (negative test) - the zero-LLM-call
  direction enforced at the manifest level.
- **T15.7** owns call-graph precision (cross-file name resolution), byte-range
  splice, and the reparse gate. Locators already name bytes (`path:start-end`),
  which is what the splicer consumes.
- **T23** supplies `embed`, sets the rescan interval to seconds, drives the
  watcher into `apply_event`, and renders anchors into the suffix T14 hashes.
