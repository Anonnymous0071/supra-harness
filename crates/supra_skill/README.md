# supra_skill

Skill loading, dependency resolution, and hot reload. **T19** of the stage
sequence.

## What this stage is for

A skill is a directory with a `SKILL.md` - front matter (`name`,
`description`, optional `requires`) plus a Markdown body. This crate loads
them from one skills directory, resolves dependencies, orders them, and
reloads them on watcher events.

| Module | Owns |
|---|---|
| `skill` | the SKILL.md format: fences, `key: value` grammar, verbatim body |
| `loader` | `Skills`: load, resolve, topological order, `apply_event` |
| `error` | `SkillError`: io / parse / missing field / unknown dep / cycle / duplicate |

## Decisions

**The layout is one directory per skill.** `skills/name/SKILL.md` - the
shape every skill system the user studied converged on: a skill can carry
its own files beside the manifest without colliding with siblings. The
first loader draft read flat files and every fixture caught it - the
layout is stated in the load doc and pinned by every test.

**The body is content, never a `system` mutation (I2).** The prompt
listing carries name + description only - what the model reads to decide
whether to open the body. The body rides as content on demand. This is
the token-efficiency contract Kimchi's skills and the 0xPony SKILL.md
files both taught, kept as the default shape.

**Parsing by hand, deliberately.** The front matter is a `key: value`
line grammar - small enough to own. A YAML dependency would carry its
version pins and its grammar ambiguities into a file the user edits by
hand (the T15.7 two-parsers lesson, applied before the second parser
could exist). Unknown keys are preserved verbatim, not refused: the
author's metadata is the author's business.

**Name is the front matter's, not the file's.** The name is identity -
what `requires` and the model refer to. A renamed file keeps its
identity; a moved file keeps its dependencies. A front-matter rename
through a watcher event is a drop plus a load, and both identities are
reported so the TUI can render "renamed old -> new".

**Reload refuses and stands.** `reload_from` applies events to a copy and
adopts only when everything resolved: a broken rewrite, a rename onto a
sibling's name, a reload that introduces a cycle - the author's error
leaves the session with the skills it had. This is the T15 watcher shape
(`apply_event(&notify::Event)`, the turn loop forwards), not a background
thread with its own timing.

**Determinism everywhere.** Paths sort, the topological walk keeps name
order among unrelated skills, `all()` iterates the name map - same
skills, same bytes, every session, the prefix property applied to skill
listings.

## Mutation results

Ten mutations; nine caught, one control survived by design.

| Mutation | Verdict |
|---|---|
| M1: cycle detection removed | CAUGHT |
| M2: duplicate check removed | CAUGHT |
| M3: unknown dependency accepted | CAUGHT |
| M4: topological order reversed | CAUGHT *(survived first)* |
| M5: failed reload still adopted | CAUGHT |
| M6: name not required | CAUGHT |
| M7: fences not required | CAUGHT |
| M8: remove event loads instead of drops | CAUGHT |
| M9: body trimmed (not verbatim) | CAUGHT |
| M10 control: comment only | SURVIVED |

M4 survived with the honest shape of the gap: the fixture's dependent
sorted *after* its dependency alphabetically, so BTreeMap iteration order
alone produced the correct answer and `push_order`'s recursion was never
exercised. Closed by renaming the dependent to sort first (the
recursion must run) and adding a diamond (one edge only proves one edge).
The lesson is the T18 pattern again: a surviving mutation is a statement
about the fixture.

## Obligations left to later stages

- **T23** renders the listing block (name + description, topological or
  name order per the prompt layout) and forwards watcher events to
  `reload_from`.
- **T17/T18 terms** - a skill that contributes tools appends through the
  registry at session start; a hot reload appends what arrived and never
  removes what a session already advertised (I3, the same terms MCP
  registers under).
