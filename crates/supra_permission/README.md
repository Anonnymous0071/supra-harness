# supra_permission

The host-side permission gate. **T16.7** of the stage sequence.

## What this stage is for

T6 (`supra_types::permission`) owns the two axes - `ToolClass`/`Invoker`
authority and `Mode`/`Reversibility` consent - plus the mode matrix and
deny-wins resolution. T16.7 owns what a tool call actually passes through:
the catalogue that turns a *resolved effect* into a class, the gate that
composes catalogue + rules + matrix, and the batching that makes many
questions one prompt.

| Module | Owns |
|---|---|
| `catalogue` | `Effect`: a closed enumeration of effect shapes, classified |
| `gate` | `Request`/`Outcome`, the gate, `Batch` |
| `error` | `PermissionError`: denied / empty batch |

## Decisions

**A closed enum, not a string classifier.** Section 6: "Classification runs
on the **resolved effect**, never the tool name: `shell_run(\"cargo test\")`
is R0, `shell_run(\"rm -rf node_modules\")` is R3." A `contains("rm")`
classifier is a second grammar that disagrees with the shell about `env rm`
and `\\rm` (T15.7's two-parsers lesson); the caller - T17's registry, which
knows the argv - resolves the effect into a shape carrying data (`Remove {
target }`, `RecoverableEdit { basis }`), and the catalogue maps shapes to
classes exhaustively. Both of the architecture's shell examples differ by
shape, not by tool, and a test pins that.

**Damping is routed, not duplicated.** `Reversibility::damped` (T6) does the
softening; the catalogue's only damping job is *routing*: the structural
shape (T15.7's reparse-gated splice) goes through it, everything else
around it. The measurable consequence, pinned by a test: under `ask`, a
blind edit prompts and a verified splice on the same file runs.

**The escape-hatch exception.** `yolo` pre-grants consent; stepping aside
from the sandbox is not consent's to grant. An `EscapeHatch` effect asks in
*every* mode, `yolo` included - the gate routes it before the matrix. A
rule deny still beats it (deny wins over everything), and a rule allow
short-circuits before the check, which is the rule set's prerogative and
the reason `builtin` is obliged to deny exactly the never-permittable.

**Rules, authority, matrix - in that order.** Deny wins literally (T6's
`resolve`); an allow skips consent; no rule falls through. Authority is
consulted before the mode is examined, so no mode can widen it - the
two-axis property, asserted exhaustively at the gate level. Plan refuses
rather than queues: a plan that queues its refusals is a staging area.

**Unanswered is not consent.** Batch answers align by item index; a missing
answer refuses. An empty batch is `EmptyBatch`, not an empty approval -
"a question the user did not answer is not consent" is the whole sentence.

## Mutation results

Ten mutations; nine caught, one control survived by design.

| Mutation | Verdict |
|---|---|
| M1: authority check removed | CAUGHT |
| M2: damping applied to every effect | CAUGHT |
| M3: escape hatch follows the matrix | CAUGHT |
| M4: deny ignored by the gate | CAUGHT |
| M5: allow no longer short-circuits | CAUGHT |
| M6: blind edit classified R1 | CAUGHT |
| M7: remove classified R2 | CAUGHT |
| M8: batch collects refusals too | CAUGHT |
| M9: unanswered batch items run | CAUGHT |
| M10 control: comment only | SURVIVED |

M9 is the batch's one-sentence contract: `repeat(&false)` is the difference
between "dismissed means no" and "dismissed means yes".

## Obligations left to later stages

- **T17** resolves each tool invocation into an `Effect` shape before the
  gate runs - the registry knows argv, paths, and the sandbox's network
  policy, which is why the shape carries data.
- **T29** renders `Outcome::Ask` (summary + effect `Display` + reason) and
  collects per-item answers back into `Batch::resolve`.
- **T30** assembles the rule set from configuration; the gate evaluates
  whatever `&[Rule]` it is handed, so no source needs a special path.
