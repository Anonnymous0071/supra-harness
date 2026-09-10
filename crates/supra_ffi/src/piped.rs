//! A poll-deadline reader for pipes.
//!
//! std offers no timeout on a pipe read: `File::set_read_timeout` is a
//! socket option, and a buffered read on a child's stdout blocks for as
//! long as the child stays silent. A hung language server or debug
//! adapter would hang the session with it. [`PipedInput`] polls the
//! descriptor before every read syscall, so a silent peer becomes a
//! [`std::io::ErrorKind::TimedOut`] error the caller can kill on, and
//! the framing callers build on top never block past their deadline.

use std::io;
use std::os::fd::{AsRawFd, RawFd};
use std::time::Duration;

/// How long one read step waits before declaring the peer silent.
pub const DEFAULT_STEP_TIMEOUT: Duration = Duration::from_secs(120);

/// A byte-stream reader over a borrowed descriptor with a per-read
/// deadline.
///
/// The descriptor is borrowed, never owned: constructing from a
/// `ChildStdout` (or any `AsRawFd`) keeps ownership where it was, and
/// dropping the reader closes nothing.
pub struct PipedInput {
    fd: RawFd,
    buffer: Vec<u8>,
    eof: bool,
    step_timeout: Duration,
}

impl PipedInput {
    /// Read from `source`'s descriptor with the default deadline.
    #[must_use]
    pub fn new<R: AsRawFd>(source: &R) -> Self {
        Self { fd: source.as_raw_fd(), buffer: Vec::new(), eof: false, step_timeout: DEFAULT_STEP_TIMEOUT }
    }

    /// How long each read step waits.
    #[must_use]
    pub const fn step_timeout(&self) -> Duration {
        self.step_timeout
    }

    /// Change the per-read deadline.
    pub fn set_step_timeout(&mut self, timeout: Duration) {
        self.step_timeout = timeout;
    }

    /// Whether the peer has closed its end.
    #[must_use]
    pub const fn at_eof(&self) -> bool {
        self.eof
    }

    /// Read up to and including the next `byte`, appending to `out`.
    ///
    /// Returns how many bytes were appended; `0` means the peer closed
    /// without the byte arriving, with whatever tail it did send left
    /// in `out` for the caller to judge.
    ///
    /// # Errors
    ///
    /// [`std::io::ErrorKind::TimedOut`] when the deadline passes with
    /// no data; whatever the descriptor otherwise reports.
    pub fn read_until(&mut self, byte: u8, out: &mut Vec<u8>) -> io::Result<usize> {
        let start = out.len();
        loop {
            if let Some(position) = self.buffer.iter().position(|candidate| *candidate == byte) {
                out.extend_from_slice(&self.buffer[..=position]);
                self.buffer.drain(..=position);
                return Ok(out.len() - start);
            }
            if self.eof || self.fill()? == 0 {
                out.append(&mut self.buffer);
                self.eof = true;
                return Ok(out.len() - start);
            }
        }
    }

    /// Read exactly `count` bytes, appending them to `out`.
    ///
    /// # Errors
    ///
    /// [`std::io::ErrorKind::UnexpectedEof`] when the peer closes
    /// before `count` bytes arrive; [`std::io::ErrorKind::TimedOut`]
    /// when the deadline passes.
    pub fn read_exact_to(&mut self, count: usize, out: &mut Vec<u8>) -> io::Result<()> {
        while self.buffer.len() < count {
            if self.eof || self.fill()? == 0 {
                self.eof = true;
                return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "the peer closed mid-frame"));
            }
        }
        out.extend_from_slice(&self.buffer[..count]);
        self.buffer.drain(..count);
        Ok(())
    }

    /// One poll-then-read step. Returns `0` at end of stream.
    fn fill(&mut self) -> io::Result<usize> {
        if self.eof {
            return Ok(0);
        }
        let timeout_ms = i32::try_from(self.step_timeout.as_millis()).unwrap_or(i32::MAX);
        let ready = unsafe {
            let mut poller = libc::pollfd { fd: self.fd, events: libc::POLLIN, revents: 0 };
            let mut outcome;
            loop {
                outcome = libc::poll(std::ptr::addr_of_mut!(poller), 1, timeout_ms);
                if outcome < 0 && io::Error::last_os_error().kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                break;
            }
            outcome
        };
        if ready < 0 {
            return Err(io::Error::last_os_error());
        }
        if ready == 0 {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                format!("no bytes within {} ms", self.step_timeout.as_millis()),
            ));
        }
        let mut chunk = [0u8; 8192];
        let read = unsafe { libc::read(self.fd, chunk.as_mut_ptr().cast::<libc::c_void>(), chunk.len()) };
        if read < 0 {
            return Err(io::Error::last_os_error());
        }
        if read == 0 {
            self.eof = true;
            return Ok(0);
        }
        let read = usize::try_from(read).unwrap_or(0);
        self.buffer.extend_from_slice(&chunk[..read]);
        Ok(read)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;
    use std::os::unix::net::UnixStream;

    fn pair() -> (UnixStream, UnixStream, PipedInput) {
        let (writer, reader) = UnixStream::pair().expect("socketpair");
        writer.set_nonblocking(false).expect("blocking writer");
        let input = PipedInput::new(&reader);
        (writer, reader, input)
    }

    #[test]
    fn a_line_arrives_in_pieces() {
        let (mut writer, _owner, mut input) = pair();
        writer.write_all(b"Content").expect("write");
        writer.write_all(b"-Length: 4\r\n\r\n").expect("write");
        writer.write_all(b"body").expect("write");

        let mut header = Vec::new();
        let read = input.read_until(b'\n', &mut header).expect("first line");
        assert_eq!(read, b"Content-Length: 4\r\n".len());
        let read = input.read_until(b'\n', &mut header).expect("blank line");
        assert_eq!(read, b"\r\n".len());
        assert_eq!(header, b"Content-Length: 4\r\n\r\n");

        let mut body = Vec::new();
        input.read_exact_to(4, &mut body).expect("read body");
        assert_eq!(body, b"body");
    }

    #[test]
    fn a_silent_peer_times_out() {
        let (_writer, _owner, mut input) = pair();
        input.set_step_timeout(Duration::from_millis(50));
        let mut out = Vec::new();
        let error = input.read_until(b'\n', &mut out).expect_err("no data within 50ms");
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
        assert!(out.is_empty(), "nothing was buffered");
    }

    #[test]
    fn a_closed_peer_is_end_of_stream_not_an_error() {
        let (writer, _owner, mut input) = pair();
        drop(writer);
        let mut out = Vec::new();
        let read = input.read_until(b'\n', &mut out).expect("eof reads as zero");
        assert_eq!(read, 0);
        assert!(input.at_eof());

        let error = input.read_exact_to(4, &mut out).expect_err("eof cannot satisfy a body");
        assert_eq!(error.kind(), io::ErrorKind::UnexpectedEof);
    }

    #[test]
    fn a_partial_line_then_eof_hands_over_the_tail() {
        let (mut writer, _owner, mut input) = pair();
        writer.write_all(b"half").expect("write");
        drop(writer);
        let mut out = Vec::new();
        let read = input.read_until(b'\n', &mut out).expect("eof after partial");
        assert_eq!(read, 4);
        assert_eq!(out, b"half");
    }
}
