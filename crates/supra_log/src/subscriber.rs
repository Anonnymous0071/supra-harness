//! The `tracing` wiring: filter, format, and the adapter onto the sink.
//!
//! Kept apart from [`crate::sink`] deliberately. The sink is plain I/O plus redaction
//! and is tested as such - no subscriber, no global state, no macros. This module is
//! the only place that knows about `tracing`, so a change in how events are formatted
//! cannot disturb the guarantees about what reaches disk.
//!
//! # Whole-event granularity
//!
//! Redaction is only sound if it sees a complete line. The `fmt` layer calls
//! `make_writer_for` once per event and writes the whole formatted event with a single
//! `write_all`, so a writer that accumulates and emits when it is dropped observes
//! exactly one event at a time. That was read out of the layer's source rather than
//! assumed, because a writer that saw half an event could pass half a credential.
//!
//! # Installing is separate from building
//!
//! A process may set a global subscriber once. [`build`] returns a subscriber without
//! installing it, which is what makes this module testable: a test can scope one with
//! `tracing::subscriber::with_default` and assert on the file afterwards, as many times
//! as it likes.

use std::io;
use std::sync::Arc;

use tracing::Subscriber;
use tracing::level_filters::LevelFilter;
use tracing_subscriber::fmt::MakeWriter;
use tracing_subscriber::layer::SubscriberExt as _;
use tracing_subscriber::{EnvFilter, Layer as _};

use crate::sink::Sink;

/// How events are rendered.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Format {
    /// One JSON object per line: the default, because a log that cannot be queried is
    /// a log that gets read once and then ignored.
    #[default]
    Json,
    /// A compact human-readable line, for reading a startup failure without a tool.
    Compact,
}

/// Builds a per-event writer over a shared sink.
///
/// A local newtype rather than an impl on `Arc<Sink>` directly: `Arc` is not a
/// fundamental type, so implementing a foreign trait for `Arc<LocalType>` falls foul
/// of the orphan rule.
#[derive(Clone, Debug)]
pub struct SinkWriterFactory(Arc<Sink>);

impl SinkWriterFactory {
    /// Wrap a sink so the `fmt` layer can write to it.
    #[must_use]
    pub const fn new(sink: Arc<Sink>) -> Self {
        Self(sink)
    }
}

impl<'a> MakeWriter<'a> for SinkWriterFactory {
    type Writer = SinkWriter<'a>;

    fn make_writer(&'a self) -> Self::Writer {
        SinkWriter { sink: &self.0, buffer: Vec::new() }
    }
}

/// Accumulates one formatted event, then hands it to the sink as a single line.
pub struct SinkWriter<'a> {
    sink: &'a Sink,
    buffer: Vec<u8>,
}

impl SinkWriter<'_> {
    /// Hand the accumulated event to the sink.
    ///
    /// Idempotent: the buffer is cleared, so a `flush` followed by the drop does not
    /// write the event twice.
    fn emit(&mut self) {
        if self.buffer.is_empty() {
            return;
        }
        let line = std::mem::take(&mut self.buffer);
        match std::str::from_utf8(&line) {
            Ok(text) => self.sink.write_line(text),
            // A formatter that produced invalid UTF-8 is a bug, but losing the event
            // silently would be worse than reporting it lossily - and redaction still
            // runs over the replacement text.
            Err(_) => self.sink.write_line(&String::from_utf8_lossy(&line)),
        }
    }
}

impl io::Write for SinkWriter<'_> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        // Always consumes everything, so `write_all` never splits an event across two
        // calls on the strength of a short write.
        self.buffer.extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.emit();
        Ok(())
    }
}

impl Drop for SinkWriter<'_> {
    fn drop(&mut self) {
        self.emit();
    }
}

/// Build a subscriber writing to `sink`, without installing it.
///
/// `env_var` names the variable holding filter directives, in the usual
/// `target=level` syntax. A directive there that does not parse is skipped rather than
/// fatal: a typo in a debugging variable should not stop the harness from starting.
///
/// # Why the default is a typed level and not a string
///
/// `default_level` is a [`LevelFilter`] rather than a `&str`, and that is not
/// convenience. Almost any string parses as a *valid* directive, because a bare word is
/// read as a **target name**: `"not a level"` parses successfully into
/// `not a level=trace`, and so would `"inf"`. A typo in a string default would
/// therefore not fail - it would silently produce a filter that enables trace for a
/// target nothing logs to and silences everything else, which is the worst possible
/// outcome for a diagnostic channel. Verified against
/// `tracing_subscriber::filter::Directive` after a test caught it, rather than assumed.
///
/// A typed level cannot be misspelled.
#[must_use]
pub fn build(
    sink: Arc<Sink>,
    format: Format,
    env_var: &str,
    default_level: LevelFilter,
) -> impl Subscriber + Send + Sync {
    let filter = EnvFilter::builder()
        .with_env_var(env_var)
        .with_default_directive(default_level.into())
        .from_env_lossy();

    let writer = SinkWriterFactory::new(sink);
    let registry = tracing_subscriber::registry().with(filter);

    // The two formats produce different layer types, so the branch has to happen
    // before composition and each arm has to be boxed into the same shape.
    match format {
        Format::Json => {
            let layer = tracing_subscriber::fmt::layer().json().with_ansi(false).with_writer(writer).boxed();
            registry.with(layer)
        }
        Format::Compact => {
            let layer = tracing_subscriber::fmt::layer()
                // No ANSI: the sink may be a file, and a terminal that wants colour is
                // T29's business rather than the diagnostic channel's.
                .with_ansi(false)
                .with_target(true)
                .with_writer(writer)
                .boxed();
            registry.with(layer)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sink::SinkConfig;
    use std::fs;
    use std::path::PathBuf;

    struct Scratch(PathBuf);

    impl Scratch {
        fn new(name: &str) -> Self {
            let path = std::env::temp_dir().join(format!("supra-log-sub-{name}"));
            let _ = fs::remove_dir_all(&path);
            fs::create_dir_all(&path).expect("scratch directory");
            Self(path)
        }

        fn log(&self) -> PathBuf {
            self.0.join("supra.log")
        }

        fn read(&self) -> String {
            fs::read_to_string(self.log()).unwrap_or_default()
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn sink(scratch: &Scratch) -> Arc<Sink> {
        Arc::new(Sink::open(SinkConfig::new(scratch.log()).with_stderr(false)).expect("open"))
    }

    #[test]
    fn an_event_becomes_one_json_line() {
        let scratch = Scratch::new("json");
        let sink = sink(&scratch);
        let subscriber = build(Arc::clone(&sink), Format::Json, "SUPRA_LOG_TEST_JSON", LevelFilter::INFO);

        tracing::subscriber::with_default(subscriber, || {
            tracing::info!(turn = 7, "turn started");
        });

        let written = scratch.read();
        assert_eq!(written.lines().count(), 1, "one event, one line: {written}");

        let parsed: serde_json::Value =
            serde_json::from_str(written.trim()).expect("the line must be valid JSON");
        assert_eq!(parsed["level"], "INFO");
        assert_eq!(parsed["fields"]["message"], "turn started");
        assert_eq!(parsed["fields"]["turn"], 7);
    }

    #[test]
    fn the_compact_format_is_readable_without_a_tool() {
        let scratch = Scratch::new("compact");
        let sink = sink(&scratch);
        let subscriber =
            build(Arc::clone(&sink), Format::Compact, "SUPRA_LOG_TEST_COMPACT", LevelFilter::INFO);

        tracing::subscriber::with_default(subscriber, || {
            tracing::warn!("disk is filling");
        });

        let written = scratch.read();
        assert_eq!(written.lines().count(), 1, "{written}");
        assert!(written.contains("WARN"), "{written}");
        assert!(written.contains("disk is filling"), "{written}");
        assert!(!written.contains("\u{1b}["), "no escape sequences in a file: {written:?}");
    }

    #[test]
    fn a_credential_in_a_field_never_reaches_the_file() {
        // The end-to-end property. A caller that logs a secret - by mistake, in a
        // message, in a field - still produces a file with no secret in it.
        let scratch = Scratch::new("redacted");
        let sink = sink(&scratch);
        let subscriber = build(Arc::clone(&sink), Format::Json, "SUPRA_LOG_TEST_RED", LevelFilter::INFO);

        tracing::subscriber::with_default(subscriber, || {
            tracing::error!(api_key = "hunter2", "authentication failed for sk-ABCDEFGHIJKLMNOPQRSTUVWX");
        });

        let written = scratch.read();
        assert!(!written.contains("hunter2"), "a named field leaked: {written}");
        assert!(!written.contains("ABCDEFGHIJKLMNOPQRSTUVWX"), "a message leaked: {written}");
        assert!(written.contains("authentication failed"), "context must survive: {written}");
        // Still one well-formed line after redaction.
        assert_eq!(written.lines().count(), 1, "{written}");
        serde_json::from_str::<serde_json::Value>(written.trim()).expect("redaction must not break the JSON");
    }

    #[test]
    fn the_filter_default_applies_when_the_variable_is_unset() {
        let scratch = Scratch::new("filter-default");
        let sink = sink(&scratch);
        // A variable name nothing will have set.
        let subscriber =
            build(Arc::clone(&sink), Format::Json, "SUPRA_LOG_TEST_UNSET_XYZ", LevelFilter::WARN);

        tracing::subscriber::with_default(subscriber, || {
            tracing::info!("should be filtered out");
            tracing::warn!("should be kept");
        });

        let written = scratch.read();
        assert!(!written.contains("filtered out"), "{written}");
        assert!(written.contains("should be kept"), "{written}");
    }

    #[test]
    fn the_typed_default_gates_at_every_level() {
        // Replaces a test that asserted a fallback which cannot occur. A bare word
        // parses as a target name rather than failing, so a string default would have
        // silenced everything instead of falling back - which is why the parameter is
        // a `LevelFilter`.
        for (level, expect_debug, expect_info) in [
            (LevelFilter::DEBUG, true, true),
            (LevelFilter::INFO, false, true),
            (LevelFilter::WARN, false, false),
        ] {
            let scratch = Scratch::new(&format!("level-{level}"));
            let sink = sink(&scratch);
            let subscriber = build(Arc::clone(&sink), Format::Json, "SUPRA_LOG_TEST_LEVELS", level);

            tracing::subscriber::with_default(subscriber, || {
                tracing::debug!("a debug line");
                tracing::info!("an info line");
                tracing::warn!("a warn line");
            });

            let written = scratch.read();
            assert_eq!(written.contains("a debug line"), expect_debug, "{level}: {written}");
            assert_eq!(written.contains("an info line"), expect_info, "{level}: {written}");
            assert!(written.contains("a warn line"), "warn is never filtered: {written}");
        }
    }

    #[test]
    fn several_events_become_several_whole_lines() {
        let scratch = Scratch::new("many");
        let sink = sink(&scratch);
        let subscriber = build(Arc::clone(&sink), Format::Json, "SUPRA_LOG_TEST_MANY", LevelFilter::INFO);

        tracing::subscriber::with_default(subscriber, || {
            for index in 0..25 {
                tracing::info!(index, "event");
            }
        });

        let written = scratch.read();
        assert_eq!(written.lines().count(), 25);
        for line in written.lines() {
            serde_json::from_str::<serde_json::Value>(line)
                .unwrap_or_else(|error| panic!("torn line {line:?}: {error}"));
        }
    }

    #[test]
    fn flush_then_drop_writes_the_event_once() {
        // `flush` and `Drop` both emit, so the buffer has to be cleared by whichever
        // runs first or every event would be duplicated.
        use std::io::Write as _;
        let scratch = Scratch::new("once");
        let sink = sink(&scratch);
        let factory = SinkWriterFactory::new(Arc::clone(&sink));

        {
            let mut writer = factory.make_writer();
            writer.write_all(b"only once\n").expect("write");
            writer.flush().expect("flush");
        }

        assert_eq!(scratch.read(), "only once\n");
    }

    #[test]
    fn an_event_split_across_writes_is_still_one_line() {
        // The property redaction depends on: a secret split across two `write` calls
        // must not escape, so the writer must not emit until it has the whole event.
        use std::io::Write as _;
        let scratch = Scratch::new("split");
        let sink = sink(&scratch);
        let factory = SinkWriterFactory::new(Arc::clone(&sink));

        {
            let mut writer = factory.make_writer();
            writer.write_all(b"key sk-ABCDEFGH").expect("first half");
            writer.write_all(b"IJKLMNOPQRSTUVWX end\n").expect("second half");
        }

        let written = scratch.read();
        assert_eq!(written.lines().count(), 1, "{written}");
        assert!(!written.contains("ABCDEFGHIJKLMNOPQRSTUVWX"), "the split secret escaped: {written}");
        assert!(written.contains("end"), "{written}");
    }

    #[test]
    fn a_writer_that_saw_nothing_writes_nothing() {
        let scratch = Scratch::new("empty");
        let sink = sink(&scratch);
        let factory = SinkWriterFactory::new(Arc::clone(&sink));
        drop(factory.make_writer());
        assert_eq!(scratch.read(), "", "an unused writer must not emit a blank line");
    }
}
