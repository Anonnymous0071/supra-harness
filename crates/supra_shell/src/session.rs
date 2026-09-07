//! A persistent pty shell session.
//!
//! One session is one pty pair and one child, wired together through the T16
//! sandbox:
//!
//! 1. [`supra_ffi::pty::Pty::open`] allocates the pair - master for the
//!    harness, slave for the child - with both ends CLOEXEC from birth.
//! 2. [`supra_sandbox::spawn`] runs the child with the slave fd as all three
//!    standard streams, an explicit environment, and the full audit before
//!    exec. The sandbox is not an option here: the child gets the workspace
//!    policy or it does not run. `yolo` does not skip this; there is no
//!    unsandboxed spawn path in this crate.
//! 3. [`Pty::take_slave`] closes the parent's copy of the slave the moment
//!    the spawn succeeds (the T4 note binds T16.5: "close descriptors they
//!    do not intend to pass"). The child holds its own copies on 0/1/2; the
//!    parent-held copy would keep the pair alive after the child exits and
//!    master reads would never see EOF.
//!
//! Reads are blocking: `read` returns when at least one byte arrives, which
//! is what a dedicated reader thread wants (the TUI owns that thread; this
//! crate stays synchronous and deterministic). EOF on the master means the
//! child closed its end - the session is over.

use std::io::Read as _;
use std::io::Write as _;
use std::time::Duration;

use supra_ffi::pty::{Pty, PtySize};
use supra_ffi::sandbox::Process;
use supra_sandbox::{SandboxPolicy, SpawnRequest, Stdio, TreeBudget};
use supra_types::{Mode, Reversibility};

use crate::error::ShellError;
use crate::shaping::{Shaped, Shaper};

/// A running (or finished) pty shell session.
///
/// `process` is declared first so that drop order kills the child before the
/// master closes - a child writing into a closed master is a child getting
/// `SIGHUP`, which is the fail-closed end of the story.
pub struct ShellSession {
    process: Option<Process>,
    pty: Pty,
    shaper: Shaper,
}

impl ShellSession {
    /// Open the pty, spawn the child through the sandbox, and close the
    /// parent's slave copy.
    ///
    /// `request` carries the command, the explicit environment, the mode and
    /// reversibility the permission gate sees, and the guard's marker - all
    /// of it forwarded verbatim to [`supra_sandbox::spawn`]. Only `stdio`
    /// is overridden here: this session is the pty, so the child's streams
    /// come from the slave fd regardless of what the request said. The
    /// caller's `stdio` is deliberately ignored, because a session whose
    /// child does not talk to its own terminal is not a session.
    ///
    /// # Errors
    ///
    /// [`ShellError::Spawn`] when the sandbox refuses (including a leaky
    /// descriptor on the host); [`ShellError::Pty`] when the pair cannot be
    /// opened or sized. On the spawn error the pty is dropped, closing both
    /// ends - no child, no descriptor left behind.
    pub fn spawn(
        policy: &SandboxPolicy,
        request: &SpawnRequest<'_>,
        tree: &TreeBudget,
        size: PtySize,
        shaper: Shaper,
    ) -> Result<Self, ShellError> {
        let _ = supra_guard::establish_identity();
        let _ = supra_guard::generate_marker_key();
        let marker = supra_guard::issue_marker()
            .map_err(|refusal| -> ShellError {
                supra_sandbox::SandboxError::Authority(1, vec![refusal]).into()
            })?
            .or_else(|| request.marker.map(str::to_owned));
        let mut request = *request;
        request.marker = marker.as_deref().or(request.marker);
        let mut pty = Pty::open(size)?;
        let slave_fd = pty.slave_fd().ok_or_else(|| {
            ShellError::Pty(std::io::Error::new(
                std::io::ErrorKind::BrokenPipe,
                "the pty lost its slave before the spawn",
            ))
        })?;

        request.stdio = Stdio::all(slave_fd);
        let process = supra_sandbox::spawn(policy, &request, tree)?;

        // The child holds its copies; the parent's copy closes now.
        pty.take_slave();

        Ok(Self { process: Some(process), pty, shaper })
    }

    /// Block until at least one byte of output arrives, and shape it.
    ///
    /// The shaped transcript accumulates inside the session's [`Shaper`];
    /// call [`ShellSession::render`] to pull it out. EOF returns a shaped
    /// snapshot with `eof` set on the stats side? - no: EOF simply means
    /// the child closed its end, so this returns `Ok(None)` and the caller
    /// treats the session as finished.
    ///
    /// # Errors
    ///
    /// [`ShellError::Pty`] when the master read itself fails.
    pub fn read(&mut self, out: &mut Vec<u8>) -> Result<Option<Shaped>, ShellError> {
        let mut buffer = [0u8; 4096];
        let count = match self.pty.master().read(&mut buffer) {
            Ok(0) => return Ok(None),
            Ok(count) => count,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => return Ok(None),
            Err(error) if error.raw_os_error() == Some(5) => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        self.shaper.push(&buffer[..count]);
        out.extend_from_slice(&buffer[..count]);
        Ok(Some(self.shaper.stats()))
    }

    /// Write input to the child - keystrokes, a command line, a signal
    /// escape.
    ///
    /// # Errors
    ///
    /// [`ShellError::Pty`] when the master write fails.
    pub fn write(&mut self, input: &[u8]) -> Result<usize, ShellError> {
        Ok(self.pty.master().write(input)?)
    }

    /// Resize the terminal. The kernel signals the foreground group; a child
    /// that cares redraws.
    ///
    /// # Errors
    ///
    /// [`ShellError::Pty`] when the ioctl fails.
    pub fn resize(&mut self, size: PtySize) -> Result<(), ShellError> {
        Ok(self.pty.resize(size)?)
    }

    /// The shaped transcript so far.
    pub fn render(&self, out: &mut Vec<u8>) {
        self.shaper.render(out);
    }

    /// The shaping stats so far.
    #[must_use]
    pub fn stats(&self) -> Shaped {
        self.shaper.stats()
    }

    /// The child's process id, while it runs.
    #[must_use]
    pub fn pid(&self) -> Option<i64> {
        self.process.as_ref().map(Process::pid)
    }

    /// Wait for the child to exit.
    ///
    /// `Some(duration)` bounds the wait; `None` waits forever. On success
    /// the child's exit code comes back (`None` from the inner result means
    /// the timeout expired while the child still ran).
    ///
    /// # Errors
    ///
    /// [`ShellError::NotRunning`] when there is no child - after a
    /// successful wait, or after the child was never spawned.
    pub fn wait(&mut self, timeout: Option<Duration>) -> Result<Option<i32>, ShellError> {
        let process = self.process.as_mut().ok_or(ShellError::NotRunning)?;
        let status = process.wait(timeout)?;
        if status.is_some() {
            self.process = None;
        }
        Ok(status)
    }

    /// Kill the child with a grace period, and reap it.
    ///
    /// Returns whether a kill was sent; a child that already exited answers
    /// `false`. The session keeps its pty and shaper - the transcript is
    /// still readable after the kill.
    ///
    /// # Errors
    ///
    /// [`ShellError::NotRunning`] when there is no child to kill.
    pub fn kill(&mut self, grace: Duration) -> Result<bool, ShellError> {
        let process = self.process.as_mut().ok_or(ShellError::NotRunning)?;
        let killed = process.kill(grace);
        if killed {
            self.process = None;
        }
        Ok(killed)
    }
}

impl core::fmt::Debug for ShellSession {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("ShellSession")
            .field("pid", &self.pid())
            .field("stats", &self.stats())
            .finish_non_exhaustive()
    }
}

/// Convenience constructor for the common shape: one command, no permission
/// ceremony.
///
/// The permission ceremony (mode, reversibility, marker) lives in the
/// `SpawnRequest`; this helper exists so tests and simple callers do not
/// repeat a nine-field literal. It is not a bypass: the same audit, guard,
/// and sandbox run behind it.
///
/// # Errors
///
/// Whatever [`ShellSession::spawn`] refuses: the sandbox (authority,
/// consent, a leaky descriptor), the pty, or the marker issuance. The
/// convenience stops at the signature; the refusal shapes are identical.
pub fn spawn_simple(
    policy: &SandboxPolicy,
    argv: &[&str],
    env: &[(&str, &str)],
    working_dir: Option<&std::path::Path>,
    tree: &TreeBudget,
    size: PtySize,
    shaper: Shaper,
) -> Result<ShellSession, ShellError> {
    let _ = supra_guard::establish_identity();
    let request = SpawnRequest {
        argv,
        env,
        working_dir,
        reversibility: Reversibility::R0,
        mode: Mode::Auto,
        marker: None,
        lineage: None,
        child: None,
        claim_vote: None,
        stdio: Stdio::null(),
    };
    ShellSession::spawn(policy, &request, tree, size, shaper)
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use supra_ffi::width::Ambiguous;
    use supra_sandbox::default_policy;

    use super::*;

    /// Skip helper for hosts whose kernel cannot enforce the filesystem
    /// tier; the pty primitives are unit-tested elsewhere, but the session
    /// composition needs a real sandbox.
    fn landlock_available() -> bool {
        supra_ffi::sandbox::probe().tier.enforces_filesystem()
    }

    fn fixture_workspace(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(name);
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("workspace");
        dir
    }

    /// Read shaped output until `needle` appears or the deadline passes.
    /// Raw bytes land in `raw` as well, so tests can assert on the exact
    /// byte stream the child produced (CRLF and all).
    fn read_until(session: &mut ShellSession, raw: &mut Vec<u8>, needle: &[u8], deadline: Instant) -> bool {
        while !raw.windows(needle.len()).any(|window| window == needle) {
            if Instant::now() > deadline {
                return false;
            }
            let mut buffer = Vec::new();
            if session.read(&mut buffer).expect("read").is_none() {
                return false;
            }
            raw.extend_from_slice(&buffer);
        }
        true
    }

    #[test]
    fn session_spawns_a_child_and_reads_its_output() {
        if !landlock_available() {
            return;
        }
        let workspace = fixture_workspace("supra-shell-session-output");
        let policy = default_policy(&workspace);
        let tree = TreeBudget::default();

        let mut session = spawn_simple(
            &policy,
            &["/bin/sh", "-c", "echo hello from the pty"],
            &[("PATH", "/usr/bin:/bin")],
            Some(workspace.as_path()),
            &tree,
            PtySize::default_size(),
            Shaper::new(120, 1024, Ambiguous::Narrow),
        )
        .expect("spawn");

        let mut raw = Vec::new();
        assert!(
            read_until(
                &mut session,
                &mut raw,
                b"hello from the pty",
                Instant::now() + Duration::from_secs(10)
            ),
            "the child's output must arrive; saw {raw:?}"
        );

        // The pty's output processing renders the newline as CRLF - the
        // byte stream a terminal would have drawn.
        assert!(raw.windows(5).any(|w| w == b"pty\r\n"), "CRLF newline: {raw:?}");
        assert_eq!(session.wait(Some(Duration::from_secs(10))).expect("wait"), Some(0));
    }

    #[test]
    fn the_parent_slave_closes_so_the_master_sees_eof() {
        // The T4 note binds this: "close descriptors they do not intend to
        // pass." A parent-held slave keeps the pty pair alive after the
        // child exits, and master reads would block forever instead of
        // seeing EOF - the session would look busy with no one in it. The
        // test runs a child that exits, waits for it, then drains with a
        // hard deadline: pending output first, EOF after. A mutation that
        // drops the `take_slave` call turns the deadline into a failure
        // instead of a hung suite.
        if !landlock_available() {
            return;
        }
        let workspace = fixture_workspace("supra-shell-session-eof");
        let policy = default_policy(&workspace);
        let tree = TreeBudget::default();

        let mut session = spawn_simple(
            &policy,
            &["/bin/sh", "-c", "echo done"],
            &[("PATH", "/usr/bin:/bin")],
            Some(workspace.as_path()),
            &tree,
            PtySize::default_size(),
            Shaper::new(120, 1024, Ambiguous::Narrow),
        )
        .expect("spawn");

        assert_eq!(session.wait(Some(Duration::from_secs(10))).expect("wait"), Some(0));

        let (eof, eof_seen) = std::sync::mpsc::channel::<bool>();
        let reader = std::thread::spawn(move || {
            loop {
                let mut raw = Vec::new();
                match session.read(&mut raw) {
                    Ok(None) => {
                        let _ = eof.send(true);
                        return;
                    }
                    Ok(Some(_)) => {}
                    Err(error) => panic!("the drain failed: {error}"),
                }
            }
        });

        match eof_seen.recv_timeout(Duration::from_secs(10)) {
            Ok(true) => {}
            _ => panic!("EOF never arrived: the parent still holds the slave"),
        }
        reader.join().expect("the reader finished cleanly");
        let _ = std::fs::remove_dir_all(&workspace);
    }

    #[test]
    fn session_stdio_is_the_pty() {
        if !landlock_available() {
            return;
        }
        let workspace = fixture_workspace("supra-shell-session-tty");
        let policy = default_policy(&workspace);
        let tree = TreeBudget::default();

        // `test -t 0` answers whether stdin is a terminal. The slave fd is
        // a pty, so the answer is "yes" - and a mutation that reverts the
        // session to /dev/null stdio makes this fail.
        let mut session = spawn_simple(
            &policy,
            &["/bin/sh", "-c", "test -t 0 && echo tty || echo notty"],
            &[("PATH", "/usr/bin:/bin")],
            Some(workspace.as_path()),
            &tree,
            PtySize::default_size(),
            Shaper::new(120, 1024, Ambiguous::Narrow),
        )
        .expect("spawn");

        let mut raw = Vec::new();
        assert!(
            read_until(&mut session, &mut raw, b"tty", Instant::now() + Duration::from_secs(10)),
            "the child must see a terminal; saw {raw:?}"
        );
        assert!(!raw.windows(5).any(|w| w == b"notty"), "the child must not see /dev/null; saw {raw:?}");
        let _ = session.wait(Some(Duration::from_secs(10)));
    }

    #[test]
    fn session_writes_input_to_the_child() {
        if !landlock_available() {
            return;
        }
        let workspace = fixture_workspace("supra-shell-session-input");
        let policy = default_policy(&workspace);
        let tree = TreeBudget::default();

        let mut session = spawn_simple(
            &policy,
            &["/bin/sh", "-c", "read line; echo got:$line"],
            &[("PATH", "/usr/bin:/bin")],
            Some(workspace.as_path()),
            &tree,
            PtySize::default_size(),
            Shaper::new(120, 1024, Ambiguous::Narrow),
        )
        .expect("spawn");

        // The child blocks in `read` until a line arrives; the canonical
        // discipline echoes it back before the echo prints, so the raw
        // stream contains the typed line and then the reply.
        session.write(b"ping the shell\n").expect("write");
        let mut raw = Vec::new();
        assert!(
            read_until(
                &mut session,
                &mut raw,
                b"got:ping the shell",
                Instant::now() + Duration::from_secs(10)
            ),
            "the child must answer; saw {raw:?}"
        );
        let _ = session.wait(Some(Duration::from_secs(10)));
    }

    #[test]
    fn session_resize_reaches_the_child() {
        if !landlock_available() {
            return;
        }
        let workspace = fixture_workspace("supra-shell-session-resize");
        let policy = default_policy(&workspace);
        let tree = TreeBudget::default();

        let mut session = spawn_simple(
            &policy,
            &["/bin/sh", "-c", "stty size"],
            &[("PATH", "/usr/bin:/bin")],
            Some(workspace.as_path()),
            &tree,
            PtySize { rows: 33, cols: 100 },
            Shaper::new(120, 1024, Ambiguous::Narrow),
        )
        .expect("spawn");

        let mut raw = Vec::new();
        assert!(
            read_until(&mut session, &mut raw, b"33 100", Instant::now() + Duration::from_secs(10)),
            "the child must see the session's size; saw {raw:?}"
        );
        let _ = session.wait(Some(Duration::from_secs(10)));
    }

    #[test]
    fn session_wait_times_out_and_kill_reaps() {
        if !landlock_available() {
            return;
        }
        let workspace = fixture_workspace("supra-shell-session-kill");
        let policy = default_policy(&workspace);
        let tree = TreeBudget::default();

        let mut session = spawn_simple(
            &policy,
            &["/bin/sh", "-c", "sleep 60"],
            &[("PATH", "/usr/bin:/bin")],
            Some(workspace.as_path()),
            &tree,
            PtySize::default_size(),
            Shaper::new(120, 1024, Ambiguous::Narrow),
        )
        .expect("spawn");

        // The child is alive and the bounded wait says so.
        assert_eq!(session.wait(Some(Duration::from_millis(200))).expect("bounded wait"), None);

        // Killing reaps it; the next wait has no child to wait for.
        assert!(session.kill(Duration::from_millis(200)).expect("kill"));
        assert!(matches!(session.wait(Some(Duration::from_secs(1))), Err(ShellError::NotRunning)));
    }

    #[test]
    fn session_drop_kills_the_child() {
        if !landlock_available() {
            return;
        }
        let workspace = fixture_workspace("supra-shell-session-drop");
        let policy = default_policy(&workspace);
        let tree = TreeBudget::default();

        let session = spawn_simple(
            &policy,
            &["/bin/sh", "-c", "sleep 60"],
            &[("PATH", "/usr/bin:/bin")],
            Some(workspace.as_path()),
            &tree,
            PtySize::default_size(),
            Shaper::new(120, 1024, Ambiguous::Narrow),
        )
        .expect("spawn");
        let pid = session.pid().expect("a pid");
        assert!(std::path::Path::new(&format!("/proc/{pid}")).exists(), "the child is alive");

        drop(session);

        // The drop kills and reaps; the pid leaves the process table.
        let deadline = Instant::now() + Duration::from_secs(10);
        while std::path::Path::new(&format!("/proc/{pid}")).exists() {
            assert!(Instant::now() < deadline, "the child must be reaped on drop");
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    #[test]
    fn shaping_accumulates_across_reads() {
        // The session's shaper is the same object across reads: a progress
        // line that overwrites itself byte by byte shapes to its final
        // segment, exactly as the user saw it.
        if !landlock_available() {
            return;
        }
        let workspace = fixture_workspace("supra-shell-session-shaping");
        let policy = default_policy(&workspace);
        let tree = TreeBudget::default();

        let mut session = spawn_simple(
            &policy,
            &["/bin/sh", "-c", "printf '50%%\\r75%%\\r100%%\\n'"],
            &[("PATH", "/usr/bin:/bin")],
            Some(workspace.as_path()),
            &tree,
            PtySize::default_size(),
            Shaper::new(120, 1024, Ambiguous::Narrow),
        )
        .expect("spawn");

        let mut raw = Vec::new();
        assert!(
            read_until(&mut session, &mut raw, b"100%", Instant::now() + Duration::from_secs(10)),
            "the progress line must arrive; saw {raw:?}"
        );
        let _ = session.wait(Some(Duration::from_secs(10)));

        let mut shaped = Vec::new();
        session.render(&mut shaped);
        assert_eq!(shaped, b"100%", "only the final segment survives shaping");
    }
}
