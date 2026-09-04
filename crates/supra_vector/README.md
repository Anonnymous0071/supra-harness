# supra_vector

Hybrid retrieval. **T11** of the stage sequence: the lexical and semantic lanes behind the repo
digest's anchors, over the same SQLite file T10 opened.

## Modules

| Module | Owns |
|---|---|
| `codes` | binary codes, the exact dot product, and vector validation |
| `search` | the resident code tier, candidate selection, and rank fusion |
| `cache` | the bounded second tier of exact vectors |
| `schema` | the tables, and the configuration frozen at first write |
| `index` | the queries, and the transaction that keeps both tiers honest |
| `error` | one variant per failure, separating *this request is wrong* from *the index is* |

## What this stage is for

> A local hybrid BM25 + vector retrieval selects ~10 anchors, or ~300 tokens of precise
> pointers, appended in the suffix.

Ten anchors, once per turn, with no model call. Everything here serves that: fast enough to
disappear inside a turn, cheap enough in memory to sit beside a coding session, and exact enough
that an anchor is the one the corpus would have chosen.

## The measurement the design rests on

Retrieval has to look at every entry, so its cost is bytes moved. At 768 dimensions an exact
vector is 3072 bytes and a code is 96. Measured on a 4-core i3-8100T (6 MB L3, default `x86-64`
target, so SSE2 only), worst of 20 runs:

|  | 10k | 100k | 200k | resident at 100k |
|---|---|---|---|---|
| exact f32 scan | 2.735 ms | 24.584 ms | 46.985 ms | 307 MB |
| code scan | 1.001 ms | 4.037 ms | 7.002 ms | 9.6 MB |
| rerank of 100, from memory | | 0.065 ms | | |
| fetch of 100, from SQLite | | 0.256 ms | | |

The exact scan is not slow because of its inner loop: it runs at about 12.5 GB/s, which is this
host's single-core streaming limit. It is slow because it reads 307 MB. Nothing in the loop can
fix that; only reading less can.

The codes cost nothing in accuracy because they are not the answer. They choose a hundred
candidates and the exact vectors order those. Over a hundred queries against a corpus with
cluster structure:

| rerank width R | 10 | 50 | 100 | 200 | 500 |
|---|---|---|---|---|---|
| recall@10 | 0.898 | 1.000 | 1.000 | 1.000 | 1.000 |

and across corpora of decreasing separability, `R = 100` holds recall at 1.000 for every corpus
whose top-10 similarity exceeds its mean by 0.29 or more. Real embedding corpora sit far above
that. The case that fails is a corpus with no structure at all, where there is no correct answer
to preserve.

## Two corrections, both in the measurement

Neither mistake was in the method, and both produced numbers that looked reportable.

**One accumulator condemned exhaustive search.** The first exact-scan measurement reported
11.2 ms at 10k. Float addition is not associative, so a single `sum()` forces LLVM to keep one
dependency chain; four independent accumulators cut it to 3.05 ms. The nested `Vec<Vec<f32>>`
layout, which the diagnosis had blamed first, made no difference at all - one pointer dereference
per 3 KB of sequential reads costs nothing. Eight accumulators are no better than four and sixteen
are worse, because by then the loop is at the memory limit and wider unrolling only adds register
pressure.

**A broken corpus condemned everything at once.** The first recall measurement reported 0.544 for
this design - and 0.055 for an embedded HNSW index, which is the tell, because no graph index is
that bad. The generator normalised each centroid and then added per-component noise nine times
the size of the centroid's own components, and drew its queries from a differently seeded mixture.
That is not a clustered corpus; it is noise with a faint direction, the pathological case for
every approximate method simultaneously: all pairwise similarities collapse onto one value, so a
1-bit code has nothing to preserve and a greedy graph walk has no gradient to follow.

Hence `search::CorpusSignal`. A corpus states its own separability - the gap between what a
query's best matches score and what an arbitrary entry scores - before any recall figure taken
over it means anything. The integration test asserts the signal before it asserts recall.

## Why no approximate index

An embedded HNSW (`usearch` 2.26.2, which builds here with no package manager) was measured
against this on the corrected corpus:

| | recall@10 | p99 | build | memory |
|---|---|---|---|---|
| HNSW f32, 10k | 0.998 | 1.23 ms | 5.0 s | 67 MB |
| HNSW f16, 10k | 1.000 | 0.75 ms | 4.7 s | 34 MB |
| HNSW i8, 10k | 0.883 | 0.56 ms | 4.4 s | 17 MB |
| HNSW f32, 100k | - | - | 115 s | 554 MB |
| this design, 100k | 1.000 | ~4.3 ms | none | 9.6 MB |

It is faster per query and costs more of everything else, against a budget this design already
meets - plus a C++ toolchain requirement on every machine that builds supra.

What would change the answer is a corpus past a few hundred thousand entries, where a linear scan
leaves the budget however few bytes each entry costs. Counted by symbol, the largest repository on
this machine has 77,250 - which is 7.4 MB of codes.

**int8 quantisation was measured and rejected.** It moves a quarter of the bytes and is 1.6×
*slower*: `i8 -> i32` widening needs SSE4.1, which the default `x86-64` target does not enable, so
it costs scalar work to save a resource that is not scarce. It also loses 0.7-0.85% of recall@10
for the privilege. Asymmetric scoring through a byte-indexed lookup table avoids the widening and
lands at 14.6 ms per 100k - worse than the exact scan it was meant to beat.

**The C++20 hot path is not used here.** `u64::count_ones()` already compiles to the best
instruction the target allows, and the scan is at the machine's memory limit. A C++ kernel would
buy nothing measurable. What would justify one: a corpus past 200k entries, or parallelising
across cores, which is worth about 2-2.5× rather than 4× because the loop is bandwidth-bound.

## Decisions

**The threshold is frozen.** A code records, per dimension, whether the value was above a
threshold. If the threshold moves - recomputed as the corpus mean each time the corpus grows, say
- every code written before the move answers a different question from every code written after,
and the scan ranks them against each other. Nothing fails; the answers quietly stop being right.
So it is chosen once, stored, and never updated. Changing it is a rewrite, not a migration.

A caller passing a different threshold is **told**, not silently overridden: ignoring an argument
leaves the caller believing something false about how its own queries are encoded. The comparison
is over the encoded bytes, not the floats.

On the corpora measured here, a per-dimension mean threshold and plain sign quantisation were
indistinguishable - both recall 1.000 at `R = 50`. The threshold is stored anyway, because real
embeddings are anisotropic and re-binarising later is a full rewrite.

**A model change disables the lane rather than scoring.** Embeddings from two models share no
space, so a cosine between them is a number with no meaning - and it would still rank. This is
the rule 0xPony's `mem_meta` already followed. The mismatch is reported and the caller decides
between re-embedding and running lexical-only, because only the caller knows which is acceptable.

**Fusion is over ranks, not scores.** A cosine is bounded and roughly linear; `bm25()` is
unbounded, negative, and scaled by the corpus's term statistics - so the same document scores
differently once unrelated documents are added. Normalising onto a shared range means choosing a
min and a max, both of which move as the corpus changes, which would make the fused ranking depend
on corpus size. Reciprocal rank fusion needs only positions: `SCALE / (K + rank)`, summed as
integers, reproducible bit-for-bit.

`RRF_SCALE` was 1,000,000 with a comment claiming distinctness to rank 1000. It collapses at rank
941, because adjacent contributions differ by about `SCALE / d²`. The test found it; the claim is
now derived from `MAX_FUSION_DEPTH` and asserted at compile time, where it cannot drift out of
agreement with the constants it is about.

**A missing entry is not penalised.** A lane that never saw an entry has said nothing about it.
Scoring silence as a negative would let the narrower lane veto the wider one, and the lexical lane
is always narrower - it only returns entries containing the query's terms.

**`bm25()` is negative and better is more negative,** so ordering is ascending. Taking an absolute
value, or ordering descending, inverts the lane silently: every query still returns the number of
results it was asked for.

**The lexical query is tokenised, not passed through.** FTS5 query syntax gives meaning to `"`,
`*`, `(`, `)`, `:`, `-`, `^` and the bare words `AND`, `OR`, `NOT`, `NEAR`. A task description
contains those, so `fix the parser (see #12)` passed through is a syntax error - retrieval failing
on the punctuation in its own input. Terms are runs of alphanumerics and underscores, each quoted
as a phrase and joined with `OR`. `OR` rather than `AND` because a query is a sentence, not a
filter: `AND` over a dozen terms matches nothing, and `OR` lets `bm25()` weight the rare terms
above the common ones. A term cannot contain a quote by construction, which is what makes quoting
sufficient without escaping - asserted by a test rather than left as a remark.

**The text is indexed but not stored.** FTS5 `content=''` with `contentless_delete=1`. The corpus
is the source of truth for its own text - the digest is rebuilt from the working tree by a file
watcher - so a second copy would double the file for nothing. `contentless_delete=1` is what makes
a contentless table support `DELETE` at all, which an incrementally maintained index needs.

**Both tiers are updated only after the commit.** The resident tier is a cache of what is durable.
Updating it first would leave it describing a write that failed, and every later query would trust
it.

**Two ledgers in one file.** `user_version` is a single 32-bit slot and T10's core schema owns it,
so this crate records its version in `schema_component` through
`Store::migrate_component`. That keeps each stage's table definitions in the stage that owns their
meaning while the file keeps one schema history. The alternative - one list in T10 holding every
downstream stage's DDL - would put T11 and T16.6 inside T10.

The two doors into T10's file, `with_connection` and `with_transaction`, keep the mutex inside, so
no caller can hold the connection past its closure or take the lock twice.

## Verified rather than assumed

- **FTS5 with `bm25()` is in the bundled SQLite 3.53.2.** `PRAGMA compile_options` reports
  `ENABLE_FTS5`. The system CLI is 3.46.1 and irrelevant; only the bundled build ships.
- **`bm25()` returns REAL and negative.** `typeof(bm25(...)) = "real"`, and a better match is more
  negative.
- **CJK matches with the default `unicode61` tokeniser**, and identifier tokenisation splits on
  `_`, so `recall_turn`, `recall` and `turn` all match `fn recall_turn(...)`.
- **A `CREATE VIRTUAL TABLE ... USING fts5` rolls back with its transaction**, shadow tables
  included. A virtual table's DDL runs the module's own constructor, so there was no reason to
  assume it inherits ordinary DDL's rollback - and a half-created FTS5 table would leave the
  component at version 0 with shadow tables present, failing every retry for ever on a name that
  already exists.
- **An FTS5 auxiliary function does not accept a table alias.** `bm25(t)` is rejected as "no such
  column: t", which is a confusing way to be told that.

## Mutation results

Fourteen mutations, all caught. One survived first.

| Mutation | Verdict |
|---|---|
| M1 rank by code distance instead of the exact vector | CAUGHT |
| M2 rerank width collapses to the result limit | CAUGHT |
| M3 upsert leaves the old exact vector cached | CAUGHT |
| M4 upsert stops deleting the old indexed text | CAUGHT |
| M5 lexical lane orders `bm25` descending | CAUGHT |
| M6 open accepts a changed binarisation threshold | CAUGHT |
| M7 open accepts a different embedding model | CAUGHT |
| M8 hamming distance skips the trailing bytes | CAUGHT |
| M9 exact similarity drops its remainder lanes | CAUGHT |
| M10 validate stops rejecting non-finite components | CAUGHT |
| M11 candidate ties stop breaking on the slot | CAUGHT |
| M12 removal leaves the position map stale | CAUGHT *(survived first)* |
| M13 the exact cache stops evicting | CAUGHT |
| M14 a wrong-width code is skipped rather than refused | CAUGHT |

**M12** survived because nothing in the read path consults the position map - the scan walks the
code buffer directly - so a stale entry is invisible to every test that only searches. It surfaces
on the next *write*: an upsert of the entry that a removal relocated lands at its old position,
overwriting a different entry's code, or indexes past a buffer that was just truncated. Closed by
a test that removes from the middle and then overwrites the relocated entry, and by a shadow-map
test that checks after every operation that each slot still holds the code last written for it.

## Guard checks

Fifteen structural checks in `scripts/check-invariants.sh`, each probed against a deliberate
violation. Twenty-two probes in total, because probing T11's schema checks exposed the same
weakness in T10's and those were re-probed rather than assumed sound.

Two of them were blind, and both for the same reason: **a guard must test enforcement, not
vocabulary.**

- A `grep -F` for a constraint was satisfied by the module documentation that quotes the
  constraint while explaining why it exists - so it passed with the constraint deleted. The
  scanner strips comments now, and the checks go through `has_sql`.
- The same check was then satisfied by the migration's own `--` commentary *inside* the SQL string
  literal. The keep-strings mode strips SQL comments too.

A third defect was in the harness rather than a check: `production_lines | grep -q` returns
failure under `pipefail`, because `grep -q` exits on its first match, closes the pipe, and the
Python producer dies of `BrokenPipeError`. Every check written that way would have failed on a file
that satisfied it. `has_sql` collects the output before searching it.

## Obligations left to later stages

- **T15 `supra_digest`** owns what gets indexed. This crate never parses a locator: whether it
  names a symbol, a line range, or a turn id is T15's decision. It also owns the embedding model
  and therefore the `model` identity and `dims` passed to `VectorIndex::open`.
- **T14 `supra_prompt`** turns anchors into the ~300-token suffix, and owns the ~15 token index
  entry that replaces an evicted turn.
- **T16.6 `supra_journal`** needs content-addressed storage in the same file. It should register
  its own component in `schema_component` rather than extend either existing list.
- **T29 `supra_tui`** shows cache behaviour. `VectorIndex::cache_stats` and `resident_bytes` are
  the reporting path; `CacheStats::hit_rate` returns `None` before the first lookup, because no
  lookups is not a zero hit rate.
