# supra_plugin

The wasmtime host: WIT-world plugins with ToolClass-derived import sets.
**T20** of the stage sequence.

## What this stage is for

Two sentences from the architecture carry the whole stage:

- §3: "capability isolation by absence: an agent has no import through
  which to spawn."
- §6: `ToolClass` is enforced by "absence of the WASM capability".

Isolation here is not a check that refuses after the fact - it is a
**namespace that simply has nothing in it**. The host builds one linker
per class, each holding exactly the class's imports from
`world::imports_for`, so a guest has no name to call anything outside its
class through.

| Module | Owns |
|---|---|
| `host` | `Host`, `Plugin`, `HostState`: engine, linkers, verify, instantiate, call |
| `world` | the WIT world text (`supra.wit`, one source) and the class ladder |
| `error` | `PluginError`: engine / unlisted import / missing host import / signature / fuel / trap |

## Decisions

**One linker per class, isolation by absence.** The Agent-class linker
has no Host or User entries; the Host-class linker has no User entries.
`verify` type-checks a component's *declared imports* against its class
set **before instantiation** - `UnlistedImport` names the import's path -
and `instantiate` runs the same check itself, so a caller that skips
`verify` still cannot link an over-classed component (a dedicated test
pins the paranoid path).

**The world is three ladders up one file.** `agent`, `host`, `user`
worlds ascending the same ladder as `ToolClass`, each exporting
`run(string) -> string` and importing its class's interfaces. The WIT
text is stated once in `supra.wit` (`include_str!`), documented verbatim
in `world.rs`'s docs, and a test pins the agreement - one source, two
consumers only works when there is one source.

**Import names, three spellings, verified against the parser.** The text
format accepts flat kebab extern names (`spawn`); the WIT path
(`supra:plugin/host-tools#spawn`) is a *source-level* spelling a toolchain
lowers away. `import_in_set` accepts the interface, `interface#function`,
and bare-function spellings - the three one toolchain or another produces,
and no fourth. This was measured, not assumed: the first fixtures named
WIT paths and the parser refused them with `#spawn: trailing characters`.

**Fuel is the hang containment.** `consume_fuel(true)` on the engine,
`set_fuel(DEFAULT_FUEL)` on every store, per instantiation,
non-refillable. An infinite-loop guest dies in milliseconds as
`FuelExhausted` (component and budget named), not in minutes as a hang.
The classification greps the formatted error chain for the word `fuel` -
cheaper than downcasting through wasmtime's error type, and the
infinite-loop test pins it.

**The string ABI, learned from the parser.** A lifted `(string) ->
(string)` needs `(memory ...)` and `(realloc ...)` in the `canon lift` -
without either the component is refused at parse time (`canonical option
realloc is required`). The round-trip fixture's guest answers from its
**own** data segment (a `(ptr, len)` pair it writes at a known offset),
not the argument's pointer: the argument's memory belongs to the caller,
and a guest returning it traps `string pointer/length out of bounds` -
three fixture shapes measured that before the fourth landed.

**Component calls own their cleanup.** With the pinned Wasmtime 48 API,
`TypedFunc::call` lifts the owned result and executes canonical post-return
before returning it. A post-return failure therefore returns from the same
call as `PluginError::Trap`; fuel exhaustion during either guest execution
or cleanup remains `FuelExhausted`. The repeated-call fixture pins that a
successful call leaves one component instance reusable. The deprecated
public `post_return` method is intentionally not called because it is a
no-op in this Wasmtime version.

**`HostState` is the T23 socket.** One state per component instance, host
functions closing over it; today it carries the call log the tests and
the audit read, and T23's dispatch (file reads through the workspace,
spawns through the sandbox) plugs into the same shape.

## Mutation results

Ten mutations; all ten caught, control survived by design.

| Mutation | Verdict |
|---|---|
| M1: verify skipped by instantiate | CAUGHT *(survived first)* |
| M2: unlisted import accepted | CAUGHT |
| M3: run signature not checked | CAUGHT |
| M4: fuel not enabled | CAUGHT |
| M5: fuel budget not set | CAUGHT |
| M6: fuel trap not classified | CAUGHT |
| M7: agent sees spawn (ladder broken) | CAUGHT |
| M8: bare-function spelling dropped | CAUGHT |
| M9: import set accepts everything | CAUGHT |
| M10 control: comment only | SURVIVED (control) |

M1 survived first for the same reason T17's manifest guard needed the
inverted shape: every test called `verify` explicitly before
`instantiate`, so an `instantiate` that skipped its internal verify was
unobservable. Closed by a test that instantiates an over-classed
component *without* prior verify - the paranoid path is now the pinned
path.

## Obligations left to later stages

- **T23** supplies the real dispatch behind `HostState`'s stubs: reads
  through the digest, spawns through T16's sandbox, snapshots through
  T16.6 - the stubs answer `host:{path}` today and the call log is the
  audit trail either way.
- **T21/T22** peers run as components through this host; the cohort's
  k is bounded by the fuel budget as much as by the peer limit.
- **T30** loads plugin components from configuration and hands them to
  this host; `Plugin::load_file` is the entry it calls.
