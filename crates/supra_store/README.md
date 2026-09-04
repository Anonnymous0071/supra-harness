# supra_store

Durable storage. **T10** of the stage sequence: SQLite in WAL mode, versioned migrations,
and the verbatim turn store that invariant I4 rests on.

## Modules

| Module | Owns |
|---|---|
| `error` | one variant per failure, with `Corrupt` carrying I4's enforcement |
| `schema` | forward-only migrations, and the `CHECK` constraints the code depends on |
| `turns` | verbatim eviction, byte-identical recall, and the digest that proves it |

## What this stage is for

> Old turns are **not summarised**. They are evicted verbatim to SQLite and replaced by a
> ~15 token index entry. The `recall` tool returns the original **byte-identical**.

Everything here serves the last word of that sentence:

- bodies are stored as `BLOB` and **never re-rendered**. Storing a structure and rendering
  it back would make the guarantee depend on the renderer staying identical for the life of
  the store, which nobody can promise across versions;
- every row carries a digest of its own body, and recall recomputes it;
- a mismatch returns `Corrupt` and **no bytes**. Returning them with a warning attached
  would be worse than returning nothing — the model would carry on with content that is no
  longer what the conversation contained, and nothing downstream could tell.

Tests cover a round trip over all 256 byte values, an empty body, a megabyte of
non-repeating bytes, a tampered body, and a truncated body. That last one is what the length
prefix in T6's canonical encoding buys: without it a prefix of the bytes could hash the same
as the whole.

## Four measurements that corrected an assumption

Every one of these was something plausible enough to write down as fact. Three were wrong.

**`STRICT` does not protect a `TEXT` column.** A probe appeared to show it rejecting an
integer id; a test contradicted that, and the shell settled it:

```
text<-int accepted, stored as|text|'1'
Runtime error: cannot store INT value in BLOB column t.b
```

The rejection had come from the BLOB column *beside* the id. Left alone, a turn id written
as `1` would have become the text `'1'`, parsed as nothing, and surfaced at recall. So the
invariants the code depends on are `CHECK` constraints, where no code path can bypass them:
a 26-character id, a 32-byte digest, `byte_len = length(body)`, and a non-negative timestamp.

**`total()` returns a REAL.** Chosen over `sum()` for a good reason — `sum()` of an empty
table is `NULL` — and it silently traded that for a float. `coalesce(sum(...), 0)` gets both
properties. This matters past the deserialisation failure that found it: a float byte count
starts losing whole bytes above 2^53, and this crate has no business reintroducing the one
primitive T6 went out of its way to exclude. A test pins both types.

**WAL does not apply in memory.** An in-memory database reports `journal_mode = memory`, so
the WAL assertion needs a real file; the same assertion on an in-memory store would pass for
the wrong reason.

**Transactional DDL does roll back with `user_version`.** The one claim that survived, and
the one worth checking precisely because it sounded convenient. A migration that fails
part-way leaves the version where it was and the table gone — asserted by a test that fails a
migration on purpose.

## Durability, and what it obliges of T14

`synchronous = FULL` by default. In WAL, `NORMAL` can lose the last commits on power loss,
and this store holds turns the prefix has **already dropped** — so a lost commit is a lost
conversation, not a lost cache entry.

It is affordable because eviction is rare: it happens at a generation rewrite, not per turn,
so the fsync never lands on the hot path.

**T14 must commit the eviction before dropping the turn from the prefix.** No setting here
can make the other order safe.

## Other decisions

**A turn has one body.** Evicting identical bytes twice is a no-op, so a retry after a
partial failure is safe. Evicting *different* bytes under the same id is refused — accepting
the second would silently discard whichever version something else still believes in.

**Eviction takes an `IMMEDIATE` transaction.** It reads then writes, and a deferred
transaction that has already read must *upgrade*, which SQLite refuses rather than
deadlocking — and `busy_timeout` cannot help, because the upgrade is unsafe to retry. Taking
the write lock at `BEGIN` makes the pair atomic against a second process, not only against
another thread.

**Migrations are forward only.** A store written by a newer supra is refused, not
downgraded: a later schema may keep a table name with different meaning, and reading it with
older code would not fail — it would misinterpret.

**Canonical kind numbering.** The digest reuses T6's canonical encoding, which means picking
a `CANONICAL_KIND`. This crate establishes the convention: **T6 owns `0x00`–`0x0F`,
downstream crates take `0x10` upward, `0xF0`+ stays for tests.** Enforced by a compile-time
assertion, because a collision makes two different values hash alike and that is not a thing
to learn from a test run.

**One connection behind a mutex.** Eviction and recall are both rare, so the contention a
read pool would relieve does not exist yet, and a pool that is not measured is a guess. WAL
still earns its place: a reader does not block the writer, and a checkpoint blocks neither.

## Mutation results

Ten mutations, all caught. Two survived first.

| Mutation | Verdict |
|---|---|
| M1 recall returns bytes without verifying | CAUGHT |
| M2 a turn accepts a second body | CAUGHT |
| M3 eviction stops being idempotent | CAUGHT |
| M4 the digest covers only a prefix of the body | CAUGHT |
| M5 eviction uses `DEFERRED` instead of `IMMEDIATE` | CAUGHT *(survived first)* |
| M6 durability downgraded to `OFF` | CAUGHT |
| M7 WAL is never enabled | CAUGHT |
| M8 a migration never records that it ran | CAUGHT |
| M9 a wrong-width digest is padded instead of reported | CAUGHT *(survived first)* |
| M10 `byte_len` stops describing its body | CAUGHT |

**M5** survived because within one process the connection mutex serialises writers, so
nothing exercised the difference. Two `Store` handles on one file are two connections — the
situation a second supra process creates — and a barrier makes them race. Under `DEFERRED`
both take a shared lock, read, and then both try to upgrade; the fix is what makes the second
writer wait and then observe the first writer's row.

**M9** is a diagnostic distinction rather than a safety one, and worth stating precisely.
The schema's `CHECK` makes a wrong-width digest unreachable through SQLite, so that path is
defensive. Padding would not break I4 — a padded digest never matches, so recall would still
withhold the bytes — but it would lose the difference between `Malformed` ("the file was
edited around SQLite") and `Corrupt` ("the bytes changed under a valid digest"). Those are
different causes with different remedies. Closed with a direct unit test on the private
function, which is proportionate: the function is unit-testable, so it gets unit-tested,
rather than contorting the schema to reach it.

## Obligations left to later stages

- **T11** adds vector search over the same file (`sqlite-vec`, two-tier LRU, BM25) and
  should extend `MIGRATIONS` rather than open a second database.
- **T14** owns the ordering constraint above, and the ~15 token index entry that replaces an
  evicted turn.
- **T16.6** needs content-addressed storage for the journal. The digest convention and the
  `CHECK`-enforced widths here are the pattern to follow.
