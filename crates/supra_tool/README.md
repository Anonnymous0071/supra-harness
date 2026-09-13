# supra_tool

The tool registry: frozen manifests, robust invocation for weak tool-callers,
and instruction-carrying preconditions. **T17** of the stage sequence.

## What this stage is for

The tool surface between the model and the harness. Three bindings shaped
it, each from the architecture document:

- **I3** - tools frozen for the session lifetime. Every tool registers at
  startup; disabling uses `allowed_tools`/`tool_choice`, never removal;
  the manifest is byte-stable so the prefix at BP1 never breaks.
  [`Registry`] makes this structural: no `remove` exists, and `disable` is
  a session set the dispatcher consults, not a mutation.
- **§5 instruction efficiency** - the workflow is not described in prose;
  it is the only path the tool surface permits. [`Precondition`] is that
  principle as a type: `edit_file` fails with a structured instruction
  ("read_file {path} before editing it") when the file was not read this
  session, replacing ~10 unreliable prompt tokens with zero prompt tokens
  and a reliable refusal.
- **The robust-invocation mandate** - a weak tool-calling model must never
  lose the loop to a tool error. Every [`ToolError`] is structured and
  field-naming, because the message is the model's only channel and the
  retry has to be able to be correct.

## Modules

| Module | Owns |
|---|---|
| `registry` | `Tool`, `Registry`, `Precondition`, `SessionFacts`, `Invocation` |
| `schema` | `Schema`/`Field`/`FieldType`: the closed argument language |
| `error` | `ToolError`: the structured refusals |

## Decisions

**A closed schema language, not JSON Schema.** Validating with one schema
dialect and describing with another gives two implementations that
disagree on exactly the inputs a weak model produces (the T15.7
two-parsers lesson). `Schema` is objects with typed, required-or-optional
fields, strict about unknowns - and nothing more, until a real tool needs
more. The provider-facing manifest renders from the same `Field` list that
validates, so the two cannot disagree; a test pins that agreement.

**Unknown fields are refused, not tolerated.** An extra `pathh` beside a
valid `path` is a typo the tool would otherwise run against its default -
silent corruption wearing a success. Strictness here is the cheap half of
robust invocation.

**One serialiser, no second parse.** Argument text goes through
`supra_llm::canonicalize` (T13) exactly once - the strict,
duplicate-refusing serialiser that is the only sanctioned producer of
`CanonicalJson`. The invocation carries the canonical bytes to the ledger
unchanged; this crate never re-serialises a parsed value, so what was
hashed is what reaches the wire.

**The effect resolver rides the manifest.** T16.7's rule - classification
runs on the resolved effect, never the tool name - lands here because the
registry is the layer that knows the arguments. `shell_run("cargo test")`
and `shell_run("rm -rf node_modules")` resolve to different `Effect`s from
one resolver, and a test pins exactly that pair.

**Registry vs gate vs executor.** The registry validates and resolves; it
does not decide or run. The resolved effect travels with the invocation to
the permission gate (T16.7, owned by T23's turn loop), and execution
dispatch is the turn loop's too. The registry's job ends at a validated
`Invocation`.

## Mutation results

Ten mutations; nine caught, one control survived by design.

| Mutation | Verdict |
|---|---|
| M1: unknown tool lists nothing | CAUGHT |
| M2: disabled check removed | CAUGHT |
| M3: non-object arguments pass as empty | CAUGHT |
| M4: schema validation skipped | CAUGHT |
| M5: precondition check skipped | CAUGHT |
| M6: unknown fields tolerated | CAUGHT |
| M7: required fields become optional | CAUGHT |
| M8: type check dropped | CAUGHT |
| M9: manifest skips disabled | CAUGHT |
| M10 control: comment only | SURVIVED |

M6 is the silent-corruption case the strictness exists for; M9 is I3's
manifest-stability property (a disable that changed the manifest would
break the BP1 prefix).

## Delivered boundary and runtime ownership

- **T20** derives each peer's WASM import set from `Tool::class`, so an
  agent has no name to call a Host or User tool through.
- **Delivered in the runtime seam:** invocation effects can be evaluated against
  permission policy before execution, and `SessionFacts` grows as reads occur.
  Concrete CLI/plugin adapters remain responsible for wiring that seam to their
  execution environment.
- **T18/T19** (MCP, skills) register their tools through this registry at
  startup - dynamic discovery appends, which I3 permits, and the manifest
  test's byte-stability property is what makes that safe.
