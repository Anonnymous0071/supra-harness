# supra_journal

Write-ahead file snapshots and atomic undo. **T16.6** of the stage sequence.

## What this stage is for

The permission engine's R1 class is "automatically recoverable", and the
architecture document is explicit about what that rests on: "`auto` is the
default only because T16.6 `supra_journal` exists. Without an undo stack,
'auto' would be a hope rather than an engineering decision." This crate is
that decision, in three moves:

1. **`snapshot`** reads a file, digests it, and commits the row - before the
   caller's edit may touch the file. T14's "store before drop" for prompt
   segments, restated for files.
2. **`undo`** spans one `Immediate` store transaction across read, verify,
   write, flush, and mark. The store's lock is the only arbiter two
   concurrent undoes must both pass, so the file write happens inside it.
3. **`newest_for_path` / `count_for_path`** report the stack without
   touching it, so a UI can offer the undo that *would* run.

## Modules

| Module | Owns |
|---|---|
| `journal` | `Journal`: snapshot, undo, the report queries |
| `schema` | the `journal_snapshot` table, its CHECKs, its component migration |
| `error` | `JournalError`: store / sqlite / io / corrupt / already-undone / not-found |

## Decisions

**Bytes, not diffs.** A diff would need the current file and a patcher to
undo; bytes need neither and verify by digest. The cost is bounded by live
edits, not history - undone snapshots are prunable, and the table is
per-snapshot, never per-version.

**`created_at` comes from the id, not a second clock read.** Reading the
clock twice (once inside `Ulid::generate`, once for the column) disagreed
whenever the millisecond turned between the reads; `ORDER BY created_at
DESC` could then name the *older* snapshot as newest, and the flake that
proved it produced exactly that, once in ~15 runs. One source of truth: the
id's own timestamp. A test pins the derivation.

**A damaged row restores nothing.** The stored digest is compared against
the stored bytes before anything is written; a mismatch is `Corrupt` with
no bytes offered - I4's reasoning verbatim, because an undo that restores
*approximately* the original destroys the recoverability it exists to
provide. The corruption probe had to be a same-length mutation
(`CAST(upper(CAST(before AS TEXT)) AS BLOB)`): the schema's own CHECKs
refuse the lazier corruptions (length disagreement, TEXT in a BLOB column)
before the digest ever runs.

**Crash between write and mark is idempotent.** The bytes are written and
flushed inside the transaction, the mark commits with it. A crash after the
write but before the commit leaves the file restored and the row unmarked;
undoing again rewrites the same bytes. That is the failure mode a crash must
leave behind.

**Undo restores *this snapshot's* bytes.** Not "the file as it was before
whatever you did last": undoing an old snapshot discards newer edits,
deliberately, because the caller picked the id. The `AlreadyUndone`
refusal exists for the same reason - a second undo of the same row would
revert whatever legitimate edit landed after the first one.

**The digest is domain-separated (kind `0x11`).** `TurnBody` is `0x10`; a
turn body and a file snapshot hashing the same bytes must not produce the
same digest, or a collision between them could look like verification. The
range is compile-asserted, and the domain-separation test pins it against
`body_digest`.

## Mutation results

Nine mutations; seven caught, two survived for stated reasons.

| Mutation | Verdict |
|---|---|
| M1: undo skips digest verification | CAUGHT |
| M2: undo skips the undone check | CAUGHT |
| M3: undo does not fsync | SURVIVED *(documented)* |
| M4: undo does not mark the row | CAUGHT |
| M5: mark_undone marks unconditionally | CAUGHT |
| M6: newest_for_path includes undone | CAUGHT |
| M7: snapshot kind collides with turn body | CAUGHT |
| M8 control: comment only | SURVIVED *(control)* |
| M9: insert stores a second clock read | CAUGHT |

**M3** survives because fsync's promise is durability across *power loss*,
and no userspace test can observe a power loss. The mutation is caught by
the structural guard in `scripts/check-invariants.sh` instead (the
`sync_all` call, pinned with context), and the reasoning is written here -
the same defence-in-depth class as T15's M6.

**M8** is the control: a comment-only change must survive, proving the
harness runs the suite rather than matching diffs.

## Obligations left to later stages

- **T16.7** rates an edit whose snapshot committed as R1 (recoverable), and
  `snapshot_digest` is exposed so the permission engine can verify what it
  is about to trust.
- **T17/T23** call `snapshot` before every R1-class edit lands; the turn
  loop's step 8 (execute the winning claim) is where that call sits.
- **T26** (`supra_session`) may prune old snapshots on `count_for_path` -
  the count exists for exactly that budget.
