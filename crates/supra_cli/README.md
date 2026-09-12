# supra_cli

The single binary. **T30** of the stage sequence.

## Current executable behavior

`supra` requires an explicit subcommand: `run`, `eval`, `update`, or
`config show`.

`supra run <TASK...>` resolves configuration and credentials, selects one
configured Anthropic, OpenAI, or Google transport, derives a cohort tier from
simple task text evidence, requests one proposal, and validates it sequentially
with the admitted peers through `supra_core::Turn`. The accepted proposal is
saved to a new session and printed.

Requests currently advertise no tools. A provider response that requests tool
use is refused rather than returned as a completed answer. The sandbox,
permission, journal, AST, digest, hook, MCP, skill, plugin, TUI, and deterministic
gate crates are not yet integrated into this provider-turn path.

| Module | Owns |
|---|---|
| `args` | CLI parsing and confirmation gates |
| `startup` | config discovery, logging, and secrets |
| `registry` | assembly of session directory and library capabilities |
| `runtime` | provider turn, task estimate, admission, validation, persistence |

## Commands and limitations

- `supra eval` runs the offline shape check.
- `supra eval --live` performs network requests only for configured Anthropic
  and OpenAI providers with available credentials. Google is not probed.
- `supra update check` verifies an explicit local archive against its canonical,
  signed manifest and public key; it performs no network discovery.
- `supra update apply` repeats that verification and atomically replaces an
  explicit destination or the running executable on Unix. Windows verifies but
  refuses application until an equivalent atomic replacement exists.
- `--ignore-project-config` and `--sandbox off` each require `--yes`. Because
  `run` does not execute tools, the sandbox selection currently has no command
  execution path to affect.

The complete digest, prompt-ledger, concurrent fan-out, deterministic-gate,
tool-execution, TUI, and statistics lifecycle in `docs/ARCHITECTURE.md` remains
target executable integration, not current CLI behavior.
