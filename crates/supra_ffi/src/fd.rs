//! Descriptor hygiene primitives.
//!
//! The T4 notes bind T16 and T16.5 directly: "descriptors inherited across
//! `exec` remain usable. T16 and T16.5 must close descriptors they do not
//! intend to pass." What "close" means here is `FD_CLOEXEC` — the flag that
//! ends a descriptor at the next `exec` without disturbing the host process
//! that owns it. A library must never `close()` a descriptor out from under
//! its caller; clearing the flag is the one mechanism that is always safe.
//!
//! This module is the confined `unsafe` the T16 policy layer calls. The
//! *policy* — which descriptors may cross `exec`, and what happens when one
//! that may not turns up — lives above, in `supra_sandbox`. Enumeration
//! (`/proc/self/fd`) needs no `unsafe` at all and therefore does not live
//! here either.
//!
//! `fcntl` is declared by hand rather than through a `libc` dependency, like
//! every other foreign call in this crate. `F_GETFD`, `F_SETFD`, and
//! `FD_CLOEXEC` are POSIX-fixed values; they are declared next to the call
//! rather than in `sys`, because they describe this one primitive, not a
//! library ABI.

use std::io;

use crate::sys;

/// Read whether `fd` will close at the next `exec`.
///
/// # Errors
///
/// [`io::Error`] from the underlying `fcntl`, most usefully `EBADF` for a
/// descriptor that does not exist.
#[cfg(unix)]
pub fn cloexec_flag(fd: core::ffi::c_int) -> io::Result<bool> {
    // SAFETY: `fcntl` with `F_GETFD` takes a descriptor and a fixed command;
    // both are valid values, and the call touches no memory it does not own.
    let flags = unsafe { sys::fcntl(fd, sys::F_GETFD) };
    if flags < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(flags & sys::FD_CLOEXEC != 0)
}

/// Set whether `fd` closes at the next `exec`.
///
/// Clearing the flag is exposed because tests need a way to *create* the leak
/// the policy layer exists to fix — std opens every descriptor it creates with
/// `O_CLOEXEC`, so there is no other way to produce one. Production policy
/// code must never clear the flag; `scripts/check-invariants.sh` fails a build
/// where shipped code does.
///
/// # Errors
///
/// [`io::Error`] from the underlying `fcntl`.
#[cfg(unix)]
pub fn set_cloexec(fd: core::ffi::c_int, cloexec: bool) -> io::Result<()> {
    // SAFETY: `fcntl` with `F_GETFD` touches no memory beyond its arguments.
    let flags = unsafe { sys::fcntl(fd, sys::F_GETFD) };
    if flags < 0 {
        return Err(io::Error::last_os_error());
    }
    let flags = if cloexec { flags | sys::FD_CLOEXEC } else { flags & !sys::FD_CLOEXEC };
    // SAFETY: `F_SETFD` writes the descriptor's own flag word; `flags` came
    // from `F_GETFD` on the same descriptor.
    let written = unsafe { sys::fcntl(fd, sys::F_SETFD, flags) };
    if written < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// POSIX platforms without this module's `fcntl` (non-Unix) refuse rather than
/// pretend: a caller that cannot even read the flag cannot guarantee hygiene,
/// and a silent `Ok` would certify a sweep that never ran.
#[cfg(not(unix))]
pub fn cloexec_flag(_fd: core::ffi::c_int) -> io::Result<bool> {
    Err(io::Error::new(io::ErrorKind::Unsupported, "descriptor flags need a POSIX platform"))
}

/// See [`cloexec_flag`].
#[cfg(not(unix))]
pub fn set_cloexec(_fd: core::ffi::c_int, _cloexec: bool) -> io::Result<()> {
    Err(io::Error::new(io::ErrorKind::Unsupported, "descriptor flags need a POSIX platform"))
}

#[cfg(test)]
mod tests {
    use std::io::Write as _;
    use std::os::fd::AsRawFd as _;

    use super::*;

    #[test]
    fn std_opens_with_cloexec_and_the_flag_round_trips() {
        let dir = std::env::temp_dir().join("supra-ffi-fd-test");
        std::fs::create_dir_all(&dir).expect("scratch");
        let path = dir.join("flag.txt");
        let mut file = std::fs::File::create(&path).expect("create");
        let fd = file.as_raw_fd();

        // std opens with O_CLOEXEC, so a fresh descriptor is already covered.
        assert!(cloexec_flag(fd).expect("read flag"));

        // Clearing must be observable...
        set_cloexec(fd, false).expect("clear");
        assert!(!cloexec_flag(fd).expect("read flag"), "the cleared flag must show");

        // ...and restoring must too, without disturbing the descriptor itself.
        set_cloexec(fd, true).expect("restore");
        assert!(cloexec_flag(fd).expect("read flag"));
        file.write_all(b"still usable").expect("the descriptor keeps working");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn invalid_descriptors_report_errors() {
        // -1 and a number no descriptor can reach both refuse; a silent Ok
        // would certify a flag that was never read.
        for fd in [-1, i32::MAX] {
            assert!(cloexec_flag(fd).is_err(), "fd {fd}");
            assert!(set_cloexec(fd, true).is_err(), "fd {fd}");
        }
    }
}
