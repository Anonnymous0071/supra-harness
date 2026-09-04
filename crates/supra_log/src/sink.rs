//! The sink: rotation, file permissions, and stderr that respects the TUI.
//!
//! # One line, one write
//!
//! Every line reaches the file in a single `write` call on a descriptor opened
//! `O_APPEND`. That is what keeps two supra processes sharing a log file from
//! interleaving halves of each other's lines: `O_APPEND` makes the offset update and
//! the write one operation, so concurrent writers produce whole lines in some order
//! rather than shredded ones.
//!
//! It is also why redaction happens here rather than at the call site. The whole line
//! is in hand exactly once, at the moment before it becomes durable, so a secret
//! cannot slip through by being split across two writes.
//!
//! # stderr belongs to whoever owns the terminal
//!
//! `print_stderr` is banned workspace-wide so that every diagnostic arrives here.
//! While the TUI owns the terminal, a stray line would tear the frame - so stderr
//! mirroring is suppressed for as long as [`Sink::suppress_stderr`]'s guard is alive.
//!
//! That is a guard rather than a pair of setters on purpose. A `set(false)` whose
//! matching `set(true)` is missed does not fail loudly; it silently discards every
//! diagnostic for the rest of the session, which is the worst outcome for a component
//! whose job is to tell you what happened.
//!
//! # A lost line is itself reported
//!
//! `io::Write::flush` and `Drop` cannot return an error, so a failed write has nowhere
//! to go. Rather than swallow it, the sink counts it and prepends a notice to the next
//! line that does get through. A gap in a log is only debuggable if the log says there
//! is one.

use std::fs::{File, OpenOptions};
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use crate::redact;

/// Default size at which the current file is rotated.
///
/// Eight megabytes holds a long session at debug level while staying small enough to
/// open in an editor and to attach to a bug report.
pub const DEFAULT_MAX_BYTES: u64 = 8 * 1024 * 1024;

/// Default number of rotated files kept alongside the current one.
///
/// Four plus the current file bounds the log at five times the rotation size. A bound
/// is the point: an agent harness that runs for days must not fill a disk.
pub const DEFAULT_KEEP: usize = 4;

/// Where the log goes and how much of it is kept.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SinkConfig {
    /// The current log file. Its parent directory is created if absent.
    pub path: PathBuf,
    /// Rotate once the current file would exceed this many bytes.
    pub max_bytes: u64,
    /// How many rotated files to keep.
    pub keep: usize,
    /// Whether to mirror lines to stderr when no guard is suppressing it.
    pub stderr: bool,
}

impl SinkConfig {
    /// A configuration writing to `path` with the default bounds.
    #[must_use]
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into(), max_bytes: DEFAULT_MAX_BYTES, keep: DEFAULT_KEEP, stderr: true }
    }

    /// Rotate at `bytes` instead of the default.
    #[must_use]
    pub const fn with_max_bytes(mut self, bytes: u64) -> Self {
        self.max_bytes = bytes;
        self
    }

    /// Keep `count` rotated files instead of the default.
    #[must_use]
    pub const fn with_keep(mut self, count: usize) -> Self {
        self.keep = count;
        self
    }

    /// Whether to mirror to stderr at all.
    #[must_use]
    pub const fn with_stderr(mut self, enabled: bool) -> Self {
        self.stderr = enabled;
        self
    }
}

/// Mutable state behind the sink's lock.
struct Open {
    file: File,
    written: u64,
}

/// A log destination.
///
/// Cheap to share: wrap it in an `Arc` and hand clones to the writer adapter and to
/// whoever needs [`Sink::suppress_stderr`].
pub struct Sink {
    config: SinkConfig,
    open: Mutex<Option<Open>>,
    stderr_suppressed: AtomicBool,
    dropped: AtomicU64,
}

impl Sink {
    /// Open a sink, creating the file and its parent directory.
    ///
    /// # Errors
    ///
    /// Any I/O failure creating the directory or opening the file. Failing here is
    /// deliberate: a harness that cannot record what it did should say so at startup
    /// rather than discover it during an incident.
    pub fn open(config: SinkConfig) -> io::Result<Self> {
        let sink = Self {
            config,
            open: Mutex::new(None),
            stderr_suppressed: AtomicBool::new(false),
            dropped: AtomicU64::new(0),
        };
        // Open eagerly so a bad path is a startup error rather than a silent
        // per-line failure later.
        let mut guard = sink.open.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        *guard = Some(open_append(&sink.config.path)?);
        drop(guard);
        Ok(sink)
    }

    /// The file being written.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.config.path
    }

    /// Lines that could not be written.
    ///
    /// Non-zero means the log has a gap. The next successful line carries a notice
    /// saying so, but this is available for a health check that would rather ask.
    #[must_use]
    pub fn dropped_lines(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }

    /// Suppress stderr mirroring until the returned guard is dropped.
    ///
    /// Held by the TUI for as long as it owns the terminal.
    #[must_use]
    pub fn suppress_stderr(&self) -> StderrGuard<'_> {
        self.stderr_suppressed.store(true, Ordering::Release);
        StderrGuard { sink: self }
    }

    /// Whether a line would currently be mirrored to stderr.
    #[must_use]
    pub fn mirrors_to_stderr(&self) -> bool {
        self.config.stderr && !self.stderr_suppressed.load(Ordering::Acquire)
    }

    /// Redact `line`, then write it as one line to the file and possibly to stderr.
    ///
    /// Never returns an error: a diagnostic path that can fail its caller is a
    /// diagnostic path callers learn to avoid. A failure increments
    /// [`Sink::dropped_lines`] and is reported on the next line that succeeds.
    pub fn write_line(&self, line: &str) {
        let redacted = redact::redact(line.trim_end_matches(['\n', '\r']));

        let mut payload = String::with_capacity(redacted.len() + 1);
        let lost = self.dropped.swap(0, Ordering::Relaxed);
        if lost > 0 {
            use std::fmt::Write as _;
            // The notice is itself a well-formed line in the same shape as the rest, so a
            // tool reading the log does not have to special-case the gap marker.
            let _ = writeln!(
                payload,
                "{{\"level\":\"WARN\",\"target\":\"supra_log\",\
                 \"message\":\"{lost} log line(s) could not be written\"}}"
            );
        }
        payload.push_str(&redacted);
        payload.push('\n');

        if self.write_to_file(payload.as_bytes()).is_err() {
            // Put the count back, plus this line, so nothing is quietly forgotten.
            self.dropped.fetch_add(lost + 1, Ordering::Relaxed);
        }

        if self.mirrors_to_stderr() {
            // The one place in the workspace that writes to stderr. Failure here is
            // ignored on purpose: a closed stderr is normal for a daemonised process
            // and must not cost a line in the file, which already has it.
            let mut handle = io::stderr().lock();
            let _ = handle.write_all(payload.as_bytes());
            let _ = handle.flush();
        }
    }

    fn write_to_file(&self, payload: &[u8]) -> io::Result<()> {
        let mut guard = self.open.lock().unwrap_or_else(std::sync::PoisonError::into_inner);

        // Reopen if a previous write failed and left the slot empty.
        if guard.is_none() {
            *guard = Some(open_append(&self.config.path)?);
        }

        let needs_rotation = guard
            .as_ref()
            .is_some_and(|open| open.written.saturating_add(payload.len() as u64) > self.config.max_bytes);

        if needs_rotation {
            // Close before renaming: on Windows an open handle blocks the rename, and
            // on Unix keeping it open would append to the rotated file instead.
            *guard = None;
            // A rotation failure - a read-only directory, a permission change - is
            // tolerated rather than propagated: an oversized log is a smaller problem
            // than no log, and the reopen below is the same either way.
            let _ = rotate(&self.config.path, self.config.keep);
            *guard = Some(open_append(&self.config.path)?);
        }

        let open = guard.as_mut().ok_or_else(|| io::Error::other("log file is not open"))?;
        match open.file.write_all(payload) {
            Ok(()) => {
                open.written = open.written.saturating_add(payload.len() as u64);
                Ok(())
            }
            Err(error) => {
                // Drop the handle so the next line reopens rather than retrying a
                // descriptor that may be gone - a rotation by an outside process, a
                // deleted file.
                *guard = None;
                Err(error)
            }
        }
    }
}

/// Restores stderr mirroring when dropped.
pub struct StderrGuard<'a> {
    sink: &'a Sink,
}

impl Drop for StderrGuard<'_> {
    fn drop(&mut self) {
        self.sink.stderr_suppressed.store(false, Ordering::Release);
    }
}

/// Hand-written `Debug`, deliberately lock-free and therefore partial.
///
/// Taking the file mutex to report a byte count would let a `Debug` call inside a
/// diagnostic block on the very lock the diagnostic is waiting for. Everything reported
/// comes from the configuration or an atomic, so `open` - the only field behind the lock
/// - is omitted on purpose rather than by oversight.
#[allow(
    clippy::missing_fields_in_debug,
    reason = "the omitted field is behind a mutex that a Debug call must not take"
)]
impl std::fmt::Debug for Sink {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Sink")
            .field("path", &self.config.path)
            .field("max_bytes", &self.config.max_bytes)
            .field("keep", &self.config.keep)
            .field("mirrors_to_stderr", &self.mirrors_to_stderr())
            .field("dropped_lines", &self.dropped_lines())
            .finish()
    }
}

/// Open `path` for appending, creating it and its parent, owner-readable only.
///
/// The mode matters. A log records paths, arguments, provider names, and - despite the
/// redactor - whatever a message put in it. T7 holds the user's configuration to
/// `0600`; a log written beside it at `0644` would make that pointless.
fn open_append(path: &Path) -> io::Result<Open> {
    refuse_blocking_target(path)?;

    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }

    let mut options = OpenOptions::new();
    options.create(true).append(true);

    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }

    let file = options.open(path)?;
    // A just-opened handle essentially cannot fail to stat. Treating an unknown length as
    // zero errs toward continuing to log rather than rotating on every open, which is the
    // right direction when the alternative is losing diagnostics.
    let written = file.metadata().map_or(0, |meta| meta.len());
    Ok(Open { file, written })
}

/// Refuse a target whose `open` would block for ever.
///
/// Opening a FIFO for writing blocks until a **reader** appears. Since the sink is
/// opened at startup, pointing the log at a pipe turns a misconfiguration into a hang
/// with no diagnosis - before any UI exists to explain it. Verified by a test that hung
/// until this check was added.
///
/// This is the same failure T7 guards against when reading configuration, and it was
/// missed here: the lesson generalises, so it is now checked in both places and probed
/// in both.
///
/// Only FIFOs are refused, not every non-regular file. `/dev/null` is a legitimate
/// "discard the log" target and `/dev/full` is how the failure path is tested; both are
/// character devices and both open immediately. A character device could in principle
/// block, but refusing the class would cost more than it buys.
///
/// The pre-flight `stat` races with the open, exactly as in T7, and for the same reason
/// that does not matter: exploiting it needs write access to the log directory, and
/// anyone with that can simply replace the log.
#[cfg(unix)]
fn refuse_blocking_target(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::FileTypeExt as _;

    match std::fs::metadata(path) {
        Ok(metadata) if metadata.file_type().is_fifo() => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "{} is a FIFO; opening one for writing blocks until a reader appears, \
                 which would hang startup",
                path.display()
            ),
        )),
        // Anything else, including a path that does not exist yet, is handled by the
        // open itself - a directory reports EISDIR with a clear message already.
        _ => Ok(()),
    }
}

#[cfg(not(unix))]
fn refuse_blocking_target(_path: &Path) -> io::Result<()> {
    Ok(())
}

/// Shift `path` to `path.1`, `path.1` to `path.2`, and so on, dropping the oldest.
///
/// Oldest first, so no rename can overwrite a file that has not been moved yet.
///
/// The explicit removal of the oldest file is **required on Windows and redundant on
/// Unix**, where `rename` replaces an existing target. A mutation deleting it therefore
/// survives the suite on Linux, and that is recorded rather than papered over with a
/// contorted test: the line is platform-defensive, not untested logic.
fn rotate(path: &Path, keep: usize) -> io::Result<()> {
    if keep == 0 {
        // No history wanted: truncate rather than rename, so the bound still holds.
        std::fs::write(path, b"")?;
        return Ok(());
    }

    let rotated = |index: usize| -> PathBuf {
        let mut name = path.as_os_str().to_os_string();
        name.push(format!(".{index}"));
        PathBuf::from(name)
    };

    // The oldest file falls off the end.
    let oldest = rotated(keep);
    if oldest.exists() {
        std::fs::remove_file(&oldest)?;
    }

    for index in (1..keep).rev() {
        let from = rotated(index);
        if from.exists() {
            std::fs::rename(&from, rotated(index + 1))?;
        }
    }

    if path.exists() {
        std::fs::rename(path, rotated(1))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    struct Scratch(PathBuf);

    impl Scratch {
        fn new(name: &str) -> Self {
            let path = std::env::temp_dir().join(format!("supra-log-{name}"));
            let _ = fs::remove_dir_all(&path);
            fs::create_dir_all(&path).expect("scratch directory");
            Self(path)
        }

        fn log(&self) -> PathBuf {
            self.0.join("supra.log")
        }

        fn read(&self, suffix: &str) -> String {
            let path = if suffix.is_empty() {
                self.log()
            } else {
                PathBuf::from(format!("{}{suffix}", self.log().display()))
            };
            fs::read_to_string(path).unwrap_or_default()
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn sink(scratch: &Scratch) -> Sink {
        Sink::open(SinkConfig::new(scratch.log()).with_stderr(false)).expect("open")
    }

    #[test]
    fn a_line_reaches_the_file_with_exactly_one_newline() {
        let scratch = Scratch::new("one-line");
        let sink = sink(&scratch);
        sink.write_line("first");
        // Already-newline-terminated input must not gain a second.
        sink.write_line("second\n");
        assert_eq!(scratch.read(""), "first\nsecond\n");
    }

    #[test]
    fn a_missing_parent_directory_is_created() {
        let scratch = Scratch::new("nested");
        let nested = scratch.0.join("deep").join("deeper").join("supra.log");
        let sink = Sink::open(SinkConfig::new(&nested).with_stderr(false)).expect("open");
        sink.write_line("here");
        assert_eq!(fs::read_to_string(&nested).expect("read"), "here\n");
    }

    #[test]
    #[cfg(unix)]
    fn the_log_file_is_owner_only() {
        // A log sits beside the configuration T7 holds to 0600, and records enough
        // about a session that the same reasoning applies.
        use std::os::unix::fs::PermissionsExt as _;
        let scratch = Scratch::new("mode");
        let sink = sink(&scratch);
        sink.write_line("x");

        let mode = fs::metadata(scratch.log()).expect("stat").permissions().mode() & 0o777;
        assert_eq!(mode & 0o077, 0, "mode {mode:04o} is readable beyond its owner");
    }

    #[test]
    fn a_secret_is_redacted_on_its_way_to_the_file() {
        // Redaction is the sink's job, not the call site's, so this is the property
        // that matters: nothing a caller does puts a credential on disk.
        let scratch = Scratch::new("redacted");
        let sink = sink(&scratch);
        sink.write_line(r#"{"api_key":"hunter2","note":"sk-ABCDEFGHIJKLMNOPQRSTUVWX"}"#);

        let written = scratch.read("");
        assert!(!written.contains("hunter2"), "{written}");
        assert!(!written.contains("ABCDEFGHIJKLMNOPQRSTUVWX"), "{written}");
        assert!(written.contains("note"), "structure must survive: {written}");
    }

    #[test]
    fn rotation_happens_at_the_configured_size_and_keeps_the_bound() {
        let scratch = Scratch::new("rotate");
        let sink =
            Sink::open(SinkConfig::new(scratch.log()).with_stderr(false).with_max_bytes(32).with_keep(2))
                .expect("open");

        for index in 0..20 {
            sink.write_line(&format!("line-{index:03}-padding-to-force-rotation"));
        }

        assert!(scratch.log().exists(), "the current file must always exist");
        // keep = 2 means .1 and .2 at most, and never a .3.
        let third = PathBuf::from(format!("{}.3", scratch.log().display()));
        assert!(!third.exists(), "the bound was exceeded: {} exists", third.display());
    }

    #[test]
    fn rotation_preserves_the_most_recent_history_in_order() {
        let scratch = Scratch::new("order");
        // The threshold is compared against the *prospective* total, so a limit equal
        // to one line's length yields exactly one line per file. Each line here is
        // four bytes including its newline.
        let sink =
            Sink::open(SinkConfig::new(scratch.log()).with_stderr(false).with_max_bytes(4).with_keep(2))
                .expect("open");

        sink.write_line("aaa");
        sink.write_line("bbb");
        sink.write_line("ccc");

        // Newest in the current file, older ones shifted outward.
        assert_eq!(scratch.read(""), "ccc\n");
        assert_eq!(scratch.read(".1"), "bbb\n");
        assert_eq!(scratch.read(".2"), "aaa\n");
    }

    #[test]
    fn a_file_below_the_threshold_holds_several_lines() {
        // The complement of the test above, and the behaviour a reader should expect:
        // rotation is by size, not by line, so a generous limit accumulates.
        let scratch = Scratch::new("accumulate");
        let sink =
            Sink::open(SinkConfig::new(scratch.log()).with_stderr(false).with_max_bytes(8).with_keep(2))
                .expect("open");

        sink.write_line("aaa");
        sink.write_line("bbb");
        assert_eq!(scratch.read(""), "aaa\nbbb\n", "eight bytes holds two four-byte lines");

        sink.write_line("ccc");
        assert_eq!(scratch.read(""), "ccc\n");
        assert_eq!(scratch.read(".1"), "aaa\nbbb\n");
    }

    #[test]
    fn keeping_nothing_still_bounds_the_file() {
        let scratch = Scratch::new("keep-none");
        let sink =
            Sink::open(SinkConfig::new(scratch.log()).with_stderr(false).with_max_bytes(8).with_keep(0))
                .expect("open");

        sink.write_line("aaa");
        sink.write_line("bbb");
        sink.write_line("ccc");

        assert_eq!(scratch.read(""), "ccc\n", "the current line survives");
        let first = PathBuf::from(format!("{}.1", scratch.log().display()));
        assert!(!first.exists(), "keep = 0 must not create history");
    }

    #[test]
    fn an_existing_file_is_appended_to_not_truncated() {
        // A second supra process, or a restart, must not erase the previous session.
        let scratch = Scratch::new("append");
        {
            let sink = sink(&scratch);
            sink.write_line("from the first session");
        }
        {
            let sink = sink(&scratch);
            sink.write_line("from the second");
        }
        let written = scratch.read("");
        assert!(written.contains("from the first session"), "{written}");
        assert!(written.contains("from the second"), "{written}");
    }

    #[test]
    fn an_existing_file_counts_toward_the_rotation_size() {
        // Without reading the length at open, a restart would reset the counter and the
        // file would grow without bound across sessions.
        let scratch = Scratch::new("resume-size");
        fs::write(scratch.log(), "x".repeat(64)).expect("seed");

        let sink =
            Sink::open(SinkConfig::new(scratch.log()).with_stderr(false).with_max_bytes(32).with_keep(1))
                .expect("open");
        sink.write_line("triggers rotation immediately");

        assert_eq!(scratch.read(""), "triggers rotation immediately\n");
        assert!(scratch.read(".1").starts_with("xxx"), "the seeded content was rotated");
    }

    #[test]
    fn stderr_mirroring_is_suppressed_for_the_life_of_the_guard() {
        let scratch = Scratch::new("guard");
        let sink = Sink::open(SinkConfig::new(scratch.log()).with_stderr(true)).expect("open");

        assert!(sink.mirrors_to_stderr(), "mirroring is on before the TUI starts");
        {
            let _guard = sink.suppress_stderr();
            assert!(!sink.mirrors_to_stderr(), "the TUI owns the terminal");
        }
        assert!(sink.mirrors_to_stderr(), "and gets it back on drop");
    }

    #[test]
    fn a_line_written_under_the_guard_still_reaches_the_file() {
        // Suppression is about the terminal, not about the record. Losing diagnostics
        // for the whole time the TUI is up would defeat the point of having them.
        let scratch = Scratch::new("guarded-write");
        let sink = Sink::open(SinkConfig::new(scratch.log()).with_stderr(true)).expect("open");
        let guard = sink.suppress_stderr();
        sink.write_line("while the tui is up");
        drop(guard);
        assert_eq!(scratch.read(""), "while the tui is up\n");
    }

    #[test]
    fn mirroring_stays_off_when_it_was_never_wanted() {
        let scratch = Scratch::new("never");
        let sink = Sink::open(SinkConfig::new(scratch.log()).with_stderr(false)).expect("open");
        assert!(!sink.mirrors_to_stderr());
        let _guard = sink.suppress_stderr();
        assert!(!sink.mirrors_to_stderr());
    }

    #[test]
    fn a_lost_line_is_reported_on_the_next_one_that_succeeds() {
        // A gap in a log is only debuggable if the log says there is one.
        let scratch = Scratch::new("lost");
        let sink = sink(&scratch);
        sink.write_line("before");

        // Set the count directly to exercise the notice. The *counting* itself is
        // exercised for real by the two tests below.
        sink.dropped.store(2, Ordering::Relaxed);
        assert_eq!(sink.dropped_lines(), 2);

        sink.write_line("after");
        let written = scratch.read("");
        assert!(written.contains("2 log line(s) could not be written"), "{written}");
        assert!(written.contains("after"), "{written}");
        assert_eq!(sink.dropped_lines(), 0, "the notice clears the count");
    }

    #[test]
    #[cfg(unix)]
    fn a_failing_write_is_counted_rather_than_forgotten() {
        // A real, deterministic write failure: `/dev/full` accepts an open and answers
        // every write with ENOSPC, which is exactly what a full disk produces.
        //
        // Without this the counting path had no coverage at all - a mutation deleting
        // the increment survived, because the only test that touched `dropped` set it
        // by hand.
        let full = Path::new("/dev/full");
        if !full.exists() {
            eprintln!("skipped: /dev/full is unavailable");
            return;
        }
        let Ok(sink) = Sink::open(SinkConfig::new(full).with_stderr(false)) else {
            eprintln!("skipped: cannot open /dev/full");
            return;
        };

        assert_eq!(sink.dropped_lines(), 0);
        sink.write_line("this cannot be written");
        assert_eq!(sink.dropped_lines(), 1, "a failed write must be counted");

        sink.write_line("nor can this");
        assert_eq!(sink.dropped_lines(), 2, "and the count must accumulate");
    }

    #[test]
    #[cfg(unix)]
    fn a_pending_count_survives_a_further_failure() {
        // The subtle half. The notice is composed before the write, which clears the
        // count - so a failure has to put the old count *back* along with the new one.
        // Losing it there would mean a gap that is never reported once a second failure
        // follows the first.
        let full = Path::new("/dev/full");
        if !full.exists() {
            eprintln!("skipped: /dev/full is unavailable");
            return;
        }
        let Ok(sink) = Sink::open(SinkConfig::new(full).with_stderr(false)) else {
            eprintln!("skipped: cannot open /dev/full");
            return;
        };

        sink.dropped.store(5, Ordering::Relaxed);
        sink.write_line("also fails");
        assert_eq!(sink.dropped_lines(), 6, "five pending plus this one");
    }

    #[test]
    fn concurrent_writers_produce_whole_lines() {
        // The O_APPEND property, exercised across threads. Interleaving would show up
        // as a line that is not one of the ones written.
        let scratch = Scratch::new("threads");
        let sink = std::sync::Arc::new(sink(&scratch));

        let handles: Vec<_> = (0..8)
            .map(|thread| {
                let sink = std::sync::Arc::clone(&sink);
                std::thread::spawn(move || {
                    for line in 0..50 {
                        sink.write_line(&format!("thread-{thread}-line-{line:02}"));
                    }
                })
            })
            .collect();
        for handle in handles {
            handle.join().expect("thread");
        }

        let written = scratch.read("");
        let lines: Vec<&str> = written.lines().collect();
        assert_eq!(lines.len(), 400, "every line must arrive exactly once");
        for line in lines {
            assert!(line.starts_with("thread-") && line.contains("-line-"), "a line was torn: {line:?}");
        }
    }

    #[test]
    #[cfg(unix)]
    fn a_fifo_log_target_is_refused_rather_than_hanging() {
        // A real bug, found by an audit after T8 shipped. Opening a FIFO for *writing*
        // blocks until a reader appears, and the sink opens at startup - so pointing the
        // log at a pipe hung the process before any UI existed to explain why. T7 already
        // guarded the equivalent on its read path; T8 had not.
        //
        // If this test ever hangs rather than fails, the pre-flight check has been
        // removed. `scripts/mutate.sh` wraps the Rust branch in `timeout` for exactly
        // this shape of regression.
        let scratch = Scratch::new("fifo-target");
        let fifo = scratch.log();
        let made =
            std::process::Command::new("mkfifo").arg(&fifo).status().is_ok_and(|status| status.success());
        if !made {
            eprintln!("skipped: mkfifo unavailable");
            return;
        }

        let error = Sink::open(SinkConfig::new(&fifo).with_stderr(false))
            .expect_err("a FIFO must be refused, not opened");
        let text = error.to_string();
        assert!(text.contains("FIFO"), "{text}");
        assert!(text.contains("hang startup"), "the reason must be stated: {text}");
    }

    #[test]
    #[cfg(unix)]
    fn a_character_device_is_still_a_valid_target() {
        // The complement: only FIFOs are refused. `/dev/null` is a legitimate "discard
        // the log" target, and rejecting the whole non-regular class would break it - and
        // would break the /dev/full test that covers the failure path.
        let null = Path::new("/dev/null");
        if !null.exists() {
            eprintln!("skipped: /dev/null is unavailable");
            return;
        }
        let sink =
            Sink::open(SinkConfig::new(null).with_stderr(false)).expect("/dev/null must remain usable");
        sink.write_line("discarded");
        assert_eq!(sink.dropped_lines(), 0, "writing to /dev/null succeeds");
    }

    #[test]
    fn the_path_is_reported_so_a_reader_can_be_told_where_to_look() {
        let scratch = Scratch::new("path");
        let sink = sink(&scratch);
        assert_eq!(sink.path(), scratch.log().as_path());
    }
}
