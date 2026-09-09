# supra_hook

Eight lifecycle hooks, prefix-safe by type. **T27** of the stage
sequence.

## What this stage is for

A hook is a user command that runs at a lifecycle point. The safety
rule is structural, not advisory: only the four boundary points accept
hooks, and the four inside points refuse at registration.

| Module | Owns |
|---|---|
| `point` | the eight points, the prefix-safety rule, parse |
| `registry` | register, look up, fire; `shell_words` |
| `error` | `HookError`: unknown point / not prefix-safe / command |

## Decisions

**Prefix safety is a type-level fact, enforced at registration.**
`HookPoint::is_prefix_safe` answers for the four boundaries
(session start, session end, turn start, turn end); the registry
refuses anything else with `NotPrefixSafe`. A user command executing
mid-prefix can mutate what the provider has already cached, and a cache
break is the tax nobody asked for - so the inside points (before/after
tool, before evict, after cache break) exist in the enum, are
nameable in configuration, and cannot be registered.

**A hook is an observer, not a gate.** A failing command is reported
(`Command`) but does not stop the other hooks at the same point. Exit
42 requests a `Stop` the caller may honor; already-sealed segments stay
sealed - a hook cannot unwrite the ledger.

**The payload is what fired it, and nothing else.** The triggering
`Event` travels as JSON on stdin (`{"TurnStarted":{"turn":"..."}}` -
externally tagged, serde's default, verified by probe before the
fixtures were written); `SUPRA_HOOK_POINT` and `SUPRA_TURN_COUNT` ride
the environment. No handle to the ledger, no path to mutate the prefix.

**`shell_words` is POSIX for the subset hooks use.** Single- and
double-quoted segments, backslash escapes outside single quotes - and
backslash is *not* special inside single quotes, which the first
implementation got wrong: it unescaped `\"` inside a single-quoted
python `-c` argument, and the hook command that worked verbatim in a
shell failed under the harness. Measured by running the exact command
both ways before fixing the helper.

## Mutation results

Eight mutations; seven caught, control survived.

| Mutation | Verdict |
|---|---|
| M1: prefix-safety check dropped | CAUGHT |
| M2: all points report prefix-safe | CAUGHT |
| M3: exit 42 no longer stops | CAUGHT |
| M4: a failing hook is silent | CAUGHT |
| M5: the event is not sent to stdin | CAUGHT |
| M6: point name not in environment | CAUGHT |
| M7: unknown names parse as turn-start | CAUGHT |
| M8 control: comment only | SURVIVED (control) |

## Obligations left to later stages

- **T23** fires `TurnStart`/`TurnEnd` at the loop boundaries; the
  registry is a field of the runtime it constructs.
- **T30** loads hooks from configuration through `register_named`,
  where the point-name parse refusal surfaces a typo loudly.
