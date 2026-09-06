# supra_ast

Structural code operations. **T15.7** of the stage sequence: the call-graph
precision T15's README promises - cross-file name resolution, byte-range
splice, and the reparse gate - plus the outline and query machinery around
them.

## Modules

| Module | Owns |
|---|---|
| `splice` | byte-range splice with the reparse gate |
| `query` | outline rendering plus reference search through imports |
| `rename` | multi-file syntactic rename, atomic, shadow-checked |
| `error` | failures split by who must act |

## What this stage is for

T15 answers "what is defined where" for the cohort's blast-radius estimate;
this crate answers "who refers to what" for the edit that follows. The digest
over-approximates by import edge (safe for scrutiny); the edit needs the
narrower answer (safe for rewriting). Both are syntactic: no type resolution,
no overload disambiguation - which is why every rename and reference result
carries `semantic: false`, stated as a field rather than a footnote.

## Decisions

**One harvest, in T15.** Grammars, `Language::detect`, `Symbol`, and the
import graph all come from `supra_digest`: the splicer parses what the
indexer indexes, so a file the digest can anchor is a file this gate can
verify, and vice versa. Reimplementing the harvest here would give two
harvests that can disagree about what a file declares.

**The range must be exact.** Rounding a near-miss to the enclosing node would
make the gate certify an edit the caller did not ask for. `find_exact`
descends while exactly one child contains the range; a partial node is
refused, never rounded outward. Empty ranges match nothing - a point where a
node starts is still not a node - and the guard and the finder are tested
independently so neither passes silently on the other.

**Same-kind, not just parses.** A function replaced by a struct parses fine
and is still the wrong edit. Same-kind proves the replacement occupies the
same grammatical role, which is the part a string-matching `edit_file`
cannot promise. Cross-kind replacement is delete plus insert wearing a
splice's name - refused as one.

**Formatting is preserved by construction.** Only the addressed bytes move;
no pretty-printer runs. "Preserves formatting" is the absence of a formatter,
not a claim about one.

**Reachability before text.** A textual match outside the import graph is
coincidence, and coincidence renamed is corruption. The graph narrows (only
files with an import edge toward the defining file), the tree confirms
(whole-identifier comparison, not substring - `recall_turntable` never matches
`recall_turn`). The defining occurrence is included: the definition *is* a
reference for rename purposes.

**Shadow refusal before any splice.** Silently creating a shadow rebinds every
existing reference. Checked against the outline (harvested declarations), not
substring search: only a declaration shadows. `NothingToRename` on zero
occurrences: an empty rename reporting success is how a misspelled target
passes silently.

**Back to front within each file.** Earlier offsets stay valid while later
bytes move. The sort is load-bearing only when names differ in length -
same-length renames shift nothing, so tests use length-changing renames to
distinguish the orders. Files come out in path order; occurrences in byte
order. Both sorts are pinned by tests that isolate them (reverse-order
candidates, multi-occurrence files).

**`semantic: false` is a field.** Syntactic rename without LSP can be wrong
under shadowing or overloading (stated limitation, architecture document).
A boolean the caller must read outlives a sentence the caller must remember -
and T24 flips what it computes without reshaping the result.

**Nothing touches the filesystem.** `rename` returns rewritten files; T16.6's
journal snapshots before any byte lands. Atomicity is structural: every splice
verified before any file returned, first refusal aborts everything - all files
or an error, never a prefix of the work.

## Mutation results

Twelve mutations. Two survived first, two survive by design.

| Mutation | Verdict |
|---|---|
| M1 kind change accepted | CAUGHT |
| M2 broken splice accepted | CAUGHT *(survived first)* |
| M3 broken source spliced | CAUGHT |
| M4 partial range rounded | CAUGHT |
| M5 unreachable files renamed | CAUGHT |
| M6 shadow gate removed | CAUGHT |
| M7 forward splice order | CAUGHT *(survived first)* |
| M8 comment-only change | SURVIVED *(control: no semantic change)* |
| M9 occurrences unsorted | CAUGHT |
| M10 files sort removed | SURVIVED *(redundant-today, T13 M1 class)* |
| M11 empty range accepted | CAUGHT *(survived first)* |
| M12 references unsorted | CAUGHT |

**M2** survived because the broken-replacement fixture failed the error check
first - deleting it changed nothing the kind gate could observe. Closed with
errors *outside* the replaced span (clean span resolves, tail fires the gate).

**M7** survived because every fixture renamed equal-length names, where order
is unobservable - forward shifts nothing. Closed with a length-changing rename
(4 bytes to 6): forward order shifts the second occurrence and the gate
refuses it.

**M11** survived twice. First: the integration test could not distinguish
"guard refused" from "finder found nothing" (both refuse). Closed with a
finder unit test - then the finder had no empty-range check at all, so the
guard carried the whole refusal alone. Closed for real by putting the check in
both places with independent tests: the guard refuses before reaching the
finder, the finder matches nothing on its own.

**M10** survives because `query_references` sorts by file and the loop
preserves order - deleting the sort changes nothing today. Same class as
T13's M1: the line stays (the guarantee must not depend on an upstream
iterator's order), the test pins the output bytes.

**M8** is the control: a comment-only change must survive, proving the harness
runs the suite rather than matching diffs.

## Guard checks

Eight structural checks, each probed. Four were blind before shipping:

- The reuse check matched grammar names anywhere; a decorative import beside
  a local copy passed. Now asserts the `parse_file` call site.
- The gate checks matched bare names present twice each; deleting one use
  passed. Now asserts each decisive use with context.
- The shadow check matched the variant name appearing in docs and error
  definitions; deleting the enforcement passed. Now asserts the
  `outline.iter().find...== *new` context.
- The directory-to-`scan` lesson again (T15.5): one file per call, or the
  guard certifies what it cannot see.

## Obligations left to later stages

- **T16.7** rates `replace_node` one reversibility class lower than blind
  `edit_file` on the same file: verified structure earning greater trust.
  `yolo` does not skip the gate (authority, not consent).
- **T24** flips `semantic` where language servers prove resolution, and owns
  shadowing/overloading precision this crate declares it lacks.
- **T16.6** snapshots before any renamed byte lands; this crate never touches
  the filesystem.
