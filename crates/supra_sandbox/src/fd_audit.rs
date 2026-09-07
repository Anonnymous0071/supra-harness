//! Pre-spawn descriptor audit.
//!
//! The T4 notes bind T16 directly: "Not isolation from already-open
//! descriptors. Anything inherited across `exec` stays usable. The caller
//! must close what it does not intend to pass." The sandbox's filesystem
//! policy is meaningless if the child can read a host pipe it has no
//! business with, so this audit is a hard gate, not a hint.
//!
//! Two things matter and only two:
//!
//! 1. The descriptor number, because the call site must identify which open
//!    it was. The audit walks `/proc/self/fd` rather than the C-level `fcntl`
//!    enumeration, because the host may have fds that were not created through
//!    `supra_ffi::fd` - a `tokio` reactor pipe, a `rusqlite` WAL handle, a
//!    `tracing-subscriber` sink. Each of those has a fixed fd number, and
//!    auditing by number catches all of them.
//! 2. The `FD_CLOEXEC` flag, because the policy is "every fd that survives an
//!    `exec` must have been deliberately allowed to". A `fcntl` read is
//!    cheap, but a sweep across thousands of fds on a busy agent must not
//!    become a measurable cost. The read is `O(fd_count)` and the flag is
//!    one bit; both are well below the per-turn budget.
//!
//! What the audit does **not** do: it does not close anything, because a
//! library that closed a host descriptor out from under its caller would
//! corrupt the host. The policy is "do not spawn a child that would inherit
//! a leaking fd"; the gate is the `Refuse` on the way to the child, not the
//! `close` on the way out.

use std::io;
use std::path::Path;

use supra_ffi::fd::cloexec_flag;

/// `EBADF`: the only `errno` `fcntl(F_GETFD)` returns for a descriptor that
/// no longer exists. POSIX-fixed at 9 on every platform this crate builds
/// on (Linux, macOS); declared by hand for the same reason every other
/// foreign constant in the workspace is - no `libc` dependency to carry for
/// one number.
const EBADF: i32 = 9;

/// One descriptor the audit observed.
///
/// `path` is best-effort: a descriptor that has been unlinked can resolve to
/// something like `/tmp/supra-XXXXXX (deleted)`, which the call site still
/// needs to see, and a chroot can resolve to a name the host has no
/// permission to read. `path` is what `/proc/self/fd/N` says, verbatim.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DescriptorRecord {
    /// Descriptor number, as the host sees it.
    pub fd: i32,
    /// What the descriptor resolves to right now. May be the empty string if
    /// `/proc` was unparsable; that still counts as "a descriptor is open".
    pub path: String,
    /// Whether the descriptor will close at the next `exec`.
    pub cloexec: bool,
}

/// Walk `/proc/self/fd`, read each `FD_CLOEXEC` flag, and return one record
/// per open descriptor.
///
/// Non-Linux hosts refuse, not skip: a sweep that cannot run is the same as
/// a sweep that found a leak, because either way the caller cannot prove
/// safety. `ProcfsAbsent` is the message.
///
/// # Errors
///
/// `io::ErrorKind::Unsupported` when the host has no `/proc/self/fd` to read.
/// `io::Error` from the underlying directory walk.
pub fn snapshot() -> io::Result<Vec<DescriptorRecord>> {
    snapshot_in(Path::new("/proc/self/fd"))
}

fn snapshot_in(directory: &Path) -> io::Result<Vec<DescriptorRecord>> {
    let entries = match std::fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "ProcfsAbsent: cannot enumerate host descriptors without /proc/self/fd",
            ));
        }
        Err(error) => return Err(error),
    };

    let mut records = Vec::new();
    for entry in entries {
        let entry = entry?;
        let file_name = entry.file_name();
        let Some(raw) = file_name.to_str() else { continue };
        // Only the integer entries: `.`, `..`, and any future sysfs-style
        // extras are silently ignored. fd numbers are 0..=i32::MAX, and
        // `i32::from_str` rejects everything else for free.
        let Ok(fd) = raw.parse::<i32>() else { continue };

        // Read the symlink target before the flag. A descriptor that
        // vanished between the directory read and this readlink is gone:
        // a closed descriptor cannot cross an exec, so it is not a leak -
        // it is not present at all. Under a concurrent thread (the TUI's
        // reader closing a session while the turn loop audits) this race
        // is normal operation, not an error.
        let path = match std::fs::read_link(entry.path()) {
            Ok(target) => target.to_string_lossy().into_owned(),
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(_) => String::new(),
        };
        // `fcntl(F_GETFD)` on a live descriptor cannot fail except EBADF,
        // which here means the descriptor closed mid-walk - the same
        // verdict as the vanished symlink above. Any other failure (none
        // is known for `F_GETFD` on a live fd) keeps the fail-closed
        // default below.
        let cloexec = match cloexec_flag(fd) {
            Ok(flag) => flag,
            Err(error) if error.raw_os_error() == Some(EBADF) => continue,
            Err(_) => false,
        };
        records.push(DescriptorRecord { fd, path, cloexec });
    }
    records.sort_by_key(|record| record.fd);
    Ok(records)
}

/// Find every descriptor without `FD_CLOEXEC`.
///
/// `allow` is consulted first: a descriptor that the harness has explicitly
/// whitelisted (a pipe the agent needs to inherit, a log file that must
/// survive the child's re-exec) is not a leak. The list is what a future
/// T29 status line will show - "5 inherited, 0 leaking" - and the size of
/// both halves is a budget the operator can watch.
///
/// `audit` is a [path -> bool] predicate rather than a list of fds because
/// descriptors are renumbered as the host opens and closes them, and a
/// number-based allow-list is the wrong shape: a freshly-opened pipe might
/// land on an fd that *was* leaked and has since been closed. The path is
/// the stable thing.
///
/// Standard streams (fds 0, 1, 2) are not leaks even when the kernel
/// reports them without CLOEXEC. A TTY-attached harness hands them to the
/// child so the child can talk to the user; a CI runner hands the child
/// a pipe with no path the audit can match. Either way, the *only*
/// descriptors that matter for the spawn boundary are the ones above
/// stdin/stdout/stderr.
pub fn find_leaks<F>(records: &[DescriptorRecord], allow: F) -> Vec<&DescriptorRecord>
where
    F: Fn(&str) -> bool,
{
    records.iter().filter(|record| record.fd > 2 && !record.cloexec && !allow(&record.path)).collect()
}

/// A descriptor record whose path can be converted to a stable name.
///
/// `/proc/self/fd/N` for a deleted file becomes something like
/// `/tmp/supra-XXXXXX (deleted)`. The audit wants to print that verbatim for
/// the operator; the `path` field already holds the text. There is no
/// further normalisation.
#[must_use]
pub fn path_of(record: &DescriptorRecord) -> &str {
    &record.path
}

#[cfg(test)]
mod tests {
    use std::io::Write as _;
    use std::os::fd::AsRawFd as _;

    use super::*;

    fn path_for(records: &[DescriptorRecord], fd: i32) -> Option<&str> {
        records.iter().find(|record| record.fd == fd).map(|record| record.path.as_str())
    }

    #[test]
    fn the_audit_observes_an_open_descriptor() {
        // The host opens this test binary's own source through `include!` on
        // every compile, so at least one descriptor is reachable through
        // `/proc/self/fd` whenever the test runs. The audit must read it.
        let records = snapshot().expect("linux host");
        assert!(!records.is_empty(), "the host has at least one open descriptor");

        // The current process should be visible. A `tokio` reactor or a
        // tracing sink may also be visible; the test asserts only on what
        // is guaranteed.
        let own = path_for(&records, 0).or_else(|| path_for(&records, 1)).or_else(|| path_for(&records, 2));
        assert!(own.is_some(), "stdin, stdout, or stderr is open: {records:?}");
    }

    #[test]
    fn a_descriptor_with_cloexec_is_not_a_leak() {
        // A test that ran from a terminal would see stdin/stdout/stderr
        // without CLOEXEC - the terminal driver holds them, and the harness
        // does not own them. The audit's *unit* test creates its own
        // descriptor and asserts on that: the harness's open discipline is
        // what we control, and std is what enforces it.
        let path = std::env::temp_dir().join("supra-sandbox-fd-cloexec-probe");
        let _ = std::fs::File::create(&path).expect("create");
        let file = std::fs::File::create(&path).expect("create");
        let fd = file.as_raw_fd();

        // std sets CLOEXEC by default; the audit must read it back as true.
        assert!(cloexec_flag(fd).expect("read flag"), "std opens with O_CLOEXEC");

        let records = snapshot().expect("linux host");
        let probe = records.iter().find(|record| record.fd == fd).expect("the audit sees its own probe");
        assert!(probe.cloexec, "the probe is not a leak: {probe:?}");

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn a_descriptor_without_cloexec_is_reported_as_a_leak() {
        // Holds AUDIT_LOCK: this test manufactures a host-wide leak on
        // purpose, and a parallel spawn test's audit would see it.
        let _audit = crate::AUDIT_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        // std sets CLOEXEC by default, so the only way to *create* a leak
        // is to clear the flag. The audit must then find it. The leak
        // descriptor is closed at end of scope to keep the test self-clean.
        let path = std::env::temp_dir().join("supra-sandbox-fd-audit-probe");
        let file = std::fs::File::create(&path).expect("create");
        let fd = file.as_raw_fd();
        supra_ffi::fd::set_cloexec(fd, false).expect("clear");

        let records = snapshot().expect("linux host");
        let leaks = find_leaks(&records, |_| false);
        let probe = leaks.iter().find(|record| record.fd == fd);
        assert!(probe.is_some(), "the cleared flag must surface: {leaks:?}");

        // The allow predicate rescues a whitelisted path. The probe is
        // whitelisted by its real path; the search must come back empty.
        let path_text = path.to_string_lossy().into_owned();
        let allow = |candidate: &str| candidate == path_text;
        let leaks = find_leaks(&records, allow);
        assert!(leaks.iter().all(|record| record.fd != fd), "the allow list rescued the probe");

        supra_ffi::fd::set_cloexec(fd, true).expect("restore");
        drop(file);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn a_vanished_descriptor_is_absent_not_a_leak() {
        // A directory entry whose fd no longer exists must be skipped, not
        // reported as a leak: a closed descriptor cannot cross an exec, so
        // "vanished mid-walk" is absence, not danger. The probe walks a
        // synthetic fd directory whose one entry names an impossibly high
        // descriptor number - `cloexec_flag` answers EBADF, exactly as it
        // would for a descriptor another thread closed while this walk ran.
        // Under the old `unwrap_or(false)` shape this returned a record
        // with `cloexec: false` and an empty path, which `find_leaks`
        // reported as a leak - the parallel-test flake this behaviour
        // exists to prevent.
        let dir = std::env::temp_dir().join("supra-sandbox-fd-vanished-probe");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("dir");
        std::fs::write(dir.join("999999"), b"x").expect("entry");

        let records = snapshot_in(&dir).expect("walk");
        assert!(records.is_empty(), "a vanished fd is absent: {records:?}");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_unreadable_procfs_is_unsupported() {
        // The audit must refuse, not skip, on a host without /proc - a
        // sweep that cannot run is the same as a sweep that found a leak.
        let error = snapshot_in(Path::new("/nonexistent-supra-sandbox-proc")).expect_err("absent");
        assert_eq!(error.kind(), io::ErrorKind::Unsupported);
        assert!(error.to_string().contains("ProcfsAbsent"));
    }

    #[test]
    fn procfs_records_carry_a_path_for_diagnostics() {
        // The audit's user is the operator: when a leak is reported, the
        // path is the first thing they see. A descriptor with no name is
        // still a leak, but the message should say "fd 12 (unreadable)"
        // rather than "fd 12 ()".
        let path = std::env::temp_dir().join("supra-sandbox-fd-named-probe");
        let mut file = std::fs::File::create(&path).expect("create");
        file.write_all(b"x").expect("write");
        let fd = file.as_raw_fd();

        let records = snapshot().expect("linux host");
        let record = records.iter().find(|record| record.fd == fd).expect("the audit sees its own probe");
        assert!(!record.path.is_empty(), "the probe has a name: {record:?}");
    }
}
