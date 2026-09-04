# supra_log

Structured diagnostics. **T8** of the stage sequence, and the one crate in the workspace
permitted to write to stderr — `print_stderr` is banned workspace-wide so that every
diagnostic arrives here, in the same way `unsafe_code` is banned everywhere but T5.

## Modules

| Module | Owns |
|---|---|
| `redact` | what a credential looks like, and the two things that must survive being looked for |
| `sink` | one line per write, size-bounded rotation, an owner-only file, stderr under a guard |
| `subscriber` | the `tracing` wiring: filter, format, and whole-event granularity |

## Redaction is at the sink, not the call site

T7 removed every field that could hold a credential from the configuration schema, and a
test still found a leak: the error *refusing* a pasted credential quoted the line it was
refusing. The lesson was not "fix that message" — it was that a secret reaches a sink
through paths nobody enumerated in advance.

So the net goes at the last moment before bytes become durable, over the whole formatted
line. It is explicitly a net rather than the primary mechanism. The primary mechanisms are
upstream: T7's schema, and T12's `Secret<T>`.

Two mechanisms, because each misses what the other catches:

- **by field name** — `"api_key":"anything"` is redacted whatever the value looks like,
  which is the stronger rule: it does not care whether the secret is shaped like one;
- **by value shape** — a credential pasted into a free-text message has no field name to
  catch it, but `sk-` followed by forty-eight token characters is recognisable anywhere.

### What must survive, and why that shapes the design

A generic "high entropy" rule would be more thorough and would make this crate useless.
supra's own diagnostic material *is* high-entropy strings: ULIDs, content hashes, cache
keys, git revisions. Two survivals are load-bearing enough to be tests:

- a `ContentHash`, in both its 64-character and 12-character forms, because every
  prefix-stability diagnosis is a comparison between two of them;
- the value of `api_key_env`, which T7 defines as the **name** of an environment variable.
  Redacting `ANTHROPIC_API_KEY` would hide the one thing a reader needs to fix a
  credential problem.

The field-name rule is therefore an **exact** match and never a prefix match: `api_key`
is a secret, `api_key_env` is a signpost, and the difference is four characters.

### The bug in the fast path

The pre-check that lets ordinary lines skip the matcher originally listed its own
literals — `"sk-"`, `"gh"`, `"xox"`. It was wrong within minutes: **`github_pat_` does not
contain `gh`**, because the letters are g-i-t-h. Every GitHub personal access token passed
straight through a matcher that would have caught it, and the failure was silent in the
only direction that matters.

The pre-check now reads the same tables the matcher does, so drift is impossible by
construction, and a property test walks every entry to confirm it. The general lesson,
recorded in `docs/ARCHITECTURE.md`: a second, weaker copy of a security check's knowledge
is a place for the two to disagree, and it will fail closed-eyed.

## The sink

**One line, one write.** Every line reaches the file in a single `write` on an `O_APPEND`
descriptor, so two supra processes sharing a log produce whole lines in some order rather
than shredded ones. It is also why redaction is sound here: the whole line is in hand
exactly once, so a secret cannot slip through by being split across two writes. There is a
threaded test for the first property and a split-write test for the second.

**Bounded.** Size-based rotation with a fixed number of kept files, so a harness running
for days cannot fill a disk. An existing file's length counts toward the threshold at open,
or a restart would reset the bound.

**Owner-only.** The file is created `0600`, for the same reason T7 holds the user's
configuration to `0600`: a log written beside it at `0644` would make that pointless.

**stderr under a guard.** While the TUI owns the terminal, a stray line would tear the
frame, so mirroring is suppressed for the lifetime of `suppress_stderr`'s guard. A guard
rather than a pair of setters, because a `set(false)` whose `set(true)` is missed does not
fail loudly — it silently discards every diagnostic for the rest of the session. A line
written under the guard still reaches the file; suppression is about the terminal, not
about the record.

**A lost line is reported.** `flush` and `Drop` cannot return an error, so a failed write
has nowhere to go. Rather than swallow it, the sink counts it and prepends a notice to the
next line that succeeds. A gap in a log is only debuggable if the log says there is one.

## Whole-event granularity

Redaction is only sound if it sees a complete line. The `fmt` layer calls
`make_writer_for` once per event and writes the whole formatted event with a single
`write_all`, so a writer that accumulates and emits on drop observes exactly one event at a
time. That was read out of the layer's source rather than assumed, because a writer that
saw half an event could pass half a credential.

`build` returns a subscriber without installing it, which is what makes this testable: a
test scopes one with `tracing::subscriber::with_default` and asserts on the file, as many
times as it likes. Exactly one test installs a global subscriber, because a process may
only do that once.

## Two verified facts worth knowing

**`log_internal_errors` defaults to false.** When a writer fails, `tracing-subscriber`
would otherwise `eprintln!` directly — bypassing the TUI suppression entirely. The default
is off, so it does not; the sink's own dropped-line counter is the reporting mechanism.

**Almost any string parses as a filter directive.** A bare word is read as a *target
name*: `"not a level"` parses successfully into `not a level=trace`, and so would `"inf"`.
A string default would therefore not fail on a typo — it would silently produce a filter
that enables trace for a target nothing logs to and silences everything else. `build`
takes a `LevelFilter`, which cannot be misspelled. Found by a test that asserted a
fallback which cannot occur.

## Why this crate does not depend on T7

`supra_config` comes earlier and it would be natural to take a resolved `Config` here. It
would also be a mistake: configuration loading is exactly when you need diagnostics, and a
logger that cannot start until configuration has loaded cannot report why configuration
failed to load. `LogOptions` is plain data; T30 translates a config into it.

For the same reason the shipped crate depends on nothing from the contract layer —
`supra_types` is a dev-dependency, needed only by the test that proves a `ContentHash`
survives redaction.

## Mutation results

Twelve mutations. Eleven caught; one survives for a reason worth stating.

| Mutation | Verdict |
|---|---|
| M1 the fast path restates the literals | CAUGHT |
| M2 a prefix inside a token becomes a match | CAUGHT |
| M3 the field rule becomes a prefix match | CAUGHT |
| M4 the sink stops redacting | CAUGHT |
| M5 the log file becomes world-readable | CAUGHT |
| M6 rotation never fires | CAUGHT |
| M7 the oldest rotated file is not removed | **SURVIVED — platform-equivalent** |
| M8 the stderr guard stops restoring | CAUGHT |
| M9 the writer emits per `write` call | CAUGHT |
| M10 a failed write is forgotten | CAUGHT *(survived first)* |
| M11 an existing file's length is ignored | CAUGHT |
| M12 `flush` leaves the buffer, double-emitting | CAUGHT |

**M7 is not a test gap.** On Unix, `rename` replaces an existing target, so removing the
oldest file first is redundant; on Windows, `rename` to an existing path fails and the
removal is required. The mutation is semantically equivalent on the platform the suite runs
on. Recorded as such rather than papered over with a contorted test — the line is
platform-defensive, not untested logic.

**M10 was a real gap.** The only test touching the dropped-line counter set it by hand, so
nothing exercised the increment. Closed with `/dev/full`, which accepts an open and answers
every write with `ENOSPC` — a deterministic version of the failure a full disk produces.
Two tests now cover it, including the subtle half: the notice is composed *before* the
write and clears the count, so a failure must put the old count back along with the new one.

## Obligations left to later stages

- **T12** owns `Secret<T>`, whose `Debug` and `Display` reveal nothing. That is the
  primary mechanism; this crate is the net beneath it.
- **T29** holds the stderr guard for as long as the TUI owns the terminal.
- **T30** translates a resolved `Config` into `LogOptions`, and owns any flag that selects
  the format or the level.
