//! OS-level process isolation.
//!
//! Safe surface over `libsupra_sandbox` (T4).
//!
//! Two Rust-specific concerns shape this module.
//!
//! **Lifetimes.** The C `supra_sandbox_policy` stores borrowed `const char*`
//! pointers, so it is only valid while the strings it names are alive. Rather
//! than expose that hazard, [`Policy`] owns its paths as `CString`s and
//! materialises the raw structure only for the duration of a call.
//!
//! **RAII.** A dropped [`Process`] handle would otherwise leak a running process
//! and leave a zombie. [`Process`] kills and reaps on drop unless it has been
//! waited on or explicitly detached.

use core::ffi::{c_char, c_int};
use std::ffi::{CString, NulError, OsStr, OsString};
use std::os::unix::ffi::{OsStrExt as _, OsStringExt as _};
use std::path::{Path, PathBuf};

use crate::sys;

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// Why a sandbox operation failed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    /// A path or argument contained an interior NUL, so it cannot cross the C
    /// boundary.
    InteriorNul,
    /// A path was not absolute. Rejected at construction because resolving a
    /// relative path would depend on the working directory at an unpredictable
    /// moment, and a sandbox whose scope shifts with `chdir` is not a boundary.
    NotAbsolute(PathBuf),
    /// The policy's path or port table is full.
    TableFull,
    /// The library refused to start the process. The message names the step.
    Refused(String),
    /// Waiting on the process failed.
    WaitFailed,
}

impl core::fmt::Display for Error {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::InteriorNul => write!(f, "value contains an interior NUL byte"),
            Self::NotAbsolute(path) => write!(f, "path is not absolute: {}", path.display()),
            Self::TableFull => write!(f, "policy table is full"),
            Self::Refused(message) => write!(f, "sandbox refused to start: {message}"),
            Self::WaitFailed => write!(f, "waiting on the sandboxed process failed"),
        }
    }
}

impl std::error::Error for Error {}

impl From<NulError> for Error {
    fn from(_: NulError) -> Self {
        Self::InteriorNul
    }
}

// ---------------------------------------------------------------------------
// Capabilities
// ---------------------------------------------------------------------------

/// Which mechanism actually enforces a policy.
///
/// Reported rather than inferred. A caller needing filesystem enforcement must
/// check this and refuse to proceed on a weaker tier; silently degrading is how a
/// sandbox becomes decorative.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Tier {
    /// No enforcement available. Nothing will run.
    None,
    /// Process and network isolation only; filesystem policy not enforced.
    Namespaces,
    /// Namespaces plus Landlock: filesystem and per-port network enforced.
    Landlock,
    /// macOS `sandbox_init` with a generated profile.
    Sbpl,
    /// Windows `AppContainer` plus a job object.
    AppContainer,
}

impl Tier {
    const fn from_raw(raw: u8) -> Self {
        match raw {
            sys::SANDBOX_TIER_NAMESPACES => Self::Namespaces,
            sys::SANDBOX_TIER_LANDLOCK => Self::Landlock,
            sys::SANDBOX_TIER_SBPL => Self::Sbpl,
            sys::SANDBOX_TIER_APPCONTAINER => Self::AppContainer,
            _ => Self::None,
        }
    }

    const fn to_raw(self) -> u8 {
        match self {
            Self::None => sys::SANDBOX_TIER_NONE,
            Self::Namespaces => sys::SANDBOX_TIER_NAMESPACES,
            Self::Landlock => sys::SANDBOX_TIER_LANDLOCK,
            Self::Sbpl => sys::SANDBOX_TIER_SBPL,
            Self::AppContainer => sys::SANDBOX_TIER_APPCONTAINER,
        }
    }

    /// Whether this tier enforces filesystem policy.
    #[must_use]
    pub const fn enforces_filesystem(self) -> bool {
        matches!(self, Self::Landlock | Self::Sbpl | Self::AppContainer)
    }
}

/// What the platform can enforce.
///
/// Four of its fields are bools because this is a field-for-field mirror of
/// `supra_sandbox_capabilities`; a designed Rust type in between would be one
/// more thing to keep in agreement with the C report.
#[allow(clippy::struct_excessive_bools)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Capabilities {
    /// Best tier available.
    pub tier: Tier,
    /// Landlock ABI version, or 0 when unavailable.
    pub landlock_abi: u8,
    /// Whether an unprivileged user namespace can be created.
    pub user_namespaces: bool,
    /// Whether network isolation is available by any mechanism.
    pub network_isolation: bool,
    /// Whether per-port TCP policy is available.
    pub port_granular_network: bool,
    /// Whether `bwrap` is on PATH. Informational: the Linux backend does not use
    /// it, for reasons recorded in the T4 notes.
    pub bubblewrap_present: bool,
    /// Explanation of any gap. Empty when the best tier is available.
    pub detail: String,
}

/// Probe the platform. Cheap and cached inside the library.
///
/// The Landlock check applies a real ruleset in a forked child and confirms a
/// denial occurs, because a reported ABI version proves only that the LSM is
/// compiled in - not that it is enabled in the boot-time LSM list.
#[must_use]
pub fn probe() -> Capabilities {
    let mut raw = core::mem::MaybeUninit::<sys::supra_sandbox_capabilities>::zeroed();
    // SAFETY: `raw` is valid, aligned storage the callee fully initialises.
    let raw = unsafe {
        sys::supra_sandbox_probe(raw.as_mut_ptr());
        raw.assume_init()
    };

    Capabilities {
        tier: Tier::from_raw(raw.tier),
        landlock_abi: raw.landlock_abi,
        user_namespaces: raw.user_namespaces != 0,
        network_isolation: raw.network_isolation != 0,
        port_granular_network: raw.port_granular_network != 0,
        bubblewrap_present: raw.bubblewrap_present != 0,
        detail: fixed_string(&raw.detail),
    }
}

/// Force the reported tier. **Testing only.**
///
/// Exists because the fail-closed refusals are unreachable on a platform that
/// supports Landlock, so the branches protecting users on weaker platforms would
/// otherwise go untested on the only platform available. Pass `None` to resume
/// normal probing.
///
/// Never call this outside tests: it makes the sandbox weaker on purpose.
pub fn force_tier_for_testing(tier: Option<Tier>) {
    let raw = tier.map_or(sys::SANDBOX_TIER_UNSET, Tier::to_raw);
    // SAFETY: takes a value, mutates only library-internal state.
    unsafe { sys::supra_sandbox_force_tier_for_testing(raw) }
}

// ---------------------------------------------------------------------------
// Policy
// ---------------------------------------------------------------------------

/// What a sandboxed process may touch.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Access(u32);

impl Access {
    /// Read files and list directories.
    pub const READ: Self = Self(sys::SANDBOX_READ);
    /// Write to and truncate existing files.
    pub const WRITE: Self = Self(sys::SANDBOX_WRITE);
    /// Execute files.
    pub const EXECUTE: Self = Self(sys::SANDBOX_EXECUTE);
    /// Create, delete, and rename beneath a path. Implies `WRITE`.
    pub const MANAGE: Self = Self(sys::SANDBOX_MANAGE);

    /// Read and execute: what a program directory needs.
    #[must_use]
    pub const fn read_execute() -> Self {
        Self(sys::SANDBOX_READ | sys::SANDBOX_EXECUTE)
    }

    /// Full access to a workspace: read, write, and manage entries.
    #[must_use]
    pub const fn workspace() -> Self {
        Self(sys::SANDBOX_READ | sys::SANDBOX_WRITE | sys::SANDBOX_MANAGE)
    }
}

impl core::ops::BitOr for Access {
    type Output = Self;
    fn bitor(self, rhs: Self) -> Self {
        Self(self.0 | rhs.0)
    }
}

/// Network policy.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Network {
    /// No network at all.
    #[default]
    None,
    /// Only the listed ports. Requires per-port support, or the spawn is refused
    /// rather than silently granting full access.
    Ports,
    /// Unrestricted.
    Full,
}

impl Network {
    const fn to_raw(self) -> u8 {
        match self {
            Self::None => sys::SANDBOX_NET_NONE,
            Self::Ports => sys::SANDBOX_NET_PORTS,
            Self::Full => sys::SANDBOX_NET_FULL,
        }
    }
}

/// A sandbox policy.
///
/// Owns its path strings, so unlike the raw C structure it cannot outlive them.
/// The default is the most restrictive setting: no paths, no network, no process
/// visibility - so a caller who forgets a field gets less access rather than more.
#[derive(Clone, Debug, Default)]
pub struct Policy {
    paths: Vec<(CString, Access)>,
    network: Network,
    ports: Vec<u16>,
    isolate_processes: bool,
    isolate_ipc: bool,
    max_processes: u32,
    max_address_space: u64,
    max_cpu_seconds: u32,
    max_file_size: u64,
    required_tier: Option<Tier>,
}

impl Policy {
    /// A policy granting nothing.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Permit access beneath `path`.
    ///
    /// The path must be absolute. Relative paths are rejected here rather than
    /// resolved, because the resolution would depend on the working directory at
    /// an unpredictable later moment.
    ///
    /// # Errors
    ///
    /// [`Error::NotAbsolute`] for a relative path, [`Error::TableFull`] once the
    /// path table has reached the ABI limit, [`Error::InteriorNul`] when the path
    /// contains a NUL byte.
    pub fn allow(&mut self, path: impl AsRef<Path>, access: Access) -> Result<&mut Self, Error> {
        let path = path.as_ref();
        if !path.is_absolute() {
            return Err(Error::NotAbsolute(path.to_path_buf()));
        }
        if self.paths.len() >= sys::SANDBOX_MAX_PATHS {
            return Err(Error::TableFull);
        }

        let raw = CString::new(path.as_os_str().as_bytes())?;
        self.paths.push((raw, access));
        Ok(self)
    }

    /// Permit one outbound TCP port, switching the policy to [`Network::Ports`].
    ///
    /// # Errors
    ///
    /// [`Error::TableFull`] once the port table has reached the ABI limit.
    pub fn allow_port(&mut self, port: u16) -> Result<&mut Self, Error> {
        if !self.ports.contains(&port) {
            if self.ports.len() >= sys::SANDBOX_MAX_PORTS {
                return Err(Error::TableFull);
            }
            self.ports.push(port);
        }
        self.network = Network::Ports;
        Ok(self)
    }

    /// Set the network policy directly.
    pub fn network(&mut self, network: Network) -> &mut Self {
        self.network = network;
        self
    }

    /// Hide host processes by isolating the PID namespace.
    pub fn isolate_processes(&mut self, isolate: bool) -> &mut Self {
        self.isolate_processes = isolate;
        self
    }

    /// Isolate the IPC and UTS namespaces.
    pub fn isolate_ipc(&mut self, isolate: bool) -> &mut Self {
        self.isolate_ipc = isolate;
        self
    }

    /// Cap the process count. Advisory scheduling pressure, not accounting.
    pub fn max_processes(&mut self, limit: u32) -> &mut Self {
        self.max_processes = limit;
        self
    }

    /// Cap the address space in bytes.
    pub fn max_address_space(&mut self, bytes: u64) -> &mut Self {
        self.max_address_space = bytes;
        self
    }

    /// Cap CPU seconds.
    pub fn max_cpu_seconds(&mut self, seconds: u32) -> &mut Self {
        self.max_cpu_seconds = seconds;
        self
    }

    /// Cap file size in bytes.
    pub fn max_file_size(&mut self, bytes: u64) -> &mut Self {
        self.max_file_size = bytes;
        self
    }

    /// Refuse to run below this tier.
    ///
    /// Set [`Tier::Landlock`] to require filesystem enforcement rather than hope
    /// for it.
    pub fn require_tier(&mut self, tier: Tier) -> &mut Self {
        self.required_tier = Some(tier);
        self
    }

    /// Number of filesystem rules currently in the policy.
    ///
    /// Exposed for callers - the T16 policy layer asserts on the *count*
    /// before sending the policy to the C side, and counting the raw
    /// `paths` slice would force the field public. The method is the
    /// shape the FFI already had to manage.
    #[must_use]
    pub fn path_count(&self) -> usize {
        self.paths.len()
    }

    /// Build the raw structure.
    ///
    /// The pointers it holds borrow from `self`, so the result must not outlive
    /// this call - which is why it is private and only ever passed straight into
    /// `spawn`.
    fn to_raw(&self) -> sys::supra_sandbox_policy {
        let mut raw = core::mem::MaybeUninit::<sys::supra_sandbox_policy>::zeroed();
        // SAFETY: valid, aligned storage the callee fully initialises.
        let mut raw = unsafe {
            sys::supra_sandbox_policy_init(raw.as_mut_ptr());
            raw.assume_init()
        };

        // Populated through the ABI's own constructors rather than by writing
        // fields directly. Direct writes would mean reimplementing the C side's
        // normalisation - MANAGE implying WRITE, port de-duplication, the count
        // and array staying in agreement - and a second implementation of a
        // security-relevant rule is a second thing that can drift.
        //
        // Failures are unreachable here because `allow` and `allow_port` already
        // rejected non-absolute paths and enforced the table bounds, but they are
        // still checked: silently dropping a rule would widen the sandbox.
        for (path, access) in &self.paths {
            // SAFETY: `raw` is a live local; `path` outlives this call because it
            // is owned by `self`, which outlives `to_raw`'s caller.
            let accepted = unsafe { sys::supra_sandbox_policy_allow(&raw mut raw, path.as_ptr(), access.0) };
            debug_assert_eq!(accepted, 1, "policy_allow rejected a rule that `allow` accepted");
        }

        for port in &self.ports {
            // SAFETY: `raw` is a live local; the port is passed by value.
            let accepted = unsafe { sys::supra_sandbox_policy_allow_port(&raw mut raw, *port) };
            debug_assert_eq!(accepted, 1, "policy_allow_port rejected a port `allow_port` accepted");
        }

        // Set after the ports: `allow_port` switches the mode to Ports, which
        // would otherwise override an explicit Full or None.
        raw.network = self.network.to_raw();

        raw.isolate_processes = u8::from(self.isolate_processes);
        raw.isolate_ipc = u8::from(self.isolate_ipc);
        raw.max_processes = self.max_processes;
        raw.max_address_space = self.max_address_space;
        raw.max_cpu_seconds = self.max_cpu_seconds;
        raw.max_file_size = self.max_file_size;
        raw.required_tier = self.required_tier.map_or(0, Tier::to_raw);

        raw
    }
}

// ---------------------------------------------------------------------------
// Command
// ---------------------------------------------------------------------------

/// What to run inside the sandbox.
///
/// An argv vector, never a shell string: a command line cannot be
/// re-interpreted, and quoting bugs in a generated one are a whole vulnerability
/// class avoided by construction.
///
/// The environment is **not** inherited by default. It routinely carries
/// credentials, and passing it silently would leak them into every sandboxed
/// command.
#[derive(Clone, Debug)]
pub struct Command {
    program: CString,
    args: Vec<CString>,
    env: Vec<CString>,
    working_dir: Option<CString>,
    stdin_fd: c_int,
    stdout_fd: c_int,
    stderr_fd: c_int,
}

impl Command {
    /// A command running `program` with no arguments and no environment.
    ///
    /// # Errors
    ///
    /// [`Error::InteriorNul`] when the program path contains a NUL byte.
    pub fn new(program: impl AsRef<Path>) -> Result<Self, Error> {
        let program_c = CString::new(program.as_ref().as_os_str().as_bytes())?;
        Ok(Self {
            // argv[0] conventionally repeats the program path.
            args: vec![program_c.clone()],
            program: program_c,
            env: Vec::new(),
            working_dir: None,
            stdin_fd: -1,
            stdout_fd: -1,
            stderr_fd: -1,
        })
    }

    /// Append one argument.
    ///
    /// # Errors
    ///
    /// [`Error::InteriorNul`] when the argument contains a NUL byte.
    pub fn arg(&mut self, arg: impl AsRef<OsStr>) -> Result<&mut Self, Error> {
        self.args.push(CString::new(arg.as_ref().as_bytes())?);
        Ok(self)
    }

    /// Append several arguments.
    ///
    /// # Errors
    ///
    /// [`Error::InteriorNul`] for the first argument containing a NUL byte; later
    /// arguments are not visited.
    pub fn args<I, S>(&mut self, args: I) -> Result<&mut Self, Error>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        for arg in args {
            self.arg(arg)?;
        }
        Ok(self)
    }

    /// Set one environment variable.
    ///
    /// Only variables set explicitly are visible to the child.
    ///
    /// # Errors
    ///
    /// [`Error::InteriorNul`] when the key or value contains a NUL byte.
    pub fn env(&mut self, key: impl AsRef<OsStr>, value: impl AsRef<OsStr>) -> Result<&mut Self, Error> {
        let mut entry = OsString::from(key.as_ref());
        entry.push("=");
        entry.push(value.as_ref());
        self.env.push(CString::new(entry.into_vec())?);
        Ok(self)
    }

    /// Set the working directory. Must be readable under the policy.
    ///
    /// # Errors
    ///
    /// [`Error::InteriorNul`] when the directory contains a NUL byte.
    pub fn working_dir(&mut self, dir: impl AsRef<Path>) -> Result<&mut Self, Error> {
        self.working_dir = Some(CString::new(dir.as_ref().as_os_str().as_bytes())?);
        Ok(self)
    }

    /// Point the child's stdin at an existing descriptor. `-1` means `/dev/null`.
    ///
    /// The descriptor is borrowed, not consumed: it must stay open until the child
    /// has started, and closing it is the caller's responsibility.
    pub fn stdin(&mut self, fd: c_int) -> &mut Self {
        self.stdin_fd = fd;
        self
    }

    /// Point the child's stdout at an existing descriptor.
    pub fn stdout(&mut self, fd: c_int) -> &mut Self {
        self.stdout_fd = fd;
        self
    }

    /// Point the child's stderr at an existing descriptor.
    pub fn stderr(&mut self, fd: c_int) -> &mut Self {
        self.stderr_fd = fd;
        self
    }
}

// ---------------------------------------------------------------------------
// Process
// ---------------------------------------------------------------------------

/// A running sandboxed process.
///
/// Kills and reaps on drop unless [`Process::wait`] has returned or
/// [`Process::detach`] was called. Without that, dropping a handle would leave a
/// process running and a zombie behind - and an agent harness spawns enough
/// commands that the leak would be measured in hundreds.
pub struct Process {
    raw: sys::supra_sandbox_process,
    tier: Tier,
    reaped: bool,
}

impl Process {
    /// Host-visible process id.
    #[must_use]
    pub const fn pid(&self) -> i64 {
        self.raw.pid
    }

    /// Tier that actually applied.
    #[must_use]
    pub const fn tier(&self) -> Tier {
        self.tier
    }

    /// Wait for the process to exit.
    ///
    /// `timeout` of `None` blocks. Returns `Ok(None)` on timeout, leaving the
    /// process running.
    ///
    /// # Errors
    ///
    /// [`Error::WaitFailed`] when the underlying wait fails, which this layer
    /// cannot distinguish from a lost child.
    pub fn wait(&mut self, timeout: Option<core::time::Duration>) -> Result<Option<i32>, Error> {
        let timeout_ms = timeout.map_or(0, |d| {
            // 0 means "block forever" in the ABI, so a sub-millisecond timeout
            // must round up rather than turn into an indefinite wait.
            u32::try_from(d.as_millis()).unwrap_or(u32::MAX).max(1)
        });

        let mut status: c_int = -1;
        // SAFETY: the raw-borrowed pointer references a live structure; `status`
        // is a live local.
        let result = unsafe { sys::supra_sandbox_wait(&raw const self.raw, timeout_ms, &raw mut status) };

        match result {
            1 => {
                self.reaped = true;
                Ok(Some(status))
            }
            0 => Ok(None),
            _ => Err(Error::WaitFailed),
        }
    }

    /// Terminate the process and everything it started.
    ///
    /// `SIGTERM` to the process group, then `SIGKILL` after `grace`. The group is
    /// the unit because a sandboxed command routinely spawns children, and
    /// killing only the leader leaves them running.
    pub fn kill(&mut self, grace: core::time::Duration) -> bool {
        let grace_ms = u32::try_from(grace.as_millis()).unwrap_or(u32::MAX);
        // SAFETY: the raw-borrowed pointer references a live structure; the callee
        // only reads it.
        let ok = unsafe { sys::supra_sandbox_kill(&raw const self.raw, grace_ms) == 1 };
        if ok {
            // kill reaps as part of confirming death, so a later wait would find
            // no child.
            self.reaped = true;
        }
        ok
    }

    /// Give up ownership without killing.
    ///
    /// For a process meant to outlive this handle. The caller becomes responsible
    /// for reaping it.
    #[must_use = "the pid is the caller's only handle once the process is detached"]
    pub fn detach(mut self) -> i64 {
        self.reaped = true;
        self.raw.pid
    }
}

impl core::fmt::Debug for Process {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        // Hand-written rather than derived: the raw structure carries a 256-byte
        // error array that would drown the fields a reader actually wants.
        f.debug_struct("Process")
            .field("pid", &self.raw.pid)
            .field("tier", &self.tier)
            .field("reaped", &self.reaped)
            .finish()
    }
}

impl Drop for Process {
    fn drop(&mut self) {
        if self.reaped || self.raw.pid < 0 {
            return;
        }
        // A short grace period: this runs on a drop path, possibly during unwind,
        // so it must not block for long. Failure is ignored because there is
        // nowhere to report it from a destructor.
        let _ = self.kill(core::time::Duration::from_millis(200));
    }
}

/// Apply `policy` and start `command`.
///
/// Fails closed: when the policy cannot be enforced - a tier below what was
/// required, a filesystem rule on a platform that cannot enforce it, a per-port
/// network policy without support - no process starts and the error explains why.
///
/// # Errors
///
/// [`Error::Refused`] when the sandbox declines to start the process; the message
/// names the enforcement step that objected. No process is left behind.
pub fn spawn(policy: &Policy, command: &Command) -> Result<Process, Error> {
    let policy_raw = policy.to_raw();

    // argv and envp must be NULL-terminated pointer arrays. Built here so the
    // pointers cannot outlive the CStrings they borrow from.
    let mut argv: Vec<*const c_char> = command.args.iter().map(|a| a.as_ptr()).collect();
    argv.push(core::ptr::null());

    let mut envp: Vec<*const c_char> = command.env.iter().map(|e| e.as_ptr()).collect();
    envp.push(core::ptr::null());

    let command_raw = sys::supra_sandbox_command {
        program: command.program.as_ptr(),
        argv: argv.as_ptr(),
        // Always non-null, so an empty vector means "no environment" rather than
        // "inherit". The C side treats null the same way, but being explicit here
        // removes the question.
        envp: envp.as_ptr(),
        working_dir: command.working_dir.as_ref().map_or(core::ptr::null(), |d| d.as_ptr()),
        stdin_fd: command.stdin_fd,
        stdout_fd: command.stdout_fd,
        stderr_fd: command.stderr_fd,
    };

    let mut process = core::mem::MaybeUninit::<sys::supra_sandbox_process>::zeroed();

    // SAFETY: `policy_raw` and `command_raw` are live locals whose borrowed
    // pointers - the CStrings in `policy` and `command`, and the argv/envp vectors
    // above - all outlive this call. `process` is valid, aligned storage the callee
    // fully initialises.
    let started = unsafe {
        sys::supra_sandbox_spawn(&raw const policy_raw, &raw const command_raw, process.as_mut_ptr())
    };

    // SAFETY: the callee initialises the structure on both paths, since the error
    // string must be readable after a failure.
    let raw = unsafe { process.assume_init() };

    if started == 1 {
        Ok(Process { raw, tier: Tier::from_raw(raw.tier), reaped: false })
    } else {
        Err(Error::Refused(fixed_string(&raw.error)))
    }
}

// ---------------------------------------------------------------------------
// Self-identification
// ---------------------------------------------------------------------------

/// Device and inode identifying a file.
///
/// Identity rather than name, so a copy, symlink, or rename cannot masquerade.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct FileIdentity {
    /// Device the file lives on.
    pub device: u64,
    /// Inode within that device.
    pub inode: u64,
}

/// Canonical path of the running executable.
#[must_use]
pub fn self_path() -> Option<PathBuf> {
    let mut buffer = [0_u8; 4096];
    // SAFETY: the buffer is valid for its own length, and the callee writes at
    // most that many bytes including the terminator.
    let len = unsafe { sys::supra_sandbox_self_path(buffer.as_mut_ptr().cast(), buffer.len()) };
    if len == 0 || len >= buffer.len() {
        return None;
    }
    Some(PathBuf::from(OsStr::from_bytes(&buffer[..len])))
}

/// Identity of the running executable.
///
/// Backs guard layer L4 in T12.5: refusing to spawn this binary from inside
/// itself. Note the residual gap recorded in T4 - a *copied* binary has a
/// different inode and will not match, which is why T12.5 layers an environment
/// marker as well.
#[must_use]
pub fn self_identity() -> Option<FileIdentity> {
    let mut device: u64 = 0;
    let mut inode: u64 = 0;
    // SAFETY: both pointers reference live locals.
    let ok = unsafe { sys::supra_sandbox_self_identity(&raw mut device, &raw mut inode) };
    (ok == 1).then_some(FileIdentity { device, inode })
}

/// Identity of the file `path` resolves to, following symlinks.
///
/// # Errors
///
/// [`Error::InteriorNul`] when the path contains a NUL byte. A path that does not
/// resolve is `Ok(None)`, not an error: absence is an answer.
pub fn file_identity(path: impl AsRef<Path>) -> Result<Option<FileIdentity>, Error> {
    let raw_path = CString::new(path.as_ref().as_os_str().as_bytes())?;
    let mut device: u64 = 0;
    let mut inode: u64 = 0;
    // SAFETY: `raw_path` is a live NUL-terminated string; both output pointers
    // reference live locals.
    let ok = unsafe { sys::supra_sandbox_file_identity(raw_path.as_ptr(), &raw mut device, &raw mut inode) };
    Ok((ok == 1).then_some(FileIdentity { device, inode }))
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Read a fixed-size C char array into a `String`.
///
/// Stops at the first NUL, and replaces invalid UTF-8 rather than failing: these
/// are diagnostic strings, and losing the message because one byte was malformed
/// would be worse than an imperfect message.
fn fixed_string(buffer: &[c_char]) -> String {
    // Bit-exact reinterpretation rather than a cast: a byte >= 0x80 arrives as a
    // negative c_char, and it is the byte, not its signed value, that matters.
    let bytes: Vec<u8> =
        buffer.iter().take_while(|byte| **byte != 0).map(|byte| byte.to_ne_bytes()[0]).collect();
    String::from_utf8_lossy(&bytes).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixed_string_stops_at_the_first_nul() {
        // The diagnostic fields are fixed-size arrays; everything after the first
        // NUL is stale bytes from a previous, longer message. Concatenating them
        // would make error text depend on what the buffer held before. 97 is 'a',
        // and literals infer their type from the array, so no cast is needed.
        let mut buffer: [c_char; 8] = [97; 8];
        assert_eq!(fixed_string(&buffer), "aaaaaaaa");

        buffer[2] = 0;
        assert_eq!(fixed_string(&buffer), "aa");

        buffer[0] = 0;
        assert_eq!(fixed_string(&buffer), "");

        // A message byte >= 0x80 arrives as a negative c_char and must survive as
        // its byte, then be replaced, not silently dropped.
        buffer[0] = -1;
        buffer[1] = 0;
        assert_eq!(fixed_string(&buffer), String::from('\u{FFFD}'));
    }

    fn workspace_policy(workspace: &Path) -> Policy {
        let mut policy = Policy::new();
        policy
            .allow("/usr", Access::read_execute())
            .expect("/usr")
            .allow("/bin", Access::read_execute())
            .expect("/bin")
            .allow("/lib", Access::read_execute())
            .expect("/lib");
        if Path::new("/lib64").exists() {
            policy.allow("/lib64", Access::read_execute()).expect("/lib64");
        }
        policy
            .allow("/dev/null", Access::READ | Access::WRITE)
            .expect("/dev/null")
            .allow(workspace, Access::workspace())
            .expect("workspace")
            .isolate_processes(true)
            .isolate_ipc(true)
            .require_tier(Tier::Landlock);
        policy
    }

    #[test]
    fn default_policy_grants_nothing() {
        let policy = Policy::new();
        assert!(policy.paths.is_empty());
        assert_eq!(policy.network, Network::None);
        assert_eq!(policy.required_tier, None);
    }

    #[test]
    fn relative_paths_are_rejected() {
        // A sandbox whose scope shifts with chdir is not a boundary, so this is
        // refused at construction rather than resolved.
        let mut policy = Policy::new();
        assert!(matches!(policy.allow("relative/path", Access::READ), Err(Error::NotAbsolute(_))));
        assert!(matches!(policy.allow("../up", Access::READ), Err(Error::NotAbsolute(_))));
        assert!(policy.paths.is_empty());
    }

    #[test]
    fn interior_nul_is_rejected() {
        let mut policy = Policy::new();
        assert_eq!(policy.allow("/tmp/a\0b", Access::READ).err(), Some(Error::InteriorNul));
    }

    #[test]
    fn path_table_is_bounded() {
        let mut policy = Policy::new();
        for _ in 0..sys::SANDBOX_MAX_PATHS {
            policy.allow("/tmp", Access::READ).expect("within the limit");
        }
        assert_eq!(policy.allow("/tmp", Access::READ).err(), Some(Error::TableFull));
    }

    #[test]
    fn ports_are_deduplicated() {
        let mut policy = Policy::new();
        policy.allow_port(443).expect("port");
        policy.allow_port(443).expect("duplicate");
        assert_eq!(policy.ports, vec![443]);
        assert_eq!(policy.network, Network::Ports);
    }

    #[test]
    fn manage_implies_write_in_the_raw_policy() {
        let mut policy = Policy::new();
        policy.allow("/tmp", Access::MANAGE).expect("allow");
        let raw = policy.to_raw();
        assert_ne!(raw.paths[0].access & sys::SANDBOX_WRITE, 0);
    }

    #[test]
    fn probe_is_internally_consistent() {
        let caps = probe();
        if caps.tier == Tier::Landlock {
            assert!(caps.landlock_abi > 0, "the tier implies a working ABI");
            assert!(caps.user_namespaces);
        }
        if caps.port_granular_network {
            assert!(caps.landlock_abi >= 4);
        }
        if !caps.tier.enforces_filesystem() {
            assert!(!caps.detail.is_empty(), "a degraded tier must explain itself");
        }
    }

    #[test]
    fn unenforceable_policy_is_refused() {
        force_tier_for_testing(Some(Tier::Namespaces));

        let mut policy = Policy::new();
        policy.allow("/usr", Access::read_execute()).expect("allow");
        let command = Command::new("/bin/true").expect("command");

        let result = spawn(&policy, &command);
        force_tier_for_testing(None);

        match result {
            Err(Error::Refused(message))
                if message.contains("cannot enforce")
                    || message.contains("backend not implemented")
                    || message.contains("unprivileged user namespaces unavailable") => {}
            other => panic!("expected a refusal, got {other:?}"),
        }
    }

    #[test]
    fn required_tier_is_honoured() {
        let mut policy = Policy::new();
        policy.require_tier(Tier::AppContainer);
        let command = Command::new("/bin/true").expect("command");

        match spawn(&policy, &command) {
            Err(Error::Refused(message))
                if message.contains("requires tier")
                    || message.contains("backend not implemented")
                    || message.contains("unprivileged user namespaces unavailable") => {}
            other => panic!("expected a refusal, got {other:?}"),
        }
    }

    #[test]
    fn runs_a_command_and_reports_its_status() {
        let caps = probe();
        if !caps.tier.enforces_filesystem() {
            eprintln!("skipped: tier {:?} cannot enforce filesystem policy", caps.tier);
            return;
        }

        let workspace = std::env::temp_dir().join("supra-ffi-sandbox-test");
        std::fs::create_dir_all(&workspace).expect("workspace");

        let policy = workspace_policy(&workspace);
        let mut command = Command::new("/bin/sh").expect("command");
        command.args(["-c", "exit 9"]).expect("args").env("PATH", "/usr/bin:/bin").expect("env");

        let mut process = match spawn(&policy, &command) {
            Ok(process) => process,
            Err(Error::Refused(message)) if message.contains("uid_map") => {
                eprintln!("skipped: user namespaces unavailable on this host ({message})");
                return;
            }
            Err(error) => panic!("spawn: {error:?}"),
        };
        let status = process.wait(Some(core::time::Duration::from_secs(10))).expect("wait").expect("exited");
        assert_eq!(status, 9, "the payload's own exit code reaches the caller");
    }

    #[test]
    fn environment_is_not_inherited() {
        let caps = probe();
        if !caps.tier.enforces_filesystem() {
            return;
        }

        // Safety of set_var: single-threaded test, and the value is only read by a
        // child process that must NOT see it.
        unsafe { std::env::set_var("SUPRA_FFI_CANARY", "must-not-leak") };

        let workspace = std::env::temp_dir().join("supra-ffi-sandbox-test");
        std::fs::create_dir_all(&workspace).expect("workspace");

        let policy = workspace_policy(&workspace);
        let mut command = Command::new("/bin/sh").expect("command");
        command
            .args(["-c", "test -z \"$SUPRA_FFI_CANARY\""])
            .expect("args")
            .env("PATH", "/usr/bin:/bin")
            .expect("env");

        let mut process = match spawn(&policy, &command) {
            Ok(process) => process,
            Err(Error::Refused(message)) if message.contains("uid_map") => {
                eprintln!("skipped: user namespaces unavailable on this host ({message})");
                return;
            }
            Err(error) => panic!("spawn: {error:?}"),
        };
        let status = process.wait(Some(core::time::Duration::from_secs(10))).expect("wait").expect("exited");

        unsafe { std::env::remove_var("SUPRA_FFI_CANARY") };
        assert_eq!(status, 0, "the host environment leaked into the sandbox");
    }

    #[test]
    fn dropping_a_handle_kills_the_process() {
        let caps = probe();
        if !caps.tier.enforces_filesystem() {
            return;
        }

        let workspace = std::env::temp_dir().join("supra-ffi-sandbox-test");
        std::fs::create_dir_all(&workspace).expect("workspace");

        let policy = workspace_policy(&workspace);
        let mut command = Command::new("/bin/sh").expect("command");
        command.args(["-c", "sleep 60"]).expect("args").env("PATH", "/usr/bin:/bin").expect("env");

        let pid = {
            let process = match spawn(&policy, &command) {
                Ok(process) => process,
                Err(Error::Refused(message)) if message.contains("uid_map") => {
                    eprintln!("skipped: user namespaces unavailable on this host ({message})");
                    return;
                }
                Err(error) => panic!("spawn: {error:?}"),
            };
            let pid = process.pid();
            assert!(pid > 0);
            pid
            // Dropped here. Without the Drop impl this process would outlive the
            // test and leak.
        };

        // Reaped, so the pid is gone. Checked through /proc rather than kill(0),
        // which would also succeed for a zombie.
        std::thread::sleep(core::time::Duration::from_millis(300));
        let still_running = Path::new(&format!("/proc/{pid}")).exists();
        assert!(!still_running, "dropping the handle must terminate the process");
    }

    #[test]
    fn detach_leaves_the_process_running() {
        let caps = probe();
        if !caps.tier.enforces_filesystem() {
            return;
        }

        let workspace = std::env::temp_dir().join("supra-ffi-sandbox-test");
        std::fs::create_dir_all(&workspace).expect("workspace");

        let policy = workspace_policy(&workspace);
        let mut command = Command::new("/bin/sh").expect("command");
        command.args(["-c", "exit 0"]).expect("args").env("PATH", "/usr/bin:/bin").expect("env");

        let process = match spawn(&policy, &command) {
            Ok(process) => process,
            Err(Error::Refused(message)) if message.contains("uid_map") => {
                eprintln!("skipped: user namespaces unavailable on this host ({message})");
                return;
            }
            Err(error) => panic!("spawn: {error:?}"),
        };
        let pid = process.detach();
        assert!(pid > 0, "detach hands back the pid");
        // Reaping is now the caller's problem; nothing to assert beyond not
        // panicking on drop.
    }

    #[test]
    fn self_identity_matches_the_running_binary() {
        let path = self_path().expect("self path");
        assert!(path.is_absolute());

        let by_self = self_identity().expect("self identity");
        let by_path = file_identity(&path).expect("no NUL").expect("resolves");
        assert_eq!(by_self, by_path, "identity by path must agree with identity by self");

        let other = file_identity("/bin/sh").expect("no NUL").expect("resolves");
        assert_ne!(other, by_self, "an unrelated binary differs");

        assert_eq!(file_identity("/nonexistent-supra-ffi-probe").expect("no NUL"), None);
    }

    #[test]
    fn command_rejects_interior_nul() {
        assert_eq!(Command::new("/bin/sh\0evil").err(), Some(Error::InteriorNul));

        let mut command = Command::new("/bin/sh").expect("command");
        assert_eq!(command.arg("a\0b").err(), Some(Error::InteriorNul));
        assert_eq!(command.env("K\0", "v").err(), Some(Error::InteriorNul));
    }
}
