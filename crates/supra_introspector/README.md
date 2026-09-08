# supra_introspector

Static, dynamic, and cross-agent bug detection. **T22** of the stage
sequence.

## What this stage is for

The turn loop's step 7 names this crate a deterministic gate and step 9
names its output: "append findings; a confirmed finding escalates". §5's
instruction-efficiency table says the same thing from the model's side:
"verify with tests" is not prose - the introspector runs automatically
post-edit.

| Module | Owns |
|---|---|
| `gates` | the external gates: clippy (static), cargo test (dynamic), the cargo diagnostic parser |
| `cross` | cross-agent comparison: divergence is a finding, never a verdict |
| `finding` | `Finding`, `Kind`, the evidence reference |
| `bridge` | findings as blackboard claims; votes carrying the finding's evidence |
| `error` | `IntrospectError`: spawn / no-answers |

## Decisions

**A finding never escalates on its own.** The bridge publishes a finding
as a claim and peers vote; quorum (T21), not a linter, decides. The
mandate is cross-agent verification - mutual validation between peers -
so the introspector's job ends at a well-evidenced claim.

**The evidence reference is the finding's own.** `evidence_ref` is
`source#path:line`, sized for T6's `Verdict` evidence budget; the bridge
attaches it to every vote, so a confirming peer's verdict points at the
finding it confirmed. A dedicated test reads `bb_vote.evidence` back
from the store and asserts the string - the mutation that dropped the
`Some(...)` survived until that test existed.

**Gate failure is a refusal, never a pass.** A command that cannot start
is `IntrospectError::Spawn`; exit status feeds `passed` directly, and
clippy runs with `-D warnings` so the workspace's zero-warning policy is
the gate's own bar (measured: plain clippy exits 0 on warnings, which
made the first fixture pass a broken project).

**The diagnostic parser is line-oriented and lenient.** The cargo family
emits ` --> path:line:col`; the parser takes the path and line, and an
unlocated `error:` line still yields a finding - a finding the reporter
cannot place is still a finding. Noise lines (`Checking`, `Compiling`,
bare warnings) yield nothing.

**Cross-agent comparison counts, it does not judge.** Answers group by
exact text; a minority side produces one finding naming the split
("2 of 3 agree, 1 differ"), unanimity and single answers produce
nothing. Who is right is the blackboard's question, asked with this
finding as evidence.

## Mutation results

Ten mutations; all ten caught, control survived by design.

| Mutation | Verdict |
|---|---|
| M1: located diagnostic loses its position | CAUGHT |
| M2: unlocated errors yield nothing | CAUGHT |
| M3: gate exit status ignored | CAUGHT |
| M4: a gate that cannot run counts as a pass | CAUGHT |
| M5: cross-check reports unanimity | CAUGHT |
| M6: cross-check reports nothing on divergence | CAUGHT |
| M7: single answers report divergence | CAUGHT |
| M8: evidence ref drops the location | CAUGHT |
| M9: bridge votes without evidence | CAUGHT *(survived first)* |
| M10 control: comment only | SURVIVED (control) |

M9 survived first because the verdict's evidence was written to the
store but nothing read it back - the same shape as T21's roster bug: a
value no test observes is a value no mutation can break. Closed by
reopening the store and asserting `bb_vote.evidence` equals the finding's
reference.

## Obligations left to later stages

- **T23** runs the gates in parallel at step 7 and forwards peer answers
  to `cross_check`; the turn loop owns the scheduling.
- **T24/LSP** adds a fourth gate kind (diagnostics) through the same
  `Finding` shape; `Kind` is closed today and opens with it.
- **T15.5** consumes confirmed findings as a signal - the escalation is
  already wired through `escalates`.
