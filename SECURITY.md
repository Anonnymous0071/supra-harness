# Security policy

## Reporting a vulnerability

Report privately through GitHub's [Report a vulnerability][advisories] flow.
Do not open a public issue.

Include: affected version or commit, reproduction steps, and the impact you
believe it has. If you have a proof of concept, attach it privately.

[advisories]: https://github.com/Anonnymous0071/supra-harness/security/advisories/new

## Scope

The repository contains sandbox, permission, journal, guard, AST, tool, and plugin
components intended to protect model-directed commands. The current `supra run`
provider path advertises no tools and rejects tool use, so it does not yet execute
model-directed commands through those components. The following are in scope for
the library boundaries and for any executable path that integrates them:

- **Sandbox escape.** Any command reaching outside the configured filesystem or
  network policy (T4, T16).
- **Self-spawn.** Any path by which an agent starts another agent or another
  copy of the harness, defeating the seven guard layers (T12.5).
- **Permission bypass.** An irreversible action taken without consent in `plan`,
  `ask`, or `auto` mode (T16.7).
- **Secret disclosure.** A credential written to logs, session transcripts,
  input history, telemetry, or a provider request (T8, T12, T26, T28).
- **Journal corruption.** Undo destroying user work, or a snapshot that cannot
  restore what it recorded (T16.6).
- **Plugin capability escalation.** A WASM component obtaining an import outside
  the verified allowlist (T20).
- **Prompt injection with effect.** Repository or tool-output content causing an
  action the permission gate should have refused.

Out of scope: the LLM producing wrong code; provider outages; cost incurred by
`yolo` mode with sandbox disabled, which is documented and confirmed.

## Design commitments

**Library boundaries do not treat permission mode as authority.** `yolo` removes
user prompts in the permission component only. Guard, sandbox, and AST components
remain separate enforcement layers. The current `supra run` path advertises no
tools; when executable tool integration lands, disabling the sandbox must remain
a separate confirmed `--sandbox off` choice and be surfaced persistently.

**Capability by absence, not by refusal.** A runtime check is code that can have
a bug. A missing ABI cannot. Agent WASM components are linked against an
allowlist verified to match **exactly** - not a superset - so an agent has no
vocabulary in which to request a spawn.

**Secrets never rest in plaintext.** T12 stores credentials in the OS keyring,
falling back to AES-GCM with PBKDF2 at 600k iterations in a 0600 file. Values
are zeroized on drop and redacted before logging.

**Irreversibility is classified, not assumed.** Classification runs on the
resolved effect rather than the tool name: `shell_run("cargo test")` is R0,
`shell_run("rm -rf node_modules")` is R3. When classification is uncertain the
result **fails closed** to R3.

## Known limitations

Stated openly rather than left for a reporter to discover:

- **Syntactic rename** without a language server can be wrong under shadowing or
  overloading. Results are flagged `semantic: false`; they are not silently
  presented as verified.
- **The environment-marker guard layer** can be stripped by a command that
  deliberately clears the environment. The process-tree budget catches the
  consequence, not the intent.
- **Interactive-prompt detection** is heuristic and will produce false
  positives, which is why its action is to ask the user rather than kill
  silently.
- **The journal cannot undo non-filesystem effects.** A sent network request or
  a pushed commit is gone. Such effects are classified R3 and never run
  unprompted in `auto`.

## Supported versions

Pre-alpha. No released version is supported yet; fixes land on `main`.
