# supra_shell

Persistent pty shell sessions, spawned through the T16 sandbox. **T16.5** of
the stage sequence.

## What this stage is for

A shell tool that shells out one command at a time loses the process's
state every call: environment, working directory, loaded shell functions,
command history. A session keeps them. But a session is also a live child
attached to the harness through three descriptors, which is exactly the
shape the T4 boundary warns about - so the session is built as a
composition of the stages that already own each concern:

| Concern | Owner | How this crate uses it |
|---|---|---|
| pty pair, CLOEXEC from birth | `supra_ffi::pty` | `Pty::open` at a size; master for reads/writes, slave for the child |
| sandbox, fd audit, guard | `supra_sandbox` (T16) | `spawn` with the slave fd as stdio, explicit env, full audit |
| escape parsing, C1 positional | `supra_ffi::ansi` (T3) | every byte routes through the C++ scanner - never reimplemented |
| cell width, grapheme safety | `supra_ffi::width` (T3) | line truncation and transcript measurement |

## Modules

| Module | Owns |
|---|---|
| `session` | `ShellSession`: pty + child + shaper, read/write/resize/wait/kill |
| `shaping` | `Shaper`: deterministic transcript from raw pty bytes |
| `error` | `ShellError`: spawn / pty / child-handle / misuse shapes |

## Decisions

**No unsandboxed path, by construction.** The child is always spawned
through `supra_sandbox::spawn` with the workspace policy; there is no flag
in this crate that skips it, so `yolo` cannot. The T16 fd audit runs before
every exec, which is what keeps the pty's slave from being an exfiltration
channel.

**CLOEXEC from the syscall, not after.** `posix_openpt(O_CLOEXEC | ...)`
and `open(slave, O_CLOEXEC | ...)` set the flag as the descriptor is born.
`openpty(3)` would return both descriptors unflagged and leave a window in
which a concurrent audit sees the pair as a leak - a false positive under
exactly the load this harness exists for. The window is closed by
construction, not by ordering.

**The parent's slave copy closes at spawn.** The T4 note binds this
directly. The child holds its own dup2'd copies on 0/1/2; the parent's
copy would keep the pair alive after the child exits, and master reads
would block forever instead of seeing EOF. A dedicated test turns the
would-be hang into a deadline failure.

**Shaping is deterministic across chunk boundaries.** The same byte stream
produces the same transcript whatever chunks it arrived in - pinned by a
test that cuts mid-sequence, mid-escape, and mid-word on purpose. Three
mechanisms: the resumable C++ scanner (PARTIAL tokens are bookkeeping, not
content - skipped, like the C++ suite's reference consumer), explicit C0
semantics (`\n` commits a line, `\r` returns the cursor, backspace steps
left, other controls drop), and per-line cell-accurate truncation through
the C++ planner. A progress line `50%\r75%\r100%` shapes to `100%` -
what the terminal drew, not what the pipe carried.

**Text runs carry their own newlines.** The C++ scanner bundles `\n` inside
text tokens rather than emitting control tokens, so `apply` splits text
runs at newlines and commits per piece. Getting this wrong shows up as a
one-line transcript with embedded `\n` bytes - the first bug the shaping
suite caught.

**The prompt heuristic asks, never acts.** A short unterminated line ending
in a prompt glyph suggests the child is waiting for input. T4's
verification is explicit that this heuristic has false positives, which is
why it is a flag the caller reads (`suggests_prompt`) and not a trigger
this crate pulls.

**Read is blocking by design.** `read` returns when at least one byte
arrives - the shape a dedicated reader thread wants. The TUI owns that
thread; this crate stays synchronous and deterministic. EOF on the master
returns `Ok(None)`; an `EIO` after the child's exit is the pty's own
hangup, mapped to the same `Ok(None)` so a draining reader finishes
cleanly.

## Mutation results

Nine mutations; eight caught, one control survived by design.

| Mutation | Verdict |
|---|---|
| M1: drop `take_slave` (parent holds slave) | CAUGHT - EOF deadline test |
| M2: drop `\r` handling | CAUGHT |
| M3: drop `O_CLOEXEC` from `posix_openpt` | CAUGHT |
| M4: no stdio override in session | CAUGHT - `test -t 0` sees `/dev/null` |
| M5: ring never pops | CAUGHT |
| M6: prompt heuristic always false | CAUGHT |
| M7: `read` stops feeding the shaper | CAUGHT |
| M8: comment-only change | SURVIVED (control) |
| M9: vanished fd treated as live leak | CAUGHT - synthetic-directory test |

M9 is the T16 audit's lesson restated: a descriptor that closed mid-walk
cannot cross an exec, so it is absence, not danger. The unit test feeds
the walk a directory whose single entry names an impossible fd number and
asserts the record is skipped - deterministic, where the real race it
models is not.

## Parallel-test stability

Two races surfaced under `--test-threads=N` and both were closed
structurally:

1. **The vanish race.** A session dropping its pty while another test's
   audit walks `/proc/self/fd` produced `LeakyDescriptor { path: "" }` -
   a closed descriptor reported as a leak. Fixed in the audit (skip, as
   M9 documents); verified with ten consecutive parallel runs.
2. **The probe race.** Tests that manufacture leaks by clearing CLOEXEC
   share a binary with tests whose spawns must see a clean host. Fixed
   with a test-only `AUDIT_LOCK` in `supra_sandbox`; production has no
   such sharing - the turn loop is the only spawner and never clears
   flags - so the lock lives under `cfg(test)`.

## Obligations left to later stages

- **T16.7** rates a session's spawn like any other: the session forwards
  `Mode` and `Reversibility` in the `SpawnRequest` and refuses what the
  gate refuses.
- **T23** (`supra_core`) owns the reader thread and the session
  lifecycle across turns; this crate is the synchronous primitive
  underneath.
- **T29** (`supra_tui`) renders the shaped transcript and surfaces
  `Shaped`'s counters (lines, cells, truncated, dropped) on the status
  line.
