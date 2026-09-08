# supra_dap

Three debug adapters over one DAP client: breakpoints and stack traces
through the Debug Adapter Protocol. **T25** of the stage sequence.

## What this stage is for

The debugger is the escalation tool the introspector cannot be: when a
gate fails and the finding points at a line, a breakpoint at that line
answers what the code actually did. DAP is LSP's framing with a
debugger's vocabulary - `seq`/`request_seq` instead of `id`, `command`
instead of `method`, and adapter-pushed events (`stopped`,
`terminated`) a debugger must wait for.

| Module | Owns |
|---|---|
| `adapters` | the three adapters and their language coverage |
| `framing` | DAP wire framing: Content-Length headers over stdio |
| `client` | the session: initialize, breakpoint, launch, stack trace |
| `error` | `DapError`: transport / protocol / uncovered / stopped |

## Decisions

**Three adapters cover five of seven languages.** `CodeLLDB` (Rust, C,
C++ - the LLVM debugger behind one adapter), `debugpy` (Python), `Delve`
(Go). TypeScript and JavaScript refuse rather than guess - the same
refusal T24 makes for uncovered languages, and for the same reason: an
adapter against the wrong runtime reports breakpoints that do not bind.

**The adapter's confirmation is the breakpoint.** A requested line can
move when the compiler plants it elsewhere; the confirmed position is
the one the stop will report, and `verified` is the adapter's word that
the binding holds. Both are read from the `setBreakpoints` body, never
assumed from the request.

**Responses correlate by `request_seq`, and failure is failure.** A
stale answer to an earlier request must not satisfy a later one; a
refused command that reports success is a silent lie. Both live in the
request loop, where no unit test of framing can observe them - guards
pin the two lines, the same closure shape T24 needed for its flip.

**The frame is Content-Length, always** - the same rule as T24's, for
the same reason: a boundary the sender did not state is one a reader
cannot trust.

## Mutation results

Seven mutations; two caught by tests, four by guards, control survived.

| Mutation | Verdict |
|---|---|
| M1: adapter coverage removed | CAUGHT |
| M2: request_seq correlation ignored | CAUGHT via guard |
| M3: failed responses treated as success | CAUGHT via guard |
| M4: confirmed line ignored | CAUGHT via guard |
| M5: verified flag dropped | CAUGHT via guard |
| M6: Content-Length not enforced | CAUGHT |
| M7 control: comment only | SURVIVED (control) |

M2-M5 all live in `Client::request` or `set_breakpoint`, where the
client's process plumbing (a real adapter process) is the only way a
unit test could reach them - and the harness does not ship adapter
binaries in CI. The guards pin the load-bearing lines instead; two
fixture tests (a two-seq framing probe, a parse of a real
`setBreakpoints` body) cover what pure parsing can.

Two guard-writing lessons from T22 reappeared and were reapplied: a
`scan` pattern whose subject is a string literal needs `scan_sql`
(`confirmed.get("line")` blanks to `confirmed.get("")` under plain
`scan`), and shell single-quoted ERE needs one backslash, not two - the
double-escaped `\\(` read as escaped-backslash plus a group and matched
nothing.

## Obligations left to later stages

- **T23** wires `wait_event("stopped")` into the turn loop's step 7
  escalation path when a finding asks for it; the client is synchronous
  and the runtime owns timing.
- **T29** renders the stack trace where the finding pointed.
