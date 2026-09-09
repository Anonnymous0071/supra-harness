# supra_cli

The single binary. **T30** of the stage sequence.

## What this stage is for

One binary, `supra`, with subcommands `run` (default), `eval`, `update`,
and `config show`. Wiring only: every decision lives in the library
crate it wires, and this crate assembles them in startup order -
discover, resolve, log, secrets, registry, runtime.

| Module | Owns |
|---|---|
| `args` | `Cli`, `validate` - the confirmation gates |
| `startup` | discover, resolve, logging, secrets |
| `registry` | command registry, session dir, hooks, consent, theme |
| `runtime` | tier estimate, admission plan |

## Decisions

**Two flags need their own confirmation.** `--ignore-project-config`
and `--sandbox off` each require `--yes`. The first escapes the
project tightening rule, the second disables the sandbox - both are
trust decisions, and a trust decision without confirmation is a
bypass with a flag.

**Eval is offline-first.** `supra eval` always runs the shape-check;
`--live` is the exception. Without a credential the probe skips
explicitly and exits 0 - silence would read as a pass, so the skip
says so. With a credential it refuses until the networked probe
lands.

**Update refuses before it verifies.** `update apply` bails without
a verified signature: fetch, verify, then apply, in that order.
`update check` names `supra_update::verify` so the operator knows
what verifies the artefact.

**The runtime reports the admitted tier.** `describe` names the tier
`admit` returned under the limit, not the tier `estimate` requested -
limit 6 reduces E3 to E2/k=5, and the line says E2.

## Mutation results

Seven behavior mutations, all caught.

| Mutation | Verdict |
|---|---|
| P1: `--ignore-project-config` guard dropped | CAUGHT |
| P2: `--sandbox off` guard dropped | CAUGHT |
| P3: eval offline-first dropped | CAUGHT |
| P4: live-skip `Ok` becomes `bail` | CAUGHT |
| P5: apply without verification allowed | CAUGHT |
| P6: check stops naming the verifier | CAUGHT |
| P7: presence refusal becomes `Ok` | CAUGHT |

Two probes survived first and taught the same lesson twice: a
`println!`-only skip and a help-text change no test observes. Both
were fixed by extracting `live_probe_report` and
`update_check_message` as testable pure functions.

Six guards in `check-invariants.sh`, probed 6/6. Three need
`scan_sql` - the refusal text, the `"skipped"` marker, and the
verifier name all live inside literals, which plain `scan` blanks
(the T29 lesson, third restatement). The sandbox pattern matches the
shape, not `"off"`, for the same reason.

## Obligations left to later stages

- **T31** owns the raw-mode terminal, the event pump, and the
  `ctrl+o` binding; the 13-step turn loop drives through
  `runtime::plan_turn` and reports through `StatusLine`.
