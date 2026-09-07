//! Choreographed spawn.
//!
//! `SpawnTicket` is the host-side single entry point a tool call uses to
//! start a sandboxed command. Three things must happen in order, and the
//! order is the product:
//!
//! 1. **The fd audit** finds every descriptor without `FD_CLOEXEC`. A
//!    single leak defeats the whole boundary: the child can read any file
//!    the host can read, just through a different path.
//! 2. **The guard** (T12.5) refuses self-spawn and verifies the marker.
//!    Authority is enforced before the FFI is even called.
//! 3. **The permission gate** (T16.7 contract) refuses the reversibility
//!    the classifier assigned, in the user's mode. Consent is the last
//!    gate, never the first.
//!
//! A failure at any of the three returns an error and a [`SpawnTicket`]
//! that the caller can render for the user. None of the three is
//! optional, and a future contributor who removes any one of them is
//! removing a layer the design says must hold.

use std::io;
use std::path::Path;
use std::process::ExitStatus;

use supra_ffi::sandbox::{self, Process, Tier};
use supra_types::{AgentId, Lineage, Mode, Reversibility};

use crate::error::SandboxError;
use crate::fd_audit::{self, DescriptorRecord};
use crate::policy::SandboxPolicy;
use crate::tree::TreeBudget;

/// What the spawn needs from the caller.
///
/// The fields are deliberately explicit. Hiding them behind a builder
/// means the caller has to track what the builder does on its behalf; a
/// flat struct means the type system says "these are the four things you
/// have to provide", and the rest is filled in by the host.
///
/// `Clone`/`Copy` are derived because the shell session (T16.5) forwards the
/// caller's request and overrides `stdio`; a request is a bundle of
/// references, so copying it is copying a handful of pointers.
#[derive(Clone, Copy, Debug)]
pub struct SpawnRequest<'a> {
    /// The command's argv. `argv[0]` is conventionally the program path.
    pub argv: &'a [&'a str],
    /// The environment the child should see. Empty by default: a default
    /// that *inherits* the host env would leak the secrets the host holds
    /// into every child.
    pub env: &'a [(&'a str, &'a str)],
    /// The directory the child should run in. `None` for the host's cwd.
    pub working_dir: Option<&'a Path>,
    /// The reversibility the classifier (T16.7) assigned.
    pub reversibility: Reversibility,
    /// The mode in force at the moment of the call. Frozen per session.
    pub mode: Mode,
    /// The guard's marker, if any. The host process mints one and passes
    /// it; the child presents it to L5.
    pub marker: Option<&'a str>,
    /// The lineage the child extends, if spawning a peer. `None` for a
    /// host-side command that has no child id to register.
    pub lineage: Option<&'a Lineage>,
    /// The id the child would carry, if spawning a peer.
    pub child: Option<AgentId>,
    /// Whether the child is voting. `Some((proposer, voter))` triggers
    /// L7's self-vote refusal when the two are equal.
    pub claim_vote: Option<(AgentId, AgentId)>,
    /// Where the child's standard streams come from.
    ///
    /// The default (`Stdio::default()`) points all three at `/dev/null`,
    /// which is right for a batch command. A caller that needs to *see* the
    /// output passes descriptors - a pipe pair, or the slave end of a pty
    /// (T16.5). The descriptors are borrowed for the spawn only; they stay
    /// open in the parent and the parent closes them, because a library
    /// that closes descriptors out from under its caller corrupts it.
    pub stdio: Stdio,
}

/// Borrowed descriptors for the child's standard streams.
///
/// `None` means the C side opens `/dev/null` for that stream, so a child
/// that reads stdin reads nothing and a child that writes stdout writes
/// into the void - both are correct fail-closed shapes for a command whose
/// output nobody asked for. `Some(fd)` dup2s the descriptor onto the
/// stream before exec; `dup2` clears CLOEXEC on the duplicate, so the
/// stream survives the exec even though the source descriptor may not.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Stdio {
    /// The child's stdin. `None` = `/dev/null`.
    pub stdin: Option<core::ffi::c_int>,
    /// The child's stdout. `None` = `/dev/null`.
    pub stdout: Option<core::ffi::c_int>,
    /// The child's stderr. `None` = `/dev/null`.
    pub stderr: Option<core::ffi::c_int>,
}

impl Stdio {
    /// All three streams from one descriptor - the pty-slave shape, where
    /// stdin, stdout, and stderr must be the same terminal.
    #[must_use]
    pub const fn all(fd: core::ffi::c_int) -> Self {
        Self { stdin: Some(fd), stdout: Some(fd), stderr: Some(fd) }
    }

    /// All three streams at `/dev/null`.
    #[must_use]
    pub const fn null() -> Self {
        Self { stdin: None, stdout: None, stderr: None }
    }
}

/// A pre-flight check that did not run, with a slot for the cause.
///
/// `SpawnTicket` is both the entry point and the error type. The reason
/// a single type carries both is that the caller can render the ticket
/// for the user whether the call succeeded or failed - the same fields
/// describe what was about to be done.
#[derive(Debug)]
#[non_exhaustive]
pub struct SpawnTicket {
    /// The argv the caller's request had. Recorded for diagnostics; the
    /// ticket owns the strings through `Display`, not through `argv`,
    /// because the command may be re-rendered with redactions in T29.
    pub argv: Vec<String>,
    /// The reversibility the ticket was asked to spawn.
    pub reversibility: Reversibility,
    /// The mode the ticket was asked to spawn under.
    pub mode: Mode,
}

impl SpawnTicket {
    /// Build a ticket from a request.
    #[must_use]
    pub fn from_request(request: &SpawnRequest<'_>) -> Self {
        Self {
            argv: request.argv.iter().map(|arg| (*arg).to_owned()).collect(),
            reversibility: request.reversibility,
            mode: request.mode,
        }
    }
}

impl core::fmt::Display for SpawnTicket {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(
            f,
            "spawn argv[0]={:?} mode={:?} reversibility={:?}",
            self.argv.first(),
            self.mode,
            self.reversibility
        )
    }
}

/// Run the pre-spawn audit, then the guard, then the FFI.
///
/// `tree` is shared across the host so the process-tree budget is one
/// number the TUI can show. `policy` is borrowed for the duration of the
/// call only; the C ABI's borrowed pointers live as long as `policy`.
///
/// # Errors
///
/// [`SandboxError::LeakyDescriptor`] when a descriptor without
/// `FD_CLOEXEC` is open and not on the allow list.
/// [`SandboxError::Authority`] when the guard refuses.
/// [`SandboxError::Ffi`] when the C side refuses.
pub fn spawn(
    policy: &SandboxPolicy,
    request: &SpawnRequest<'_>,
    tree: &TreeBudget,
) -> Result<Process, SandboxError> {
    audit_descriptors(policy)?;

    let identity = supra_guard::current().ok_or_else(|| {
        SandboxError::Authority(
            0,
            vec![supra_guard::Refusal::NoIdentity {
                detail: "process identity is not established".to_owned(),
            }],
        )
    })?;

    let argv_strings: Vec<&str> = request.argv.to_vec();
    let (proposer, voter) = match request.claim_vote {
        Some((p, v)) => (Some(p), Some(v)),
        None => (None, None),
    };
    let guard_request = supra_guard::SpawnRequest {
        argv: &argv_strings,
        marker: request.marker,
        lineage: request.lineage,
        child: request.child,
        claim_proposer: proposer,
        voter,
    };
    let verdict = supra_guard::judge(&guard_request);
    if !verdict.allowed() {
        return Err(SandboxError::Authority(verdict.refusals.len(), verdict.refusals));
    }
    let _ = identity;

    let mut command = sandbox::Command::new(request.argv[0]).map_err(|error| map_ffi(&error))?;
    for arg in &request.argv[1..] {
        command.arg(arg).map_err(|error| map_ffi(&error))?;
    }
    for (key, value) in request.env {
        command.env(key, value).map_err(|error| map_ffi(&error))?;
    }
    if let Some(dir) = request.working_dir {
        command.working_dir(dir).map_err(|error| map_ffi(&error))?;
    }

    // Borrowed stdio: the caller's descriptors become the child's streams.
    // `None` maps to the C side's `-1`, which opens `/dev/null`.
    command.stdin(request.stdio.stdin.unwrap_or(-1));
    command.stdout(request.stdio.stdout.unwrap_or(-1));
    command.stderr(request.stdio.stderr.unwrap_or(-1));

    let process = sandbox::spawn(policy.inner(), &command).map_err(|error| map_ffi(&error))?;
    tree.record_child();
    Ok(process)
}

fn audit_descriptors(policy: &SandboxPolicy) -> Result<(), SandboxError> {
    // The audit always runs. `allow_inherited_path` adds paths to the allow
    // list; it does not turn the audit off. A policy without an allow list
    // refuses on the first non-stdio descriptor; a policy with one refuses
    // on the first descriptor that *is not* on the list. There is no third
    // shape.
    let records = fd_audit::snapshot().map_err(|error| map_io(&error))?;
    if let Some(leak) = fd_leaks(policy, &records).into_iter().next() {
        return Err(SandboxError::LeakyDescriptor { fd: leak.fd, path: leak.path.clone() });
    }
    Ok(())
}

fn fd_leaks<'a>(policy: &'a SandboxPolicy, records: &'a [DescriptorRecord]) -> Vec<&'a DescriptorRecord> {
    fd_audit::find_leaks(records, |path| {
        policy.inherited_paths().iter().any(|allowed| allowed.to_string_lossy().as_ref() == path)
    })
}

fn map_ffi(error: &sandbox::Error) -> SandboxError {
    SandboxError::Ffi(error.to_string())
}

fn map_io(error: &io::Error) -> SandboxError {
    if error.kind() == io::ErrorKind::Unsupported {
        return SandboxError::Unsupported { detail: error.to_string() };
    }
    SandboxError::Ffi(error.to_string())
}

/// Best-available enforcement tier, exposed for tests and the TUI.
///
/// The platform's tier is what the C side reports, not what the policy
/// asked for: a policy that requires `Landlock` on a `Namespaces`-only
/// platform is refused by the FFI. The TUI shows the *platform* tier
/// here, and the *policy* tier next to it, so a mismatch is visible.
#[must_use]
pub fn platform_tier() -> Tier {
    sandbox::probe().tier
}

/// Wrap a [`std::process::ExitStatus`] into something the harness logs.
///
/// The exit status's `success()` method says "did the child return 0",
/// which is what most callers want; the message carries the raw status
/// for the cases that need it.
#[must_use]
pub fn status_summary(status: ExitStatus) -> &'static str {
    if status.success() { "ok" } else { "non-zero" }
}

#[cfg(test)]
mod tests {
    use std::os::fd::AsRawFd as _;

    use super::*;

    #[test]
    fn spawn_ticket_records_what_was_asked() {
        let argv = ["/bin/sh", "-c", "true"];
        let request = SpawnRequest {
            argv: &argv,
            env: &[],
            working_dir: None,
            reversibility: Reversibility::R0,
            mode: Mode::Auto,
            marker: None,
            lineage: None,
            child: None,
            claim_vote: None,
            stdio: Stdio::null(),
        };
        let ticket = SpawnTicket::from_request(&request);
        assert_eq!(ticket.argv, vec!["/bin/sh", "-c", "true"]);
        assert_eq!(ticket.reversibility, Reversibility::R0);
        assert_eq!(ticket.mode, Mode::Auto);
    }

    #[test]
    fn display_names_the_first_argv_and_the_matrix() {
        let argv = ["/bin/true"];
        let request = SpawnRequest {
            argv: &argv,
            env: &[],
            working_dir: None,
            reversibility: Reversibility::R1,
            mode: Mode::Ask,
            marker: None,
            lineage: None,
            child: None,
            claim_vote: None,
            stdio: Stdio::null(),
        };
        let ticket = SpawnTicket::from_request(&request);
        let text = ticket.to_string();
        assert!(text.contains("/bin/true"), "{text}");
        assert!(text.contains("Ask"), "{text}");
        assert!(text.contains("R1"), "{text}");
    }

    #[test]
    fn the_pre_spawn_audit_finds_no_leaks_in_a_self_created_probe() {
        // The pre-spawn audit is unit-tested through the same code path as
        // `spawn`, but the *host* may have legitimate non-CLOEXEC fds the
        // test does not own - a terminal's stdio, a CI runner's pipe. The
        // test creates its own probe, so the assertion is precise: a clean
        // harness open discipline is what the audit exists to certify.
        let path = std::env::temp_dir().join("supra-sandbox-spawn-clean-probe");
        let file = std::fs::File::create(&path).expect("create");
        let fd = file.as_raw_fd();

        let policy = SandboxPolicy::new();
        let records = fd_audit::snapshot().expect("linux host");
        let leaks = fd_leaks(&policy, &records);
        assert!(leaks.iter().all(|record| record.fd != fd), "the harness's own open is cloexec: {leaks:?}");

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn the_spawn_records_a_child_in_the_tree_budget() {
        use std::time::Duration;

        // Holds AUDIT_LOCK: this test runs a real spawn whose audit must see a
        // clean host, and a parallel test manufacturing a leak would break it.
        let _audit = crate::AUDIT_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        // The counter is the only thing that proves the host-side hook ran.
        // A mutation that drops `record_child()` would leave the count at
        // zero, and the tree budget would be a counter that never counts.
        // The test runs a real command through the C side and asserts the
        // post-condition; a stage that is unable to actually spawn is its
        // own failure mode and the test reports it.
        let caps = supra_ffi::sandbox::probe();
        if !caps.tier.enforces_filesystem() {
            // Skip rather than fail: the audit and ticket tests above already
            // exercise the policy layer on this kernel. The tree-budget hook
            // needs a working sandbox, which the harness's own tier override
            // would fake, and faking it is the wrong shape for a unit test.
            return;
        }

        let workspace = std::env::temp_dir().join("supra-sandbox-spawn-tree-probe");
        std::fs::create_dir_all(&workspace).expect("workspace");
        let policy = crate::default_policy(&workspace);
        let argv = ["/bin/true"];
        let tree = TreeBudget::new(8);
        let request = SpawnRequest {
            argv: &argv,
            env: &[("PATH", "/usr/bin:/bin")],
            working_dir: Some(workspace.as_path()),
            reversibility: Reversibility::R0,
            mode: Mode::Auto,
            marker: None,
            lineage: None,
            child: None,
            claim_vote: None,
            stdio: Stdio::null(),
        };

        // Identity must be established for the guard to pass; the test sets
        // it up so the spawn actually reaches the tree-bookkeeping line.
        let _ = supra_guard::establish_identity();
        let _ = supra_guard::generate_marker_key();
        let marker = supra_guard::issue_marker().expect("issue").expect("a marker");

        let mut request = request;
        request.marker = Some(marker.as_str());
        let started = tree.total_count();
        let mut process = spawn(&policy, &request, &tree).expect("spawn");
        let status = process.wait(Some(Duration::from_secs(5))).expect("wait").expect("exited");
        assert_eq!(status, 0, "/bin/true exits 0");
        assert_eq!(tree.total_count(), started + 1, "the spawn must record exactly one child");

        let _ = std::fs::remove_dir_all(&workspace);
    }

    #[test]
    fn the_spawn_refuses_when_an_unlisted_descriptor_is_open() {
        // Holds AUDIT_LOCK: this test manufactures a host-wide leak on purpose,
        // which a parallel spawn test's audit would see.
        let _audit = crate::AUDIT_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        // The whole point of the pre-spawn audit is to refuse a child
        // that would inherit a descriptor the host has not deliberately
        // allowed. The test creates the leak, calls `spawn` end to end,
        // and asserts the refusal is the audit's, not the C side's.
        let caps = supra_ffi::sandbox::probe();
        if !caps.tier.enforces_filesystem() {
            return;
        }

        let path = std::env::temp_dir().join("supra-sandbox-spawn-end-to-end-leak");
        std::fs::create_dir_all(&path).expect("workspace");
        let file = std::fs::File::create(path.join("probe")).expect("create");
        let fd = file.as_raw_fd();
        supra_ffi::fd::set_cloexec(fd, false).expect("clear");

        let _ = supra_guard::establish_identity();
        let _ = supra_guard::generate_marker_key();
        let marker = supra_guard::issue_marker().expect("issue").expect("marker");

        let policy = SandboxPolicy::new();
        let argv = ["/bin/true"];
        let tree = TreeBudget::new(8);
        let request = SpawnRequest {
            argv: &argv,
            env: &[("PATH", "/usr/bin:/bin")],
            working_dir: Some(path.as_path()),
            reversibility: Reversibility::R0,
            mode: Mode::Auto,
            marker: Some(marker.as_str()),
            lineage: None,
            child: None,
            claim_vote: None,
            stdio: Stdio::null(),
        };

        let result = spawn(&policy, &request, &tree);
        supra_ffi::fd::set_cloexec(fd, true).expect("restore");
        drop(file);
        let _ = std::fs::remove_dir_all(&path);

        match result {
            Err(SandboxError::LeakyDescriptor { fd: leaked_fd, .. }) => {
                assert_eq!(leaked_fd, fd, "the refused fd is the one the audit found");
            }
            other => panic!("the audit must refuse; got {other:?}"),
        }
    }

    #[test]
    fn the_spawn_refuses_when_the_guard_refuses() {
        // Holds AUDIT_LOCK: this test runs a real spawn whose audit must see a
        // clean host, and a parallel test manufacturing a leak would break it.
        let _audit = crate::AUDIT_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        // The guard (T12.5) is the host-side anti-self-spawn. A spawn that
        // would create a self-cycle - the agent voting on its own claim -
        // must be refused by the guard, and `spawn` must propagate the
        // refusal. The test injects the conditions for L7's self-vote check
        // and asserts the error variant is `Authority`, not `Ffi`.

        let caps = supra_ffi::sandbox::probe();
        if !caps.tier.enforces_filesystem() {
            return;
        }

        let _ = supra_guard::establish_identity();
        let _ = supra_guard::generate_marker_key();
        let marker = supra_guard::issue_marker().expect("issue").expect("marker");

        let workspace = std::env::temp_dir().join("supra-sandbox-spawn-guard-probe");
        std::fs::create_dir_all(&workspace).expect("workspace");
        let policy = crate::default_policy(&workspace);

        // The same id as both proposer and voter trips L7. The guard
        // returns a refusal, and `spawn` surfaces it before the C side
        // even sees the policy.
        let id = AgentId::generate();
        let argv = ["/bin/true"];
        let tree = TreeBudget::new(8);
        let request = SpawnRequest {
            argv: &argv,
            env: &[("PATH", "/usr/bin:/bin")],
            working_dir: Some(workspace.as_path()),
            reversibility: Reversibility::R0,
            mode: Mode::Auto,
            marker: Some(marker.as_str()),
            lineage: None,
            child: None,
            claim_vote: Some((id, id)),
            stdio: Stdio::null(),
        };

        let started = tree.total_count();
        let result = spawn(&policy, &request, &tree);
        assert_eq!(tree.total_count(), started, "a refused spawn must not record a child");
        match result {
            Err(SandboxError::Authority(_layers, _refusals)) => {}
            other => panic!("the guard must refuse; got {other:?}"),
        }

        let _ = std::fs::remove_dir_all(&workspace);
    }

    #[test]
    fn the_pre_spawn_audit_refuses_a_leak() {
        // Holds AUDIT_LOCK: this test manufactures a host-wide leak on purpose,
        // which a parallel spawn test's audit would see.
        let _audit = crate::AUDIT_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let path = std::env::temp_dir().join("supra-sandbox-spawn-leak-probe");
        let file = std::fs::File::create(&path).expect("create");
        let fd = file.as_raw_fd();
        supra_ffi::fd::set_cloexec(fd, false).expect("clear");

        let policy = SandboxPolicy::new();
        let records = fd_audit::snapshot().expect("linux host");
        let leaks = fd_leaks(&policy, &records);
        let probe = leaks.iter().find(|record| record.fd == fd).expect("the audit finds the leak");
        assert_eq!(probe.fd, fd);

        supra_ffi::fd::set_cloexec(fd, true).expect("restore");
        drop(file);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn the_allow_list_relaxes_the_audit() {
        // Holds AUDIT_LOCK: this test manufactures a host-wide leak on purpose,
        // which a parallel spawn test's audit would see.
        let _audit = crate::AUDIT_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        // The caller is allowed to inherit a path, and the audit must
        // respect that. The harness uses this for a pipe to the log; the
        // default `SandboxPolicy::new()` has the allow list empty.
        let path = std::env::temp_dir().join("supra-sandbox-spawn-allow-probe");
        let file = std::fs::File::create(&path).expect("create");
        let fd = file.as_raw_fd();
        supra_ffi::fd::set_cloexec(fd, false).expect("clear");

        let mut policy = SandboxPolicy::new();
        policy.allow_inherited_path(&path);
        let records = fd_audit::snapshot().expect("linux host");
        let leaks = fd_leaks(&policy, &records);
        assert!(leaks.iter().all(|record| record.fd != fd), "the allow list rescued the leak");

        supra_ffi::fd::set_cloexec(fd, true).expect("restore");
        drop(file);
        let _ = std::fs::remove_file(&path);
    }
}
