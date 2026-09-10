//! Pseudo-terminal primitives.
//!
//! T16.5 owns persistent shell sessions; the kernel facility underneath is a
//! pty pair. This module is the confined `unsafe` for pty allocation and the
//! window-size `ioctl`, shaped so everything above it - the session, the
//! output shaping, the sandbox composition - is safe code.
//!
//! Hygiene comes first, because the T4 note binds T16.5 directly: "descriptors
//! inherited across `exec` remain usable. T16 and T16.5 must close
//! descriptors they do not intend to pass."
//!
//! - The **master** fd is CLOEXEC. It belongs to the harness; a child that
//!   inherited it could read the session's output or inject keystrokes.
//! - The **slave** fd is CLOEXEC too. The session passes it to a child as
//!   stdio through the sandbox `Command::stdin/stdout/stderr` borrows, and
//!   `dup2` onto 0/1/2 clears CLOEXEC on the *duplicate*, so the child keeps
//!   working standard streams. Any exec that did not go through the sandbox
//!   loses the fd - which is the point.
//!
//! Both flags are set **at the syscall that creates the descriptor**, not
//! after: the master comes from `posix_openpt(O_CLOEXEC | ...)` and the slave
//! from `open(path, O_CLOEXEC | ...)`. The seemingly simpler `openpty(3)`
//! returns both descriptors with the flag clear, which leaves a window in
//! which a concurrent fd audit (T16 runs before every spawn, sessions open
//! from the TUI's threads) would observe this pair as unflagged - a false
//! leak under exactly the load the harness is built for. CLOEXEC from birth
//! closes the window by construction.
//!
//! Both ends are held as [`std::fs::File`], so ownership, drop-closing, and
//! `Send`/`Sync` come from std rather than a custom wrapper. The parent's
//! slave copy stays open until [`Pty::take_slave`] is called; the session
//! takes it right after spawn, because a parent-held slave would keep the
//! pair alive after the child exits and master reads would never see EOF.

use std::ffi::CString;
use std::fs::File;
use std::io;
use std::os::fd::{AsRawFd as _, FromRawFd, RawFd};

use crate::sys;

// open(2) flag values for the two allocation calls. These differ between
// kernels (Linux keeps the historical low bits, macOS the `0x10000` block),
// so they are pinned per platform next to their only use.
#[cfg(target_os = "linux")]
const O_RDWR: core::ffi::c_int = 0x2;
#[cfg(target_os = "linux")]
const O_NOCTTY: core::ffi::c_int = 0x100;
#[cfg(target_os = "linux")]
const O_CLOEXEC: core::ffi::c_int = 0x80000;
#[cfg(target_os = "macos")]
const O_RDWR: core::ffi::c_int = 0x2;
#[cfg(target_os = "macos")]
const O_NOCTTY: core::ffi::c_int = 0x20000;
#[cfg(target_os = "macos")]
const O_CLOEXEC: core::ffi::c_int = 0x10000;

/// The visible size of a pty, in cells.
///
/// Pixel fields exist in the kernel structure but carry no meaning for this
/// harness's renderer, which works in cells; they are pinned to zero.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PtySize {
    /// Rows of text.
    pub rows: u16,
    /// Columns of text.
    pub cols: u16,
}

impl PtySize {
    /// The size the session opens with when the caller has no better answer.
    ///
    /// 80x24 is the historical default terminal geometry; a session is
    /// resized to the real viewport before the child's first prompt matters.
    #[must_use]
    pub const fn default_size() -> Self {
        Self { rows: 24, cols: 80 }
    }
}

/// The kernel's `winsize`, laid out for the two `ioctl` requests.
///
/// Four `u16` fields, POSIX-stable on every platform this crate builds on;
/// declared here rather than in `sys` because only this module's two calls
/// touch it.
#[repr(C)]
#[derive(Clone, Copy, Default)]
#[allow(clippy::struct_field_names)] // the kernel names the fields `ws_*`; renaming them would lie about the ABI
struct Winsize {
    ws_row: u16,
    ws_col: u16,
    ws_xpixel: u16,
    ws_ypixel: u16,
}

// TIOCSWINSZ / TIOCGWINSZ set and read the window size. The values differ
// between kernels: Linux uses the `0x54xx` terminal block, macOS the
// `_IOW('t', ...)`/`_IOR('t', ...)` encodings. Both constants are
// platform-stable and documented in the respective headers.
#[cfg(target_os = "linux")]
const TIOCSWINSZ: core::ffi::c_ulong = 0x5414;
#[cfg(target_os = "linux")]
const TIOCGWINSZ: core::ffi::c_ulong = 0x5413;
#[cfg(target_os = "macos")]
const TIOCSWINSZ: core::ffi::c_ulong = 0x4008_7467;
#[cfg(target_os = "macos")]
const TIOCGWINSZ: core::ffi::c_ulong = 0x4008_7466;

// A third pty-capable kernel would need its own flag and ioctl values;
// failing the build here is cheaper than failing a spawn at runtime on a
// platform nobody validated. The whole module is Unix-only, so the gate
// reads `unix and not linux/macos`: a `not(any(linux, macos))` would
// misfire on Windows, which compiles the crate but not this module.
#[cfg(all(unix, not(any(target_os = "linux", target_os = "macos"))))]
compile_error!("supra_ffi::pty pins Linux and macOS constants; add this platform before building it");

/// A master/slave pty pair.
///
/// The master is the harness's end: reading it yields what the child wrote,
/// writing it delivers what the user typed. The slave is the child's end -
/// the session hands its raw fd to the sandbox spawn as the child's stdio,
/// then takes it out of this struct so the parent's copy closes.
#[derive(Debug)]
pub struct Pty {
    master: File,
    slave: Option<File>,
}

impl Pty {
    /// Open a pty pair at `size`.
    ///
    /// Both descriptors are CLOEXEC from the syscall that creates them, per
    /// the module hygiene contract above. `resize` is applied immediately,
    /// so the child's first `TIOCGWINSZ` sees the requested geometry rather
    /// than a stale default.
    ///
    /// # Errors
    ///
    /// [`io::Error`] from `posix_openpt`, `grantpt`, `unlockpt`, the slave
    /// name lookup, the slave `open`, or the initial resize. A failure here
    /// leaves no descriptor behind: each fd is taken into a `File` the
    /// moment it exists, so every error path drops what was opened.
    #[cfg(unix)]
    pub fn open(size: PtySize) -> io::Result<Self> {
        // SAFETY: fixed open(2) flag values; the call allocates the master
        // descriptor and returns it or -1.
        let master_fd = unsafe { sys::posix_openpt(O_RDWR | O_NOCTTY | O_CLOEXEC) };
        if master_fd < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: `master_fd` was just returned by a successful
        // `posix_openpt`; it is owned exactly once by this function, wrapped
        // immediately, and never read as a raw fd again except through the
        // File.
        let master_file = unsafe { File::from_raw_fd(master_fd) };

        // grantpt/unlockpt prepare the slave for use; without them the
        // slave open below fails or the first read does.
        //
        // SAFETY: the master descriptor is live and owned by this function.
        if unsafe { sys::grantpt(master_fd) } != 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: as `grantpt`.
        if unsafe { sys::unlockpt(master_fd) } != 0 {
            return Err(io::Error::last_os_error());
        }

        let name = slave_name(master_fd)?;
        let c_name = CString::new(name.as_str())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "slave path is not UTF-8-clean"))?;
        // SAFETY: `c_name` is a live NUL-terminated path; the flags are the
        // same fixed open(2) values as the master.
        let slave_fd = unsafe { sys::open(c_name.as_ptr(), O_RDWR | O_NOCTTY | O_CLOEXEC) };
        if slave_fd < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: as the master - a fresh descriptor from a successful
        // `open`, wrapped immediately.
        let slave_file = unsafe { File::from_raw_fd(slave_fd) };

        let pty = Self { master: master_file, slave: Some(slave_file) };
        pty.resize(size)?;
        Ok(pty)
    }

    /// Open a pty pair at `size`. See [`Pty::open`].
    ///
    /// Non-Unix hosts have no pty facility in this stage; the refusal is
    /// explicit rather than absent, so callers fail closed at the same call
    /// site on every platform.
    #[cfg(not(unix))]
    pub fn open(_size: PtySize) -> io::Result<Self> {
        Err(io::Error::new(io::ErrorKind::Unsupported, "pseudo-terminals need a POSIX platform"))
    }

    /// The master end, for reading output and writing input.
    #[must_use]
    pub fn master(&self) -> &File {
        &self.master
    }

    /// A second handle to the master end.
    ///
    /// The reader and the writer belong on different threads in the TUI, and
    /// std's `File::try_clone` shares the underlying descriptor, so what one
    /// writes the other's peer sees.
    ///
    /// # Errors
    ///
    /// [`io::Error`] from `dup`.
    pub fn try_clone_master(&self) -> io::Result<File> {
        self.master.try_clone()
    }

    /// The slave fd, to pass to a child as stdio.
    ///
    /// Borrowed, not consumed: the child's spawn dup2s it onto 0/1/2, and the
    /// parent's copy stays open until [`Pty::take_slave`]. Returns `None`
    /// once taken.
    #[must_use]
    pub fn slave_fd(&self) -> Option<RawFd> {
        self.slave.as_ref().map(File::as_raw_fd)
    }

    /// Take the parent's slave descriptor out and close it by dropping.
    ///
    /// The session calls this right after a successful spawn: the child holds
    /// its own copies on 0/1/2, and a parent-held slave would keep the pair
    /// alive after the child exits, so master reads would never see EOF.
    pub fn take_slave(&mut self) {
        self.slave = None;
    }

    /// Resize the terminal.
    ///
    /// The kernel signals the foreground process group (`SIGWINCH`); a child
    /// that cares redraws.
    ///
    /// # Errors
    ///
    /// [`io::Error`] from the `ioctl`.
    #[cfg(unix)]
    pub fn resize(&self, size: PtySize) -> io::Result<()> {
        let winsize = Winsize { ws_row: size.rows, ws_col: size.cols, ws_xpixel: 0, ws_ypixel: 0 };
        // SAFETY: the master fd is open (owned by this struct); the request
        // code and the pointer match the documented `TIOCSWINSZ` shape.
        if unsafe { sys::ioctl(self.master.as_raw_fd(), TIOCSWINSZ, &raw const winsize) } != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    /// Resize the terminal. See [`Pty::resize`].
    #[cfg(not(unix))]
    pub fn resize(&self, _size: PtySize) -> io::Result<()> {
        Err(io::Error::new(io::ErrorKind::Unsupported, "pseudo-terminals need a POSIX platform"))
    }

    /// Read the window size back from the kernel.
    ///
    /// Exists so tests can assert that a resize landed, and so a caller can
    /// detect an externally-forced geometry change.
    ///
    /// # Errors
    ///
    /// [`io::Error`] from the `ioctl`.
    #[cfg(unix)]
    pub fn size(&self) -> io::Result<PtySize> {
        let mut winsize = Winsize::default();
        // SAFETY: as in `resize`; the pointer is a live local of the
        // documented `TIOCGWINSZ` shape.
        if unsafe { sys::ioctl(self.master.as_raw_fd(), TIOCGWINSZ, &raw mut winsize) } != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(PtySize { rows: winsize.ws_row, cols: winsize.ws_col })
    }

    /// Read the window size back from the kernel. See [`Pty::size`].
    #[cfg(not(unix))]
    pub fn size(&self) -> io::Result<PtySize> {
        Err(io::Error::new(io::ErrorKind::Unsupported, "pseudo-terminals need a POSIX platform"))
    }
}

/// The slave's filesystem path, from the master descriptor.
///
/// Linux has `ptsname_r`, the reentrant form. Every other Unix this module
/// builds on has only `ptsname`, whose result lives in a static buffer -
/// the lock keeps concurrent opens from sharing it.
#[cfg(all(unix, target_os = "linux"))]
fn slave_name(master_fd: RawFd) -> io::Result<String> {
    let mut buffer = [0u8; 64];
    // SAFETY: `buffer` is a live, correctly sized destination for the
    // documented `ptsname_r` shape.
    if unsafe { sys::ptsname_r(master_fd, buffer.as_mut_ptr().cast(), buffer.len()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    let end = buffer.iter().position(|&byte| byte == 0).unwrap_or(buffer.len());
    Ok(String::from_utf8_lossy(&buffer[..end]).into_owned())
}

/// See the Linux `slave_name` above.
#[cfg(all(unix, not(target_os = "linux")))]
fn slave_name(master_fd: RawFd) -> io::Result<String> {
    use std::sync::Mutex;
    use std::sync::PoisonError;

    static PTSNAME: Mutex<()> = Mutex::new(());
    let _guard = PTSNAME.lock().unwrap_or_else(PoisonError::into_inner);
    // SAFETY: the master descriptor is live; the static result buffer is
    // protected by `PTSNAME` for the duration of the read.
    let pointer = unsafe { sys::ptsname(master_fd) };
    if pointer.is_null() {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: on success `ptsname` returns a NUL-terminated string.
    let bytes = unsafe { core::ffi::CStr::from_ptr(pointer) }.to_bytes();
    Ok(String::from_utf8_lossy(bytes).into_owned())
}

#[cfg(test)]
mod tests {
    use std::io::{Read as _, Write as _};

    use super::*;

    /// Open a pair, or skip loudly when the host has no pty device.
    ///
    /// GitHub's macOS runners report `ENXIO` ("Inappropriate ioctl") from
    /// `posix_openpt`: the sandbox allows the call shape but the host
    /// exposes no device. A skip names the cause; a pass would verify
    /// nothing, and a failure would blame the code for the room it runs in.
    fn open_or_skip(size: PtySize) -> Option<Pty> {
        match Pty::open(size) {
            Ok(pty) => Some(pty),
            Err(error) if error.raw_os_error() == Some(25) => {
                eprintln!("skipped: no pty device on this host ({error})");
                None
            }
            Err(error) => panic!("openpty on a pty-capable host: {error}"),
        }
    }

    #[test]
    fn both_ends_are_cloexec_from_birth() {
        let Some(pty) = open_or_skip(PtySize::default_size()) else { return };

        // The module contract: a child must never inherit either end. The
        // audit (T16) relies on this - an fd without CLOEXEC that is not on
        // the allow list refuses every spawn.
        assert!(
            crate::fd::cloexec_flag(pty.master.as_raw_fd()).expect("master flag"),
            "the master must be CLOEXEC"
        );
        let slave_fd = pty.slave_fd().expect("slave present before take");
        assert!(crate::fd::cloexec_flag(slave_fd).expect("slave flag"), "the slave must be CLOEXEC");
    }

    #[test]
    fn resize_round_trips_through_the_kernel() {
        let Some(pty) = open_or_skip(PtySize { rows: 10, cols: 40 }) else { return };
        assert_eq!(pty.size().expect("size"), PtySize { rows: 10, cols: 40 }, "the open-time size");

        pty.resize(PtySize { rows: 33, cols: 100 }).expect("resize");
        assert_eq!(pty.size().expect("size"), PtySize { rows: 33, cols: 100 }, "the resized size");
    }

    #[test]
    fn master_writes_reach_the_slave_side() {
        // The pair must actually carry bytes. The slave is not yet taken, so
        // the test reads through it directly: write on the master, read on
        // the slave. This is the property every session depends on.
        //
        // The slave's line discipline is canonical by default (ICANON), so
        // reads deliver whole lines and only after a newline. The writes
        // carry `\n` on purpose, and the assertions expect it back. Canonical
        // mode also echoes what the master writes back to the master, so the
        // test drains that echo before exercising the reverse direction.
        let Some(mut pty) = open_or_skip(PtySize::default_size()) else { return };
        let mut slave = pty.slave.take().expect("slave present");

        pty.master().write_all(b"ping\n").expect("write master");

        let mut buffer = [0u8; 5];
        slave.read_exact(&mut buffer).expect("read slave");
        assert_eq!(&buffer, b"ping\n", "the master's bytes crossed the pair");

        // The echo of "ping\n" sits in the master's read buffer, and the
        // slave's output processing (ONLCR) renders it as CRLF - the same
        // byte stream a terminal would have drawn. Consume it so the
        // reverse-direction assertion reads only its own payload.
        let mut echo = [0u8; 6];
        pty.master().read_exact(&mut echo).expect("drain echo");
        assert_eq!(&echo, b"ping\r\n", "the line discipline echoed the write back");

        // And the reverse direction, because the session writes input the
        // same way the child reads it. Output processing applies here too,
        // so the newline arrives as CRLF.
        slave.write_all(b"pong\n").expect("write slave");
        let mut back = [0u8; 6];
        pty.master().read_exact(&mut back).expect("read master");
        assert_eq!(&back, b"pong\r\n", "the slave's bytes crossed the pair");
    }

    #[test]
    fn take_slave_closes_the_parent_copy() {
        let Some(mut pty) = open_or_skip(PtySize::default_size()) else { return };
        let slave_fd = pty.slave_fd().expect("slave present");

        pty.take_slave();
        assert!(pty.slave_fd().is_none(), "taken means taken");

        // The descriptor must be gone from the process, not merely detached
        // from the struct. Probed with fcntl immediately after the drop:
        // nothing allocates or opens between the close and this call, so the
        // number cannot have been reused yet. (A `read_dir` probe would be
        // racy - its own dirfd can take the recycled number.) A leaked fd
        // here keeps the pty alive after the child exits and starves the
        // session's EOF detection.
        assert!(crate::fd::cloexec_flag(slave_fd).is_err(), "fd {slave_fd} must be closed after take_slave");
    }

    #[test]
    fn two_pairs_are_independent() {
        let Some(first) = open_or_skip(PtySize { rows: 11, cols: 44 }) else { return };
        let Some(second) = open_or_skip(PtySize { rows: 22, cols: 55 }) else { return };

        assert_ne!(first.master().as_raw_fd(), second.master().as_raw_fd());
        assert_ne!(first.size().expect("size"), second.size().expect("size"));
    }
}
