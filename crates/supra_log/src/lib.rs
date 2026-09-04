//! Structured diagnostics for supra-harness.
//!
//! **T8** of the stage sequence, and the one crate in the workspace permitted to write
//! to stderr. `print_stderr` is banned workspace-wide so that every diagnostic arrives
//! here, in the same way `unsafe_code` is banned everywhere but T5.
//!
//! # Three guarantees
//!
//! **A secret never reaches the file.** Redaction happens at the sink, over the whole
//! formatted line, after every call site has had its say. T7 removed every field that
//! could hold a credential from the configuration schema, and a test still found a leak:
//! the error *refusing* a pasted credential quoted it. The lesson was that secrets arrive
//! through paths nobody enumerated, so the net goes at the last moment before bytes become
//! durable. See [`redact`] for what is caught, and for the two things that must survive it.
//!
//! **The log is bounded.** Size-based rotation with a fixed number of kept files, so a
//! harness running for days cannot fill a disk. See [`sink`].
//!
//! **stderr belongs to whoever owns the terminal.** While the TUI is up, a stray line
//! would tear the frame, so mirroring is suppressed for the lifetime of a guard - not by
//! a pair of setters whose second half can be forgotten.
//!
//! # Why this crate does not depend on T7
//!
//! `supra_config` comes earlier in the stage order and it would be natural to take a
//! resolved `Config` here. It would also be a mistake: configuration loading is exactly
//! the moment you need diagnostics, and a logger that cannot start until configuration
//! has loaded cannot report why configuration failed to load. [`LogOptions`] is plain
//! data, and T30 is where a config is translated into it.
//!
//! # Usage
//!
//! ```no_run
//! use supra_log::{LogOptions, init};
//!
//! let handle = init(LogOptions::default())?;
//! tracing::info!(target: "supra", "started");
//!
//! // While the TUI owns the terminal, hold this guard.
//! let quiet = handle.sink().suppress_stderr();
//! drop(quiet);
//! # Ok::<(), supra_log::LogError>(())
//! ```

#![deny(missing_docs)]
#![forbid(unsafe_code)]
// Tests assert with `.expect()` and `panic!`, and report environment-dependent skips on
// stderr. Scoped to `cfg(test)` so no allow reaches a shipped path.
#![cfg_attr(test, allow(clippy::expect_used, clippy::panic, clippy::print_stderr))]

pub mod redact;
pub mod sink;
pub mod subscriber;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use tracing::level_filters::LevelFilter;

pub use redact::redact;
pub use sink::{DEFAULT_KEEP, DEFAULT_MAX_BYTES, Sink, SinkConfig, StderrGuard};
pub use subscriber::{Format, SinkWriterFactory, build};

/// Environment variable holding filter directives.
///
/// A dedicated name rather than `RUST_LOG`, so a directive left over from working on an
/// unrelated Rust project does not silently change what supra records.
pub const ENV_FILTER_VAR: &str = "SUPRA_LOG";

/// Directory name used under the state root.
pub const LOG_DIR: &str = "supra";

/// File name for the current log.
pub const LOG_FILE: &str = "supra.log";

/// Why logging could not be started.
#[derive(Debug, thiserror::Error)]
pub enum LogError {
    /// The log file could not be created or opened.
    ///
    /// Fatal on purpose. A harness that cannot record what it did should say so at
    /// startup rather than discover it during an incident.
    #[error("cannot open the log at {}: {source}", path.display())]
    Sink {
        /// The file that could not be opened.
        path: PathBuf,
        /// The underlying I/O failure.
        source: std::io::Error,
    },

    /// A global subscriber was already installed.
    ///
    /// A process may install one once. Reported rather than ignored: a second `init`
    /// means two components each believe they own diagnostics, and whichever lost would
    /// be writing nowhere.
    #[error("a global tracing subscriber is already installed; init may only be called once")]
    AlreadyInstalled,

    /// No writable state directory could be determined.
    #[error(
        "cannot determine where to write the log: neither XDG_STATE_HOME nor HOME is set; \
         pass an explicit path"
    )]
    NoStateDirectory,
}

/// How logging is set up.
#[derive(Clone, Debug)]
pub struct LogOptions {
    /// Where the log goes and how much is kept. `None` uses [`default_log_path`].
    pub sink: Option<SinkConfig>,
    /// How events are rendered.
    pub format: Format,
    /// Variable holding filter directives.
    pub env_var: String,
    /// Level applied when that variable is unset.
    pub default_level: LevelFilter,
}

impl Default for LogOptions {
    fn default() -> Self {
        Self {
            sink: None,
            format: Format::default(),
            env_var: ENV_FILTER_VAR.to_owned(),
            default_level: LevelFilter::INFO,
        }
    }
}

impl LogOptions {
    /// Write to `path` instead of the default location.
    #[must_use]
    pub fn with_path(mut self, path: impl Into<PathBuf>) -> Self {
        self.sink = Some(SinkConfig::new(path));
        self
    }

    /// Use an explicit sink configuration.
    #[must_use]
    pub fn with_sink(mut self, config: SinkConfig) -> Self {
        self.sink = Some(config);
        self
    }

    /// Render events in `format`.
    #[must_use]
    pub const fn with_format(mut self, format: Format) -> Self {
        self.format = format;
        self
    }

    /// Apply `level` when the filter variable is unset.
    #[must_use]
    pub const fn with_default_level(mut self, level: LevelFilter) -> Self {
        self.default_level = level;
        self
    }
}

/// A running logger.
///
/// Holds the sink so a caller can suppress stderr while the TUI owns the terminal, and
/// so the log's path can be shown to a user who needs to attach it to a report.
#[derive(Clone, Debug)]
pub struct LogHandle {
    sink: Arc<Sink>,
}

impl LogHandle {
    /// The sink behind this logger.
    #[must_use]
    pub const fn sink(&self) -> &Arc<Sink> {
        &self.sink
    }

    /// The file being written.
    #[must_use]
    pub fn path(&self) -> &Path {
        self.sink.path()
    }
}

/// Default log location.
///
/// `$XDG_STATE_HOME/supra/supra.log`, else `$HOME/.local/state/supra/supra.log`.
///
/// State rather than cache, because a cache directory is something a user or a cleanup
/// tool may delete at any time and a log's value is entirely historical. A relative
/// `XDG_STATE_HOME` is ignored rather than resolved, for the same reason T7 ignores a
/// relative `XDG_CONFIG_HOME`: resolving it would make the log's location depend on
/// where supra was started.
///
/// # Errors
///
/// [`LogError::NoStateDirectory`] when neither variable gives an absolute path.
pub fn default_log_path() -> Result<PathBuf, LogError> {
    let base = match std::env::var_os("XDG_STATE_HOME") {
        Some(value) if !value.is_empty() && Path::new(&value).is_absolute() => PathBuf::from(value),
        _ => {
            let home =
                std::env::var_os("HOME").filter(|home| !home.is_empty()).ok_or(LogError::NoStateDirectory)?;
            PathBuf::from(home).join(".local").join("state")
        }
    };
    Ok(base.join(LOG_DIR).join(LOG_FILE))
}

/// Open the sink and install a global subscriber.
///
/// # Errors
///
/// [`LogError::Sink`] when the file cannot be opened, [`LogError::AlreadyInstalled`]
/// when a subscriber is already in place, [`LogError::NoStateDirectory`] when no default
/// path can be determined.
pub fn init(options: LogOptions) -> Result<LogHandle, LogError> {
    let config = match options.sink {
        Some(config) => config,
        None => SinkConfig::new(default_log_path()?),
    };

    let path = config.path.clone();
    let sink = Arc::new(Sink::open(config).map_err(|source| LogError::Sink { path, source })?);

    let subscriber = build(Arc::clone(&sink), options.format, &options.env_var, options.default_level);

    tracing::subscriber::set_global_default(subscriber).map_err(|_| LogError::AlreadyInstalled)?;

    Ok(LogHandle { sink })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    struct Scratch(PathBuf);

    impl Scratch {
        fn new(name: &str) -> Self {
            let path = std::env::temp_dir().join(format!("supra-log-init-{name}"));
            let _ = fs::remove_dir_all(&path);
            fs::create_dir_all(&path).expect("scratch directory");
            Self(path)
        }

        fn log(&self) -> PathBuf {
            self.0.join("supra.log")
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn the_defaults_are_the_documented_ones() {
        let options = LogOptions::default();
        assert!(options.sink.is_none(), "the default path is resolved at init");
        assert_eq!(options.format, Format::Json, "structured by default");
        assert_eq!(options.env_var, "SUPRA_LOG");
        assert_eq!(options.default_level, LevelFilter::INFO);
    }

    #[test]
    fn the_filter_variable_is_not_rust_log() {
        // A directive left over from an unrelated project must not silently change what
        // supra records.
        assert_ne!(ENV_FILTER_VAR, "RUST_LOG");
        assert_eq!(ENV_FILTER_VAR, "SUPRA_LOG");
    }

    #[test]
    fn the_default_path_lands_under_state_not_cache() {
        // A cache directory is something a user or a cleanup tool may delete at any
        // time, and a log's whole value is historical.
        match default_log_path() {
            Ok(path) => {
                let text = path.to_string_lossy().into_owned();
                assert!(text.ends_with("supra/supra.log"), "{text}");
                assert!(!text.contains("/cache/"), "{text}");
            }
            Err(LogError::NoStateDirectory) => {
                eprintln!("skipped: neither XDG_STATE_HOME nor HOME is set");
            }
            Err(other) => panic!("unexpected error: {other}"),
        }
    }

    #[test]
    fn options_build_the_sink_that_was_asked_for() {
        let scratch = Scratch::new("options");
        let options = LogOptions::default()
            .with_sink(SinkConfig::new(scratch.log()).with_max_bytes(1024).with_keep(1))
            .with_format(Format::Compact)
            .with_default_level(LevelFilter::DEBUG);

        assert_eq!(options.format, Format::Compact);
        assert_eq!(options.default_level, LevelFilter::DEBUG);

        let sink = options.sink.expect("set");
        assert_eq!(sink.path, scratch.log());
        assert_eq!(sink.max_bytes, 1024);
        assert_eq!(sink.keep, 1);
    }

    #[test]
    fn with_path_is_shorthand_for_a_default_sink_at_that_path() {
        let scratch = Scratch::new("with-path");
        let options = LogOptions::default().with_path(scratch.log());
        let sink = options.sink.expect("set");
        assert_eq!(sink.path, scratch.log());
        assert_eq!(sink.max_bytes, DEFAULT_MAX_BYTES);
        assert_eq!(sink.keep, DEFAULT_KEEP);
    }

    #[test]
    fn a_handle_reports_where_the_log_is() {
        // What a user is told when asked to attach the log to a report.
        let scratch = Scratch::new("handle");
        let sink = Arc::new(Sink::open(SinkConfig::new(scratch.log()).with_stderr(false)).expect("open"));
        let handle = LogHandle { sink };
        assert_eq!(handle.path(), scratch.log().as_path());
        assert!(!handle.sink().mirrors_to_stderr());
    }

    #[test]
    fn an_unopenable_path_is_a_startup_error_naming_the_file() {
        // A directory where a file belongs. Fatal by design: a harness that cannot
        // record what it did should say so now rather than during an incident.
        let scratch = Scratch::new("unopenable");
        let as_directory = scratch.log();
        fs::create_dir_all(&as_directory).expect("directory in the file's place");

        let error =
            init(LogOptions::default().with_path(&as_directory)).expect_err("a directory is not a log file");
        match error {
            LogError::Sink { path, .. } => assert_eq!(path, as_directory),
            other => panic!("expected a sink error, got {other}"),
        }
    }

    #[test]
    fn init_installs_once_and_says_so_the_second_time() {
        // `set_global_default` succeeds once per process. This test therefore also
        // covers the success path, and is the only test in the crate that installs a
        // global subscriber - which is why every other test uses `build` with
        // `with_default` instead.
        let scratch = Scratch::new("install");
        let first = init(LogOptions::default().with_sink(SinkConfig::new(scratch.log()).with_stderr(false)));

        match first {
            Ok(handle) => {
                tracing::info!(target: "supra_log_test", "installed");
                assert_eq!(handle.path(), scratch.log().as_path());

                let second = init(LogOptions::default().with_path(scratch.0.join("other.log")));
                assert!(
                    matches!(second, Err(LogError::AlreadyInstalled)),
                    "a second install must be refused"
                );
            }
            Err(LogError::AlreadyInstalled) => {
                // Another test in this binary installed first. The refusal is the
                // property under test either way.
                eprintln!("skipped: a subscriber was already installed");
            }
            Err(other) => panic!("unexpected error: {other}"),
        }
    }
}
