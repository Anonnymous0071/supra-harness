# supra_session

Session persistence, resume, branch, and export. **T26** of the stage
sequence.

## What this stage is for

A session is the identity a resume restores: its id, its completed
turns, and the file both live in. The four operations the stage map
names - persistence, resume, branch, export - are four functions over
one shape.

| Module | Owns |
|---|---|
| `session` | `Session`: new, record, save, resume, branch, export |
| `store` | the on-disk files: one JSON per session id |
| `error` | `SessionError`: io / malformed / ledger / no-checkpoint |

## Decisions

**Absent is a state, not an error.** Resuming a session that was never
saved answers `None` - a clean "nothing to resume" the caller renders.
An unreadable file is an error; a missing one is an answer.

**The file names its session, and the check is load-bearing.** Every
file stores its own id, and `load` refuses a mismatch: a session file
renamed or copied by hand would otherwise resume under the wrong id
with no error anywhere. The mismatch fixture had to write one session's
bytes under another's name - merely asking for an unsaved id exercises
absence, not mismatch, and the first fixture proved it by passing
vacuously.

**A branch is a copy, not a move.** `branch` mints a fresh id carrying
the same turns; the original file is untouched. Saving the branch adds
a file, never rewrites one.

**Save hardens the checkpoint boundary.** Each save serializes before touching disk, then uses an unpredictable sibling temporary created exclusively, syncs its bytes, atomically renames it, and syncs the parent directory. Failed saves remove their temporary. Saves for the same session are serialized in-process so concurrent writers cannot share or corrupt staging files. On Unix, the session directory is forced to `0700` and checkpoint files are created as `0600`; no predictable `.tmp` pathname is opened or followed.

**Save creates the directory it was given.** The first mutation run
proved the tests had never checked this: every fixture pre-created its
scratch directory, so dropping `create_dir_all` changed nothing
observable. Closed by saving into a `a/b/c` that does not exist.

**Export is Markdown, one heading per turn.** The transcript a human or
a downstream tool reads; `T30` will attach it to a command.

## Mutation results

Seven mutations; six caught, control survived.

| Mutation | Verdict |
|---|---|
| M1: resume loads any id from any file | CAUGHT |
| M2: branch shares the id | CAUGHT |
| M3: save skips the directory creation | CAUGHT *(survived first)* |
| M4: absent session is an error | CAUGHT |
| M5: export drops turn headings | CAUGHT |
| M6: list returns unsorted | CAUGHT |
| M7 control: comment only | SURVIVED (control) |

M3 survived first because every fixture pre-created its directory - the
recurring lesson, now in its fourth appearance: a value (or a
precondition) no test observes is one no mutation can break.

## Obligations left to later stages

- **T23** owns the turn loop that calls `record_turn` and `save` at
  step 13; this crate is the storage it calls into.
- **T28.7** (command registry) exposes resume/branch/export as
  commands over this crate's functions.
- **T30** wires the session directory from configuration.
