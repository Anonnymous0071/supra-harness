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
| `host` | `Host`, `Plugin`, `HostState`: engine, class linkers, verify, instantiate, call |
| `dispatch` | injectable `HostDispatcher`, closed WIT routes, fail-closed default, limits, audit records |
| `world` | the WIT world text (`supra.wit`, one source) and the class ladder |
| `error` | `PluginError`: engine / import / signature / dispatch / fuel / guest trap |

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

**`HostState` is the runtime socket.** `Host::with_dispatcher` accepts an
`Arc<dyn HostDispatcher>` supplied by the runtime. Dispatch receives a closed
`HostFunction` enum rather than caller-selected path text, so only functions
registered in the plugin's class linker can reach it. This crate deliberately
does not execute CLI tools or assemble digest, permission, sandbox, journal,
or TUI services; the runtime adapter owns that policy and behavior.

`Host::new` installs `DenyAllDispatcher`: an allowed import is present but its
call traps as `PluginError::HostDispatch` until the runtime injects behavior.
It never returns fake success. Dispatcher failures and argument/result/call
bound refusals use the same distinct error variant; unrelated guest traps stay
`PluginError::Trap`.

**Host calls and their audit are bounded per instance.** `HostLimits` caps
argument bytes, result bytes, and the number of attempted dispatches. The call
cap is also the retained audit cap. Accepted attempts are recorded before the
dispatcher runs, including dispatcher and oversized-result failures; an
oversized argument or exhausted call cap is rejected before retaining more
untrusted text. `Plugin::calls` exposes the ordered `HostCall` records
read-only.

## Runtime integration

The runtime can inject its supra_tool/permission/sandbox/journal-backed adapter
without coupling those crates to the component mechanism:

```rust
use std::sync::Arc;
use supra_plugin::{DispatchError, Host, HostDispatcher, HostFunction};

struct RuntimeDispatcher;

impl HostDispatcher for RuntimeDispatcher {
    fn dispatch(
        &self,
        _caller: &supra_plugin::PluginIdentity,
        function: HostFunction,
        argument: &str,
    ) -> Result<String, DispatchError> {
        match function {
            HostFunction::ReadFile => todo!("workspace-bounded read of {argument}"),
            HostFunction::Search => todo!("digest-backed search of {argument}"),
            HostFunction::Spawn => todo!("permission and sandbox-backed spawn of {argument}"),
            HostFunction::Snapshot => todo!("journal-backed snapshot of {argument}"),
            HostFunction::AskUser => todo!("TUI-mediated question {argument}"),
        }
    }
}

let host = Host::with_dispatcher(Arc::new(RuntimeDispatcher))?;
# Ok::<(), supra_plugin::PluginError>(())
```

The dispatcher is synchronous because the current Wasmtime callbacks are
synchronous. A future async runtime must deliberately migrate the host to
Wasmtime's async component API rather than block inside this trait.

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

- **T23/runtime assembly** implements the injected dispatcher with
  workspace-bounded reads, digest search, permission-gated sandbox spawn,
  journal snapshots, and TUI-mediated user questions. This crate supplies the
  class-safe routing seam and fail-closed behavior, not those services.
- **T21/T22** peers run as components through this host; the cohort's
  k is bounded by the fuel budget as much as by the peer limit.
- **T30** loads plugin components from configuration and hands them to
  this host; `Plugin::load_file` is the entry it calls.
