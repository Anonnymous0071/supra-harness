//! Injectable host-call dispatch and its resource bounds.

use std::fmt;

use supra_types::ToolClass;

/// Default maximum UTF-8 bytes accepted from one plugin host call.
pub const DEFAULT_MAX_ARGUMENT_BYTES: usize = 64 * 1024;
/// Default maximum UTF-8 bytes returned by one plugin host call.
pub const DEFAULT_MAX_RESULT_BYTES: usize = 1024 * 1024;
/// Default maximum host calls, and therefore audit records, per instance.
pub const DEFAULT_MAX_CALLS: usize = 1024;
/// Default maximum bytes in one guest linear memory.
pub const DEFAULT_MAX_MEMORY_BYTES: usize = 64 * 1024 * 1024;
/// Default maximum elements in one guest table.
pub const DEFAULT_MAX_TABLE_ELEMENTS: usize = 10_000;
/// Default maximum core instances created by one component.
pub const DEFAULT_MAX_INSTANCES: usize = 64;
/// Default maximum tables created by one component.
pub const DEFAULT_MAX_TABLES: usize = 16;
/// Default maximum memories created by one component.
pub const DEFAULT_MAX_MEMORIES: usize = 16;

/// A host function from the closed `supra:plugin` WIT world.
///
/// Dispatchers receive this enum rather than caller-controlled path text, so
/// they can route only functions the host registered for the plugin's class.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HostFunction {
    /// `supra:plugin/agent-tools#read-file`.
    ReadFile,
    /// `supra:plugin/agent-tools#search`.
    Search,
    /// `supra:plugin/host-tools#spawn`.
    Spawn,
    /// `supra:plugin/host-tools#snapshot`.
    Snapshot,
    /// `supra:plugin/user-tools#ask-user`.
    AskUser,
}

impl HostFunction {
    /// Resolve one entry from [`crate::world::imports_for`].
    pub(crate) fn from_wit(interface: &str, function: &str) -> Option<Self> {
        match (interface, function) {
            ("supra:plugin/agent-tools", "read-file") => Some(Self::ReadFile),
            ("supra:plugin/agent-tools", "search") => Some(Self::Search),
            ("supra:plugin/host-tools", "spawn") => Some(Self::Spawn),
            ("supra:plugin/host-tools", "snapshot") => Some(Self::Snapshot),
            ("supra:plugin/user-tools", "ask-user") => Some(Self::AskUser),
            _ => None,
        }
    }

    /// The canonical `interface#function` path used for audit and errors.
    #[must_use]
    pub const fn path(self) -> &'static str {
        match self {
            Self::ReadFile => "supra:plugin/agent-tools#read-file",
            Self::Search => "supra:plugin/agent-tools#search",
            Self::Spawn => "supra:plugin/host-tools#spawn",
            Self::Snapshot => "supra:plugin/host-tools#snapshot",
            Self::AskUser => "supra:plugin/user-tools#ask-user",
        }
    }
}

impl fmt::Display for HostFunction {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.path())
    }
}

/// A refusal from the runtime-supplied host dispatcher.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DispatchError {
    detail: String,
}

impl DispatchError {
    /// Construct a dispatcher refusal with operator-safe detail.
    #[must_use]
    pub fn new(detail: impl Into<String>) -> Self {
        Self { detail: detail.into() }
    }

    /// The dispatcher-provided refusal detail.
    #[must_use]
    pub fn detail(&self) -> &str {
        &self.detail
    }
}

impl fmt::Display for DispatchError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.detail)
    }
}

impl std::error::Error for DispatchError {}

/// Stable identity of the plugin issuing a host call.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PluginIdentity {
    /// The component's configured name.
    pub component: String,
    /// The capability class used to construct its linker.
    pub class: ToolClass,
}

/// Runtime behavior behind allowed plugin host imports.
pub trait HostDispatcher: Send + Sync {
    /// Route one already class-checked host function.
    ///
    /// # Errors
    ///
    /// [`DispatchError`] when the backing runtime refuses or cannot perform
    /// the call; the plugin host converts the refusal to a host-dispatch trap.
    fn dispatch(
        &self,
        caller: &PluginIdentity,
        function: HostFunction,
        argument: &str,
    ) -> Result<String, DispatchError>;
}

/// The default dispatcher: every present import traps instead of faking success.
#[derive(Clone, Copy, Debug, Default)]
pub struct DenyAllDispatcher;

impl HostDispatcher for DenyAllDispatcher {
    fn dispatch(
        &self,
        caller: &PluginIdentity,
        function: HostFunction,
        _argument: &str,
    ) -> Result<String, DispatchError> {
        Err(DispatchError::new(format!(
            "no runtime dispatcher configured for {function} from {:?} component {:?}",
            caller.class, caller.component
        )))
    }
}

/// Per-instance bounds for host calls and guest store resources.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct HostLimits {
    /// Maximum UTF-8 bytes in one call argument.
    pub max_argument_bytes: usize,
    /// Maximum UTF-8 bytes returned by one successful call result.
    pub max_result_bytes: usize,
    /// Maximum dispatched calls and retained audit records per instance.
    pub max_calls: usize,
    /// Maximum UTF-8 bytes returned by the top-level `run` export.
    pub max_plugin_result_bytes: usize,
    /// Maximum bytes in any one guest linear memory.
    pub max_memory_bytes: usize,
    /// Maximum elements in any one guest table.
    pub max_table_elements: usize,
    /// Maximum core instances created by the component.
    pub max_instances: usize,
    /// Maximum tables created by the component.
    pub max_tables: usize,
    /// Maximum memories created by the component.
    pub max_memories: usize,
}

impl Default for HostLimits {
    fn default() -> Self {
        Self {
            max_argument_bytes: DEFAULT_MAX_ARGUMENT_BYTES,
            max_result_bytes: DEFAULT_MAX_RESULT_BYTES,
            max_calls: DEFAULT_MAX_CALLS,
            max_plugin_result_bytes: DEFAULT_MAX_RESULT_BYTES,
            max_memory_bytes: DEFAULT_MAX_MEMORY_BYTES,
            max_table_elements: DEFAULT_MAX_TABLE_ELEMENTS,
            max_instances: DEFAULT_MAX_INSTANCES,
            max_tables: DEFAULT_MAX_TABLES,
            max_memories: DEFAULT_MAX_MEMORIES,
        }
    }
}

/// One bounded, ordered host-call audit record.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HostCall {
    /// The plugin that issued the call.
    pub caller: PluginIdentity,
    /// The closed WIT function selected by the linker.
    pub function: HostFunction,
    /// The exact argument forwarded to the dispatcher.
    pub argument: String,
}
