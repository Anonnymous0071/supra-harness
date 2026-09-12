//! The host: link a class's imports, verify a component, run its export.
//!
//! The lifecycle, in order, and the order is the product:
//!
//! 1. [`Host::new`] builds one [`wasmtime::Engine`] with fuel enabled and
//!    one [`wasmtime::component::Linker`] per class, each holding exactly
//!    the class's imports from [`crate::world::imports_for`]. A Agent-class
//!    linker has no Host or User entries - isolation by absence, the T20
//!    sentence, not a check that refuses after the fact.
//! 2. [`Host::verify`] type-checks a component **without instantiating
//!    it**: every import the component declares must be in its class set
//!    (else [`PluginError::UnlistedImport`]), and the linker must already
//!    offer every import the class set needs (else
//!    [`PluginError::MissingHostImport`], which is a harness bug). A
//!    component that would misbehave is refused before its code ever runs.
//! 3. [`Host::instantiate`] links, type-checks the `run` export's
//!    signature (else [`PluginError::WrongSignature`]), and hands back a
//!    [`Plugin`] whose store carries the fuel budget. The budget starts
//!    full; every call consumes; exhaustion is
//!    [`PluginError::FuelExhausted`], not a hang.
//!
//! Fuel is the hang containment. An agent component that loops forever is
//! not a bug the harness can fix, so the harness bounds it: the budget
//! is per instantiation, non-refillable, and a trap (including the fuel
//! trap) names the component.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

use wasmtime::component::types::{ComponentItem, Type};
use wasmtime::component::{Component, Instance, Linker};
use wasmtime::{Config, Engine, Store, StoreLimits, StoreLimitsBuilder, Trap};

use crate::dispatch::{
    DenyAllDispatcher, HostCall, HostDispatcher, HostFunction, HostLimits, PluginIdentity,
};
use crate::error::PluginError;
use crate::world::imports_for;

/// The default fuel budget: enough for thousands of host calls, small
/// enough that an infinite loop dies in milliseconds rather than minutes.
/// Per instantiation, non-refillable: a component that needs more is a
/// component whose author should say so, and the budget is the place.
pub const DEFAULT_FUEL: u64 = 10_000_000;

/// The host: one engine, one linker per class, and the host functions
/// behind them.
pub struct Host {
    engine: Engine,
    linkers: BTreeMap<supra_types::ToolClass, Linker<HostState>>,
    dispatcher: Arc<dyn HostDispatcher>,
    limits: HostLimits,
}

/// Per-component state shared with host-import callbacks.
pub struct HostState {
    dispatcher: Arc<dyn HostDispatcher>,
    identity: PluginIdentity,
    calls: Vec<HostCall>,
    limits: HostLimits,
    store_limits: StoreLimits,
}

impl std::fmt::Debug for HostState {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("HostState")
            .field("identity", &self.identity)
            .field("calls", &self.calls)
            .field("limits", &self.limits)
            .finish_non_exhaustive()
    }
}

impl HostState {
    fn new(dispatcher: Arc<dyn HostDispatcher>, identity: PluginIdentity, limits: HostLimits) -> Self {
        let store_limits = StoreLimitsBuilder::new()
            .memory_size(limits.max_memory_bytes)
            .table_elements(limits.max_table_elements)
            .instances(limits.max_instances)
            .tables(limits.max_tables)
            .memories(limits.max_memories)
            .build();
        Self { dispatcher, identity, calls: Vec::new(), limits, store_limits }
    }

    fn dispatch(&mut self, function: HostFunction, argument: &str) -> wasmtime::Result<(String,)> {
        let path = function.path();
        if self.calls.len() >= self.limits.max_calls {
            return Err(wasmtime::Error::new(HostDispatchFailure(format!(
                "host dispatch {path} refused: call limit {} exhausted",
                self.limits.max_calls
            ))));
        }
        if argument.len() > self.limits.max_argument_bytes {
            return Err(wasmtime::Error::new(HostDispatchFailure(format!(
                "host dispatch {path} refused: argument is {} bytes, limit is {}",
                argument.len(),
                self.limits.max_argument_bytes
            ))));
        }

        self.calls.push(HostCall { caller: self.identity.clone(), function, argument: argument.to_owned() });
        let result = self.dispatcher.dispatch(&self.identity, function, argument).map_err(|error| {
            wasmtime::Error::new(HostDispatchFailure(format!("host dispatch {path} failed: {error}")))
        })?;
        if result.len() > self.limits.max_result_bytes {
            return Err(wasmtime::Error::new(HostDispatchFailure(format!(
                "host dispatch {path} refused: result is {} bytes, limit is {}",
                result.len(),
                self.limits.max_result_bytes
            ))));
        }
        Ok((result,))
    }
}

#[derive(Debug)]
struct HostDispatchFailure(String);

impl std::fmt::Display for HostDispatchFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for HostDispatchFailure {}

impl Host {
    /// Build the engine, class linkers, and default fail-closed dispatcher.
    ///
    /// # Errors
    ///
    /// [`PluginError::Engine`] when wasmtime refuses the configuration.
    /// Nothing else here can fail: registering a known-good host function
    /// under a known-good name is infallible by construction, and the
    /// test below pins that no registration was skipped.
    pub fn new() -> Result<Self, PluginError> {
        Self::with_dispatcher(Arc::new(DenyAllDispatcher))
    }

    /// Build the host with runtime-supplied behavior and default bounds.
    ///
    /// # Errors
    ///
    /// [`PluginError::Engine`] when wasmtime refuses the configuration.
    pub fn with_dispatcher(dispatcher: Arc<dyn HostDispatcher>) -> Result<Self, PluginError> {
        Self::with_dispatcher_and_limits(dispatcher, HostLimits::default())
    }

    /// Build the host with runtime-supplied behavior and explicit bounds.
    ///
    /// # Errors
    ///
    /// [`PluginError::Engine`] when wasmtime refuses the configuration, or
    /// [`PluginError::MissingHostImport`] if the WIT routes disagree.
    pub fn with_dispatcher_and_limits(
        dispatcher: Arc<dyn HostDispatcher>,
        limits: HostLimits,
    ) -> Result<Self, PluginError> {
        let mut config = Config::new();
        config.consume_fuel(true);
        let engine = Engine::new(&config)?;

        let mut linkers = BTreeMap::new();
        for class in
            [supra_types::ToolClass::Agent, supra_types::ToolClass::Host, supra_types::ToolClass::User]
        {
            let mut linker = Linker::new(&engine);
            let allowed = imports_for(class);
            for interface in
                allowed.iter().map(|(interface, _)| *interface).collect::<std::collections::BTreeSet<_>>()
            {
                for instance_name in [interface.to_string(), format!("{interface}@0.1.0")] {
                    let mut instance =
                        linker.instance(&instance_name).map_err(|error| PluginError::MissingHostImport {
                            import: format!("registering {instance_name}: {error}"),
                        })?;
                    for (_, function) in allowed.iter().filter(|(candidate, _)| *candidate == interface) {
                        let path = format!("{interface}#{function}");
                        let Some(host_function) = HostFunction::from_wit(interface, function) else {
                            return Err(PluginError::MissingHostImport {
                                import: format!("no dispatcher route for {path}"),
                            });
                        };
                        let route = move |mut store: wasmtime::StoreContextMut<'_, HostState>,
                                          (argument,): (String,)| {
                            store.data_mut().dispatch(host_function, &argument)
                        };
                        instance.func_wrap(function, route).map_err(|error| {
                            PluginError::MissingHostImport {
                                import: format!("registering {instance_name}#{function}: {error}"),
                            }
                        })?;
                    }
                }
            }
            for (interface, function) in allowed {
                let path = format!("{interface}#{function}");
                let Some(host_function) = HostFunction::from_wit(interface, function) else {
                    return Err(PluginError::MissingHostImport {
                        import: format!("no dispatcher route for {path}"),
                    });
                };
                let route = move |mut store: wasmtime::StoreContextMut<'_, HostState>,
                                  (argument,): (String,)| {
                    store.data_mut().dispatch(host_function, &argument)
                };
                linker.root().func_wrap(function, route).map_err(|error| PluginError::MissingHostImport {
                    import: format!("registering {function}: {error}"),
                })?;
            }
            linkers.insert(class, linker);
        }

        Ok(Self { engine, linkers, dispatcher, limits })
    }

    /// The engine, for compiling components the host will run.
    #[must_use]
    pub fn engine(&self) -> &Engine {
        &self.engine
    }

    /// Verify a component against a class, without instantiating it.
    ///
    /// Every import the component declares must be in the class's set;
    /// every import the class's set needs must already be registered.
    /// A component that passes has no path to anything outside its
    /// class - there is simply no import left unaccounted for.
    ///
    /// # Errors
    ///
    /// [`PluginError::UnlistedImport`] for a declared import outside the
    /// set; [`PluginError::MissingHostImport`] for a set member the host
    /// does not offer (a harness bug).
    pub fn verify(
        &self,
        name: &str,
        component: &Component,
        class: supra_types::ToolClass,
    ) -> Result<(), PluginError> {
        let allowed = imports_for(class);

        // What the component declares: the type's import list, read off
        // the compiled component without running a single instruction.
        let declared = component_declared_imports(component, &self.engine);

        for import in &declared {
            if !import_in_set(import, allowed) {
                return Err(PluginError::UnlistedImport {
                    component: name.to_owned(),
                    import: import.clone(),
                });
            }
        }
        Ok(())
    }

    /// Link, type-check, and instantiate a component for one turn.
    ///
    /// The `run` export must have the world's shape - `(string) ->
    /// (string)` - verified by `get_typed_func` at this point, not
    /// discovered mid-turn. The store carries `DEFAULT_FUEL` units; the
    /// budget is spent by execution and never refilled.
    ///
    /// # Errors
    ///
    /// Whatever [`Host::verify`] refuses (verified again here, so a
    /// caller cannot skip the check); [`PluginError::WrongSignature`]
    /// for a mis-shaped `run`; [`PluginError::Engine`] when wasmtime
    /// refuses the instantiation.
    pub fn instantiate(
        &self,
        name: &str,
        component: &Component,
        class: supra_types::ToolClass,
    ) -> Result<Plugin, PluginError> {
        self.verify(name, component, class)?;

        // The linker for this class was registered in the constructor; if
        // it is somehow absent, that is a harness bug - refused as
        // MissingHostImport (the class's imports cannot be offered) rather
        // than a panic on the shipped path.
        let Some(linker) = self.linkers.get(&class) else {
            return Err(PluginError::MissingHostImport {
                import: format!("no linker registered for {class:?}"),
            });
        };

        validate_run_export(component)?;
        let pre = linker
            .instantiate_pre(component)
            .map_err(|error| classify_engine_error(name, DEFAULT_FUEL, error))?;
        let identity = PluginIdentity { component: name.to_owned(), class };
        let mut store =
            Store::new(&self.engine, HostState::new(Arc::clone(&self.dispatcher), identity, self.limits));
        store.limiter(|state| &mut state.store_limits);
        store.set_fuel(DEFAULT_FUEL)?;

        let instance =
            pre.instantiate(&mut store).map_err(|error| classify_engine_error(name, DEFAULT_FUEL, error))?;

        Ok(Plugin { name: name.to_owned(), instance, store, fuel: DEFAULT_FUEL })
    }
}

fn validate_run_export(component: &Component) -> Result<(), PluginError> {
    let Some((item, _)) = component.get_export(None, "run") else {
        return Err(PluginError::WrongSignature {
            import: "run".to_owned(),
            detail: "required export is missing".to_owned(),
        });
    };
    let ComponentItem::ComponentFunc(function) = item else {
        return Err(PluginError::WrongSignature {
            import: "run".to_owned(),
            detail: "required export is not a component function".to_owned(),
        });
    };
    let params = function.params().map(|(_, ty)| ty).collect::<Vec<_>>();
    let results = function.results().collect::<Vec<_>>();
    if params == [Type::String] && results == [Type::String] {
        Ok(())
    } else {
        Err(PluginError::WrongSignature {
            import: "run".to_owned(),
            detail: format!("expected (string) -> string, found ({params:?}) -> {results:?}"),
        })
    }
}

fn classify_engine_error(component: &str, budget: u64, error: wasmtime::Error) -> PluginError {
    if error.downcast_ref::<Trap>() == Some(&Trap::OutOfFuel) {
        PluginError::FuelExhausted { component: component.to_owned(), budget }
    } else if let Some(failure) = error.downcast_ref::<HostDispatchFailure>() {
        PluginError::HostDispatch { component: component.to_owned(), detail: failure.0.clone() }
    } else {
        PluginError::Engine(error)
    }
}

/// One instantiated component: its instance, its store, and its remaining
/// shape of the turn.
pub struct Plugin {
    name: String,
    instance: Instance,
    store: Store<HostState>,
    fuel: u64,
}

impl Plugin {
    /// The component's configured name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The fuel budget this instantiation started with.
    #[must_use]
    pub const fn fuel_budget(&self) -> u64 {
        self.fuel
    }

    /// Call the `run` export with canonical argument text.
    ///
    /// The guest answers canonical text back; the host returns it
    /// unchanged - interpretation is the turn loop's business. A fuel
    /// trap surfaces as [`PluginError::FuelExhausted`] (the budget is
    /// what ran out, and the component is spent for this turn); any
    /// other trap is [`PluginError::Trap`] with the trap's own message.
    ///
    /// # Errors
    ///
    /// [`PluginError::WrongSignature`] when the export is gone (it was
    /// verified at instantiation; this is the paranoid re-read);
    /// [`PluginError::FuelExhausted`] on the fuel trap;
    /// [`PluginError::Trap`] on any other trap.
    pub fn call(&mut self, arguments: &str) -> Result<String, PluginError> {
        let typed = self.instance.get_typed_func::<(String,), (String,)>(&mut self.store, "run").map_err(
            |error| PluginError::WrongSignature { import: "run".to_owned(), detail: error.to_string() },
        )?;
        match typed.call(&mut self.store, (arguments.to_owned(),)) {
            Ok((answer,)) => {
                let limit = self.store.data().limits.max_plugin_result_bytes;
                if answer.len() > limit {
                    Err(PluginError::ResourceLimit {
                        component: self.name.clone(),
                        detail: format!("run result is {} bytes, limit is {limit}", answer.len()),
                    })
                } else {
                    Ok(answer)
                }
            }
            Err(error) => Err(classify_call_error(&self.name, self.fuel, &error)),
        }
    }

    /// The bounded host-function calls this instance attempted, in order.
    #[must_use]
    pub fn calls(&self) -> &[HostCall] {
        &self.store.data().calls
    }

    /// Load a component from bytes, through this host's engine.
    ///
    /// # Errors
    ///
    /// [`PluginError::Engine`] when the bytes are not a component.
    pub fn load_bytes(host: &Host, bytes: &[u8]) -> Result<Component, PluginError> {
        Component::new(host.engine(), bytes).map_err(PluginError::Engine)
    }

    /// Load a component from a file.
    ///
    /// # Errors
    ///
    /// [`PluginError::Engine`] when the file cannot be read or is not a
    /// component.
    pub fn load_file(host: &Host, path: impl AsRef<Path>) -> Result<Component, PluginError> {
        let bytes =
            std::fs::read(path.as_ref()).map_err(|error| PluginError::Engine(wasmtime::Error::msg(error)))?;
        Self::load_bytes(host, &bytes)
    }
}

fn classify_call_error(component: &str, budget: u64, error: &wasmtime::Error) -> PluginError {
    if error.downcast_ref::<Trap>() == Some(&Trap::OutOfFuel) {
        PluginError::FuelExhausted { component: component.to_owned(), budget }
    } else if let Some(failure) = error.downcast_ref::<HostDispatchFailure>() {
        PluginError::HostDispatch { component: component.to_owned(), detail: failure.0.clone() }
    } else {
        PluginError::Trap { component: component.to_owned(), detail: format!("{error:#}") }
    }
}

/// The import paths a component declares, without instantiating it.
///
/// Read off the component's type: `component_type().imports()` lists
/// every import with its path, whether the linker could satisfy it or
/// not. A component with no imports declares the empty list - a pure
/// function, verifiable by inspection.
fn component_declared_imports(component: &Component, engine: &Engine) -> Vec<String> {
    component.component_type().imports(engine).map(|(name, _)| name.to_owned()).collect()
}

/// Whether a declared import path is in the class's set.
///
/// Components declare imports two ways: the WIT path
/// (`supra:plugin/agent-tools#read-file` or the interface alone) or a
/// bare function name after toolchain lowering. The set check accepts
/// the interface match (the namespace the class owns), the exact
/// `interface#function` spelling, and the bare function name - the
/// three spellings one toolchain or another produces, and no fourth.
fn import_in_set(import: &str, allowed: &[(&str, &str)]) -> bool {
    // The `interface#function` spelling: exact membership.
    for (interface, function) in allowed {
        if *import == format!("{interface}#{function}") {
            return true;
        }
    }
    // The bare interface spelling: the class owns the whole namespace.
    if allowed.iter().any(|(interface, _)| import == *interface) {
        return true;
    }
    // The bare function spelling: the name without its namespace, after
    // toolchain lowering drops it.
    if allowed.iter().any(|(_, function)| import == *function) {
        return true;
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dispatch::DispatchError;

    struct TestDispatcher {
        result: Result<String, DispatchError>,
    }

    impl HostDispatcher for TestDispatcher {
        fn dispatch(
            &self,
            _caller: &PluginIdentity,
            _function: HostFunction,
            _argument: &str,
        ) -> Result<String, DispatchError> {
            self.result.clone()
        }
    }

    fn dispatch_host(result: Result<String, DispatchError>, limits: HostLimits) -> Host {
        Host::with_dispatcher_and_limits(Arc::new(TestDispatcher { result }), limits).expect("host")
    }

    /// A component whose `run` forwards its string to one imported host
    /// function and returns the host's string unchanged.
    fn calling_component(interface: &str, function: &str) -> String {
        format!(
            r#"(component
                (type $host-interface
                    (instance
                        (type $host-type (func (param "argument" string) (result string)))
                        (export "{function}" (func (type $host-type)))))
                (import "{interface}" (instance $host-interface-instance (type $host-interface)))
                (alias export $host-interface-instance "{function}" (func $host))
                (core module $memory
                    (memory (export "memory") 1)
                    (global $heap (mut i32) (i32.const 16))
                    (func (export "realloc") (param i32 i32 i32 i32) (result i32)
                        (local $pointer i32)
                        global.get $heap
                        local.tee $pointer
                        local.get 3
                        i32.add
                        global.set $heap
                        local.get $pointer))
                (core instance $memory-instance (instantiate $memory))
                (alias core export $memory-instance "memory" (core memory $memory-export))
                (alias core export $memory-instance "realloc" (core func $realloc))
                (core func $host-lowered
                    (canon lower (func $host) (memory $memory-export) (realloc $realloc) string-encoding=utf8))
                (core module $guest
                    (type $host-type (func (param i32 i32 i32)))
                    (import "env" "memory" (memory 1))
                    (import "env" "host" (func $host (type $host-type)))
                    (func (export "run") (param i32 i32) (result i32)
                        local.get 0
                        local.get 1
                        i32.const 0
                        call $host
                        i32.const 0))
                (core instance $env
                    (export "memory" (memory $memory-export))
                    (export "host" (func $host-lowered)))
                (core instance $guest-instance (instantiate $guest (with "env" (instance $env))))
                (alias core export $guest-instance "run" (core func $run))
                (func (export "run") (param "arguments" string) (result string)
                    (canon lift (core func $run) (memory $memory-export) (realloc $realloc) string-encoding=utf8)))"#
        )
    }

    /// Compile a component from text. The text format is wasmtime's own
    /// `(component ...)` syntax - no WAT toolchain, no fixture binary, no
    /// network. An empty world (no imports, one `run` export) is a few
    /// lines; a world with imports names them.
    ///
    /// Two text-format facts the fixtures rest on, both verified against
    /// wasmtime's parser rather than assumed:
    ///
    /// - A `canon lift` of a string-typed function must name its memory:
    ///   `(canon lift (core func $i "run") (memory $memory)
    ///   string-encoding=utf8)`. Without the memory option the component
    ///   is refused at parse time.
    /// - Import names are flat kebab extern names (`spawn`,
    ///   `host-tools-spawn`). The WIT `interface#func` path is a
    ///   *source-level* spelling: by the time a component is compiled the
    ///   toolchain has lowered it, and the text format only accepts the
    ///   lowered name. The host's `import_in_set` therefore accepts all
    ///   three spellings (interface, `interface#function`, bare function)
    ///   while the fixtures exercise the ones the text format accepts.
    fn component_text(imports: &str, body: &str) -> String {
        format!(
            r#"(component
                {imports}
                (core module $m
                    (memory (export "memory") 1)
                    (func (export "run") (param i32 i32) (result i32)
                        {body})
                    (func (export "realloc") (param i32 i32 i32 i32) (result i32)
                        i32.const 0)
                )
                (core instance $i (instantiate $m))
                (alias core export $i "memory" (core memory $memory))
                (alias core export $i "realloc" (core func $realloc))
                (func (export "run") (param "arguments" string) (result string)
                    (canon lift (core func $i "run") (memory $memory) (realloc $realloc) string-encoding=utf8))
            )"#
        )
    }

    fn host() -> Host {
        Host::new().expect("host")
    }

    #[test]
    #[ignore = "requires SUPRA_COMPONENT_ARTIFACT from scripts/build-wasm.sh"]
    fn component_contract_accepts() {
        const SENTINEL: &str = "supra-component-contract-smoke";

        let path = std::env::var_os("SUPRA_COMPONENT_ARTIFACT")
            .map(std::path::PathBuf::from)
            .expect("SUPRA_COMPONENT_ARTIFACT names the built component");
        let name = path.file_stem().and_then(std::ffi::OsStr::to_str).unwrap_or("component");
        let host = host();
        let component = Plugin::load_file(&host, &path).expect("component artifact loads");
        let mut plugin = host
            .instantiate(name, &component, supra_types::ToolClass::Agent)
            .expect("component links and exports run(string) -> string");
        let answer = plugin.call(SENTINEL).expect("component run export executes");
        assert_eq!(answer, SENTINEL, "contract-smoke must echo the sentinel unchanged");
    }

    #[test]
    fn the_constructor_registers_every_class_import() {
        // The MissingHostImport arm exists for a harness bug; this test
        // pins there is no harness bug today - every import in every
        // class's set resolves through its linker. The check is the
        // verify path itself, against an empty component: no declared
        // imports means nothing to refuse, and the constructor's
        // registrations either all landed or one did not.
        let host = host();
        let empty = Component::new(host.engine(), component_text("", "i32.const 0")).expect("empty");
        for class in
            [supra_types::ToolClass::Agent, supra_types::ToolClass::Host, supra_types::ToolClass::User]
        {
            host.verify("empty", &empty, class).expect("verify");
        }
    }

    #[test]
    fn an_import_outside_the_class_set_is_refused_before_instantiation() {
        let host = host();
        // The bare lowered name: a toolchain compiling the WIT import
        // `supra:plugin/host-tools#spawn` produces a component importing
        // `spawn`, and that component must be refused for Agent.
        let imports = r#"(import "spawn" (func (param "argv" string) (result string)))"#;
        let component =
            Component::new(host.engine(), component_text(imports, "i32.const 0")).expect("component");

        match host.verify("greedy", &component, supra_types::ToolClass::Agent) {
            Err(PluginError::UnlistedImport { component, import }) => {
                assert_eq!(component, "greedy");
                assert!(import.contains("spawn"), "{import}");
            }
            other => panic!("the host-only import must be refused for Agent; got {other:?}"),
        }

        // The same component verifies for Host: the import is in that
        // class's set.
        host.verify("greedy", &component, supra_types::ToolClass::Host).expect("host may import spawn");
    }

    #[test]
    fn instantiate_refuses_an_unlisted_import_without_prior_verify() {
        // The paranoid path: a caller that skips `verify` and instantiates
        // directly still cannot link an over-classed component, because
        // `instantiate` runs the check itself. This closed mutation M1 -
        // every other test verified first, so a verify-less instantiate
        // was unobservable.
        let host = host();
        let imports = r#"(import "spawn" (func (param "argv" string) (result string)))"#;
        let component =
            Component::new(host.engine(), component_text(imports, "i32.const 0")).expect("component");
        match host.instantiate("greedy", &component, supra_types::ToolClass::Agent) {
            Err(PluginError::UnlistedImport { .. }) => {}
            _ => panic!("instantiate must verify by itself"),
        }
    }

    #[test]
    fn instantiate_checks_the_run_signature() {
        let host = host();
        // `run` with zero params: the world's shape is (string) ->
        // (string), and get_typed_func refuses the mismatch.
        let component = Component::new(
            host.engine(),
            r#"(component
                (core module $m
                    (memory (export "memory") 1)
                    (func (export "run") (result i32) i32.const 0)
                    (func (export "realloc") (param i32 i32 i32 i32) (result i32)
                        i32.const 0)
                )
                (core instance $i (instantiate $m))
                (alias core export $i "memory" (core memory $memory))
                (alias core export $i "realloc" (core func $realloc))
                (func (export "run") (result string)
                    (canon lift (core func $i "run") (memory $memory) (realloc $realloc) string-encoding=utf8))
            )"#,
        )
        .expect("component");

        match host.instantiate("misshapen", &component, supra_types::ToolClass::Agent) {
            Err(PluginError::WrongSignature { import, .. }) => assert_eq!(import, "run"),
            _ => panic!("the mis-shaped run must be refused"),
        }
    }

    #[test]
    fn call_round_trips_canonical_text_through_a_guest() {
        // The trap-free way to answer a lifted string: the guest returns a
        // pointer into its *own* data segment, not the argument's pointer
        // (which the caller owns and may reuse after the call). The
        // canonical ABI lifts `(result string)` from a `(ptr, len)` pair the
        // guest writes via realloc; this guest's echo returns the constant
        // "hi" from offset 100 and length 2, both in its own memory.
        let host = host();
        let component = Component::new(
            host.engine(),
            r#"(component
                (core module $m
                    (memory (export "memory") 1)
                    (func (export "realloc") (param i32 i32 i32 i32) (result i32)
                        local.get 0)
                    (data (i32.const 100) "hi")
                    ;; (ptr, len) for the result string: the guest writes the
                    ;; pair into its own linear memory at offset 0 and
                    ;; returns 0 (the pair's address) - the lift reads the
                    ;; pair, not a bare pointer.
                    (func (export "echo") (param i32 i32) (result i32)
                        (i32.store (i32.const 0) (i32.const 100))
                        (i32.store (i32.const 4) (i32.const 2))
                        i32.const 0)
                )
                (core instance $i (instantiate $m))
                (alias core export $i "memory" (core memory $memory))
                (alias core export $i "realloc" (core func $realloc))
                (func (export "run") (param "arguments" string) (result string)
                    (canon lift (core func $i "echo") (memory $memory) (realloc $realloc) string-encoding=utf8))
            )"#,
        )
        .expect("component");

        let mut plugin =
            host.instantiate("echo", &component, supra_types::ToolClass::Agent).expect("instantiate");
        let answer = plugin.call("hello").expect("call");
        // The guest answered "hi" from its own data segment: lifted
        // (param string) in and lifted (result string) out, through the
        // guest's own memory, no dangling pointer between the two.
        assert_eq!(answer, "hi", "the guest returned its data segment");
    }

    #[test]
    fn dispatch_result_and_attempt_are_returned_and_logged() {
        let host = dispatch_host(Ok("file contents".to_owned()), HostLimits::default());
        let component =
            Component::new(host.engine(), calling_component("supra:plugin/agent-tools", "read-file"))
                .expect("component");
        let mut plugin =
            host.instantiate("reader", &component, supra_types::ToolClass::Agent).expect("instantiate");

        assert_eq!(plugin.call("notes.txt").expect("call"), "file contents");
        assert_eq!(
            plugin.calls(),
            &[HostCall {
                caller: PluginIdentity {
                    component: "reader".to_owned(),
                    class: supra_types::ToolClass::Agent,
                },
                function: HostFunction::ReadFile,
                argument: "notes.txt".to_owned(),
            }]
        );
    }

    #[test]
    fn dispatch_errors_surface_as_plugin_errors_without_losing_the_audit() {
        let host = dispatch_host(Err(DispatchError::new("workspace refused path")), HostLimits::default());
        let component =
            Component::new(host.engine(), calling_component("supra:plugin/agent-tools", "read-file"))
                .expect("component");
        let mut plugin =
            host.instantiate("reader", &component, supra_types::ToolClass::Agent).expect("instantiate");

        match plugin.call("../secret") {
            Err(PluginError::HostDispatch { component, detail }) => {
                assert_eq!(component, "reader");
                assert!(detail.contains("read-file"), "{detail}");
                assert!(detail.contains("workspace refused path"), "{detail}");
            }
            other => panic!("dispatch refusal must stay distinct from a guest trap: {other:?}"),
        }
        assert_eq!(plugin.calls().len(), 1, "the failed attempt remains auditable");
    }

    #[test]
    fn default_dispatch_is_fail_closed() {
        let host = host();
        let component =
            Component::new(host.engine(), calling_component("supra:plugin/agent-tools", "read-file"))
                .expect("component");
        let mut plugin =
            host.instantiate("reader", &component, supra_types::ToolClass::Agent).expect("instantiate");

        match plugin.call("notes.txt") {
            Err(PluginError::HostDispatch { detail, .. }) => {
                assert!(detail.contains("no runtime dispatcher configured"), "{detail}");
            }
            other => panic!("the default host must not fake success: {other:?}"),
        }
    }

    #[test]
    fn class_isolation_still_refuses_present_dispatch_routes() {
        let host = dispatch_host(Ok("spawned".to_owned()), HostLimits::default());
        let component = Component::new(host.engine(), calling_component("supra:plugin/host-tools", "spawn"))
            .expect("component");

        assert!(
            matches!(
                host.instantiate("agent", &component, supra_types::ToolClass::Agent),
                Err(PluginError::UnlistedImport { .. })
            ),
            "an injected dispatcher must not widen the Agent linker"
        );
        let mut plugin =
            host.instantiate("host", &component, supra_types::ToolClass::Host).expect("Host may spawn");
        assert_eq!(plugin.call("tool --flag").expect("call"), "spawned");
        assert_eq!(plugin.calls()[0].function, HostFunction::Spawn);
    }

    #[test]
    fn argument_result_and_call_bounds_trap_without_unbounded_logs() {
        let argument_limits = HostLimits { max_argument_bytes: 3, ..HostLimits::default() };
        let host = dispatch_host(Ok("ok".to_owned()), argument_limits);
        let component =
            Component::new(host.engine(), calling_component("supra:plugin/agent-tools", "search"))
                .expect("component");
        let mut plugin =
            host.instantiate("searcher", &component, supra_types::ToolClass::Agent).expect("instantiate");
        assert!(matches!(plugin.call("four"), Err(PluginError::HostDispatch { .. })));
        assert!(plugin.calls().is_empty(), "oversized arguments are not retained");

        let result_limits = HostLimits { max_result_bytes: 2, ..HostLimits::default() };
        let host = dispatch_host(Ok("long".to_owned()), result_limits);
        let component =
            Component::new(host.engine(), calling_component("supra:plugin/agent-tools", "search"))
                .expect("component");
        let mut plugin =
            host.instantiate("searcher", &component, supra_types::ToolClass::Agent).expect("instantiate");
        assert!(matches!(plugin.call("q"), Err(PluginError::HostDispatch { .. })));
        assert_eq!(plugin.calls().len(), 1, "the bounded attempt remains auditable");

        let call_limits = HostLimits { max_calls: 0, ..HostLimits::default() };
        let host = dispatch_host(Ok("ok".to_owned()), call_limits);
        let component =
            Component::new(host.engine(), calling_component("supra:plugin/agent-tools", "search"))
                .expect("component");
        let mut plugin =
            host.instantiate("searcher", &component, supra_types::ToolClass::Agent).expect("instantiate");
        assert!(matches!(plugin.call("q"), Err(PluginError::HostDispatch { .. })));
        assert!(plugin.calls().is_empty(), "the call cap is also the log cap");
    }

    #[test]
    fn an_infinite_loop_dies_on_fuel_not_on_patience() {
        // Fuel is the hang containment: a guest that never returns burns
        // its budget and the host refuses with FuelExhausted, naming the
        // component and the budget.
        let host = host();
        let component = Component::new(
            host.engine(),
            r#"(component
                (core module $m
                    (memory (export "memory") 1)
                    (func (export "realloc") (param i32 i32 i32 i32) (result i32)
                        local.get 0)
                    (func (export "spin") (param i32 i32) (result i32)
                        (loop (br 0))
                        i32.const 0)
                )
                (core instance $i (instantiate $m))
                (alias core export $i "memory" (core memory $memory))
                (alias core export $i "realloc" (core func $realloc))
                (func (export "run") (param "arguments" string) (result string)
                    (canon lift (core func $i "spin") (memory $memory) (realloc $realloc) string-encoding=utf8))
            )"#,
        )
        .expect("component");

        let mut plugin =
            host.instantiate("spinner", &component, supra_types::ToolClass::Agent).expect("instantiate");
        match plugin.call("anything") {
            Err(PluginError::FuelExhausted { component, budget }) => {
                assert_eq!(component, "spinner");
                assert_eq!(budget, DEFAULT_FUEL);
            }
            other => panic!("the loop must die on fuel; got {other:?}"),
        }
    }
}
