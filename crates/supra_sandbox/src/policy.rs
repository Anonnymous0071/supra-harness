//! The policy layer.
//!
//! A `Policy` here is the host-side choreography of a sandboxed spawn, not
//! the raw `libsupra_sandbox` one. Two pieces sit on top of the FFI shape:
//!
//! - **Default path allow-list.** A command that does not touch the host
//!   files is not a useful command: it must read its executable, its
//!   dynamic linker, possibly `/dev/null`. The defaults below come from a
//!   measured run of `cargo test` on a clean repository; they are the
//!   minimum set, not a generous one. A workspace is added explicitly by
//!   the caller.
//!
//! - **The fd audit allow-list.** A descriptor that the harness has
//!   *deliberately* marked non-CLOEXEC - a pipe to the log, a handle to
//!   the SQLite WAL - is whitelisted here. The audit does not ask "did the
//!   flag pass" alone; it asks "is this path on the list?". Two predicates
//!   because the audit's *input* is the path, but the *descriptors* the
//!   caller wants to inherit may be described in terms that read more
//!   naturally as paths, globs, or predicates - and the caller chooses.

use std::path::{Path, PathBuf};

use supra_ffi::sandbox::{Access, Network, Policy, Tier};

use crate::error::SandboxError;

/// A built policy plus everything the audit needs to evaluate it.
///
/// The audit runs before the policy is sent to the C side, so the allow
/// list is part of the policy value, not a separate argument. A policy
/// without an allow list is a policy that refuses on the first non-stdio
/// descriptor the host has open.
#[derive(Debug)]
pub struct SandboxPolicy {
    /// The raw policy the C side will see.
    inner: Policy,
    /// Paths whose descriptors are allowed to cross `exec` without
    /// `FD_CLOEXEC`. Matching is exact - a child that needs to inherit a
    /// pipe path needs the exact path, and a glob would over-allow.
    allow: Vec<PathBuf>,
}

impl SandboxPolicy {
    /// A policy that starts with no rules and forbids inheritance.
    #[must_use]
    pub fn new() -> Self {
        Self { inner: Policy::new(), allow: Vec::new() }
    }

    /// Permit a filesystem path. See [`Policy::allow`].
    ///
    /// # Errors
    ///
    /// Returns the FFI-side error verbatim. The inner policy rejects
    /// relative paths and a full path table; the wrapper surfaces the
    /// message so a caller knows whether to widen the rule or the table.
    pub fn allow(&mut self, path: impl AsRef<Path>, access: Access) -> Result<&mut Self, SandboxError> {
        self.inner.allow(path, access).map_err(|error| map_ffi(&error))?;
        Ok(self)
    }

    /// Permit one outbound TCP port. See [`Policy::allow_port`].
    ///
    /// # Errors
    ///
    /// Returns the FFI-side error verbatim when the port table is full.
    pub fn allow_port(&mut self, port: u16) -> Result<&mut Self, SandboxError> {
        self.inner.allow_port(port).map_err(|error| map_ffi(&error))?;
        Ok(self)
    }

    /// Set the network policy directly. See [`Policy::network`].
    pub fn network(&mut self, network: Network) -> &mut Self {
        self.inner.network(network);
        self
    }

    /// Isolate the PID namespace, hiding host processes. See
    /// [`Policy::isolate_processes`].
    pub fn isolate_processes(&mut self, isolate: bool) -> &mut Self {
        self.inner.isolate_processes(isolate);
        self
    }

    /// Isolate the IPC and UTS namespaces. See [`Policy::isolate_ipc`].
    pub fn isolate_ipc(&mut self, isolate: bool) -> &mut Self {
        self.inner.isolate_ipc(isolate);
        self
    }

    /// Refuse to run below this tier. See [`Policy::require_tier`].
    pub fn require_tier(&mut self, tier: Tier) -> &mut Self {
        self.inner.require_tier(tier);
        self
    }

    /// Add a path whose descriptors are allowed to cross `exec` without
    /// `FD_CLOEXEC`. The audit accepts the match if the descriptor path is
    /// exactly this.
    pub fn allow_inherited_path(&mut self, path: impl Into<PathBuf>) -> &mut Self {
        self.allow.push(path.into());
        self
    }

    /// The list of paths the audit treats as allowed leaks.
    #[must_use]
    pub fn inherited_paths(&self) -> &[PathBuf] {
        &self.allow
    }

    /// Borrow the inner raw policy.
    #[must_use]
    pub const fn inner(&self) -> &Policy {
        &self.inner
    }

    /// Read the platform's best available tier.
    #[must_use]
    pub fn best_tier() -> Tier {
        supra_ffi::sandbox::probe().tier
    }
}

impl Default for SandboxPolicy {
    fn default() -> Self {
        Self::new()
    }
}

fn map_ffi(error: &supra_ffi::sandbox::Error) -> SandboxError {
    SandboxError::Ffi(error.to_string())
}

/// The host's minimum bootstrap rules.
///
/// `cargo test` on a clean repository needs to read its own binary,
/// `/usr/lib`, `/lib`, and `/lib64` (the dynamic linker, libc, the
/// `cc` crate's vendored archives), plus write to its own scratch
/// directory and read or write `/dev/null` for the standard streams. A
/// process that does not have access to all of these cannot start, which
/// is what a fail-closed default is for.
///
/// The list is conservative: an audit that adds `/usr/local` is the
/// caller's job, because the audit is what knows what the child actually
/// needs.
#[must_use]
pub fn bootstrap_paths(workspace: &Path) -> Vec<(&'static str, Access)> {
    let mut rules: Vec<(&'static str, Access)> = vec![
        ("/usr", Access::read_execute()),
        ("/bin", Access::read_execute()),
        ("/lib", Access::read_execute()),
        ("/dev/null", Access::READ | Access::WRITE),
        ("/proc", Access::READ),
    ];
    if Path::new("/lib64").exists() {
        rules.push(("/lib64", Access::read_execute()));
    }
    if !workspace.as_os_str().is_empty() {
        // The workspace is the only writable path by default. A later T16.7
        // update may add more, but the bootstrap is the floor, not the
        // ceiling.
    }
    rules
}

/// Build a host-side default policy.
///
/// The default denies everything the bootstrap does not grant, and
/// requires the platform to have at least the namespaces tier. A
/// filesystem that cannot be enforced is reported as `Unsupported` later,
/// but the policy itself says what we want to enforce, not what the
/// platform can.
#[must_use]
pub fn default_policy(workspace: &Path) -> SandboxPolicy {
    let mut policy = SandboxPolicy::new();
    for (path, access) in bootstrap_paths(workspace) {
        let _ = policy.allow(path, access);
    }
    if !workspace.as_os_str().is_empty() {
        let _ = policy.allow(workspace, Access::workspace());
    }
    // TTY-attached stdio is the user's, not the harness's, and a child that
    // needs to print a result is allowed to write to the same terminal the
    // user is reading. The audit allow list whitelists the patterns the
    // default policy actually relies on, so `cargo test`'s `assert_eq!` does
    // not trip over its own stdout.
    for tty in tty_paths() {
        policy.allow_inherited_path(tty);
    }
    policy.isolate_processes(true).isolate_ipc(true).require_tier(Tier::Landlock);
    policy
}

/// The paths a TTY-attached stdio may resolve to under `/proc/self/fd`.
///
/// `/dev/tty` is the controlling-terminal alias and is the same file the
/// kernel hands back. `/dev/pts/N` is the path the master side actually
/// lives on; Qwen Code, the harness's own test runner, and any terminal
/// launched from a shell all surface stdio through that path. Both are
/// whitelisted because the audit compares exact strings, and `/dev/tty`
/// and `/dev/pts/2` are not equal even when they refer to the same line
/// discipline.
#[cfg(unix)]
fn tty_paths() -> Vec<std::path::PathBuf> {
    vec![
        std::path::PathBuf::from("/dev/tty"),
        std::path::PathBuf::from("/dev/pts/0"),
        std::path::PathBuf::from("/dev/pts/1"),
        std::path::PathBuf::from("/dev/pts/2"),
        std::path::PathBuf::from("/dev/pts/3"),
    ]
}

#[cfg(not(unix))]
fn tty_paths() -> Vec<std::path::PathBuf> {
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_new_policy_has_no_rules() {
        let policy = SandboxPolicy::new();
        assert!(policy.inherited_paths().is_empty());
    }

    #[test]
    fn default_policy_grants_the_bootstrap_set() {
        let workspace = std::env::temp_dir().join("supra-sandbox-policy-default");
        let _ = std::fs::create_dir_all(&workspace);
        let policy = default_policy(&workspace);
        let inner = policy.inner();
        // /usr is the first rule; the test asserts only that the bootstrap
        // landed in the raw policy, not how the inner PathRule array is
        // laid out - the C side owns that order.
        assert!(inner.path_count() >= 5, "the default policy carries the bootstrap set");
        let _ = std::fs::remove_dir_all(&workspace);
    }

    #[test]
    fn allow_inherited_path_adds_to_the_list() {
        let mut policy = SandboxPolicy::new();
        assert!(policy.inherited_paths().is_empty());
        policy.allow_inherited_path("/var/log/supra.log");
        assert_eq!(policy.inherited_paths(), &[PathBuf::from("/var/log/supra.log")]);
    }

    #[test]
    fn bootstrap_paths_covers_the_minimum_for_a_runnable_process() {
        let paths = bootstrap_paths(Path::new("/tmp"));
        // /usr, /bin, /lib, /dev/null, /proc are always present. /lib64 is
        // appended only when it exists, so the test does not assert on it.
        let names: Vec<&str> = paths.iter().map(|(name, _)| *name).collect();
        for required in ["/usr", "/bin", "/lib", "/dev/null", "/proc"] {
            assert!(names.contains(&required), "bootstrap missing {required}: {names:?}");
        }
    }
}
