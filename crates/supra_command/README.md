# supra_command

The command registry and palette. **T28.7** of the stage sequence.

## What this stage is for

Commands are descriptions plus an authority class; the runtime owns the
action, so the registry never holds a closure over harness state. The
registry resolves slash input against the same authority axis the
permission gate uses - a command is subject to the gate, not exempt from
it, and `yolo` lifts consent, never authority.

| Module | Owns |
|---|---|
| `command` | `Command`, the built-in set, `FORBIDDEN` |
| `registry` | `Registry`: register, resolve, fuzzy search |

## Decisions

**Two prohibitions live in code and are tested, not remembered.** There
are no `cost`/`cache`/`context` commands (section 7 - transparency that
must be requested is never consulted when it matters, so those surfaces
live in the status line, T29), and there is no `/think` (the thinking
budget is frozen per session, section 6). `FORBIDDEN` names them; the
test asserts none of them appears in the built-ins; a regression that
adds one is a design regression, not a feature.

**Authority is checked at resolve, on the gate's axis.** `/sandbox`
demands `ToolClass::Host`; an Agent invoker is refused even under
`yolo` - the same two-axis property T16.7 established, applied to the
palette.

**Names are lowercase letters, digits, and hyphens.** Register-time
refuses anything else: a name with a space or a semicolon is a name the
palette cannot type safely, and an empty name is not a command.

**Search is a subsequence in name order.** A command matches when the
query's characters appear in order in its name; the query lowercases
first; ties keep name order, so the palette never reorders under a slow
typist.

## Mutation results

Seven mutations; six caught, control survived.

| Mutation | Verdict |
|---|---|
| M1: authority check dropped | CAUGHT |
| M2: duplicates accepted | CAUGHT |
| M3: bad names accepted | CAUGHT |
| M4: search ignores the query | CAUGHT |
| M5: forbidden commands exist | CAUGHT |
| M6: search is case-sensitive | CAUGHT |
| M7 control: comment only | SURVIVED (control) |

## Obligations left to later stages

- **T29** renders the palette from `search`, and the TUI's command
  entry routes through `resolve` so the authority check cannot be
  bypassed by typing.
- **T30** wires command names to their actions; the registry holds the
  descriptions, the binary holds the behaviour.
