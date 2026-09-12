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
Fuel exhaustion is classified by downcasting the Wasmtime error chain to
`wasmtime::Trap::OutOfFuel`; unrelated guest traps remain
`PluginError::Trap`. The infinite-loop test pins that distinction.

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

`Host::new()` installs `DenyAllDispatcher`, so host calls fail closed unless an
embedding injects an `Arc<dyn HostDispatcher>` through `Host::with_dispatcher`.
The dispatcher receives a verified `PluginIdentity`, a closed `HostFunction`, and
the guest argument. It may implement selected routes and return `DispatchError`
for unsupported or policy-refused calls. Tool-class linkers still determine which
routes a component can reach. Dispatch is synchronous, bounded, and audited per
plugin instance.

This integration seam does not establish that the repository's executable
runtime supplies workspace, search, process, snapshot, journal, or user-
interaction services. The following mechanics-only example implements one fixed
demo route and refuses every other function:

```rust
use std::sync::Arc;
use supra_plugin::{DispatchError, Host, HostDispatcher, HostFunction};

struct DemoDispatcher;

impl HostDispatcher for DemoDispatcher {
    fn dispatch(
        &self,
        _caller: &supra_plugin::PluginIdentity,
        function: HostFunction,
        argument: &str,
    ) -> Result<String, DispatchError> {
        match function {
            HostFunction::Search => Ok(format!("demo:{argument}")),
            _ => Err(DispatchError::new("unsupported by this demo embedding")),
        }
    }
}

let host = Host::with_dispatcher(Arc::new(DemoDispatcher))?;
# Ok::<(), supra_plugin::PluginError>(())
```

Host callbacks are synchronous today. An embedding that depends on asynchronous
services needs a deliberate integration strategy compatible with its runtime and
Wasmtime; migrating to Wasmtime's async component API is one option.

## Mutation results

These ten mutations covered the original T20 host/import/fuel implementation;
they do not claim mutation coverage for the later dispatcher injection, denial,
audit, or resource-bound changes. All ten original mutations were caught, and
the control survived by design.

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

## Delivered boundary and remaining integration

- **Delivered in `supra_plugin`:** class-specific import registration, an
  injected host-dispatch seam, fail-closed defaults, bounded arguments/results/
  call counts, ordered per-instance audits, and distinct dispatch errors.
- **Executable integration:** an embedding must provide any desired runtime
  services and enforce workspace, permission, sandbox, journal, and interaction
  policy behind the dispatcher. This crate does not establish a production
  adapter for those services.
- **T21/T22** peers run as components through this host; the cohort's k is
  bounded by the fuel budget as much as by the peer limit.
- **T30** can load plugin components from configuration through
  `Plugin::load_file`; doing so does not by itself provide a dispatcher adapter.
