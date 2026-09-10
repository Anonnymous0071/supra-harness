use std::path::Path;
use std::process::Command;

use crate::error::IntrospectError;
use crate::finding::{Finding, Kind};

/// One gate run: the command, its capture, and the findings it yielded.
#[derive(Debug)]
pub struct GateRun {
    /// Whether the gate's process exited zero.
    pub passed: bool,
    /// The findings the gate produced, parsed from its output.
    pub findings: Vec<Finding>,
}

/// Parse a `cargo`-family diagnostic line into a finding.
///
/// Two formats are recognised: the `--> path:line:col` spelling cargo uses
/// for test failures, and the `path:line:col: level:` short spelling the
/// static gate asks clippy for. The parser is line-oriented and lenient: a
/// diagnostic without a location still yields an unlocated finding, because
/// a finding the reporter cannot place is still a finding.
#[must_use]
pub fn parse_cargo_diagnostic(kind: Kind, source: &'static str, line: &str) -> Option<Finding> {
    let trimmed = line.trim_start();
    if let Some(rest) = trimmed.strip_prefix("--> ") {
        let mut parts = rest.split(':');
        let path = parts.next()?.trim();
        let line_number = parts.next().and_then(|text| text.parse::<u32>().ok());
        if path.is_empty() {
            return None;
        }
        return Some(Finding::new(
            kind,
            source,
            format!("{source} diagnostic at {path}"),
            Some(path.to_owned()),
            line_number,
        ));
    }
    let short_at = trimmed
        .match_indices(": warning: ")
        .chain(trimmed.match_indices(": error"))
        .filter_map(|(at, _)| {
            let after = &trimmed[at..];
            (after.starts_with(": warning: ")
                || after.starts_with(": error:")
                || after.starts_with(": error["))
            .then_some(at)
        })
        .max();
    if let Some(at) = short_at {
        let mut parts = trimmed[..at].split(':');
        let path = parts.next()?.trim();
        let line_number = parts.next().and_then(|text| text.parse::<u32>().ok());
        if path.is_empty() {
            return None;
        }
        return Some(Finding::new(
            kind,
            source,
            format!("{source} diagnostic at {path}"),
            Some(path.to_owned()),
            line_number,
        ));
    }
    if let Some(rest) = trimmed.strip_prefix("error") {
        if rest.starts_with(['[', ':', ' ']) && !rest.contains("no test target") {
            let summary = format!("{source}: error{}", rest.trim_end());
            return Some(Finding::new(kind, source, summary, None, None));
        }
    }
    None
}

/// Run one external gate over the workspace and parse its diagnostics.
///
/// The child runs in its own process group and its output is capped:
/// a gate that hangs (a grandchild holding the pipe), floods, or dies
/// mid-run is bounded here rather than taking the turn loop with it.
///
/// # Errors
///
/// [`IntrospectError::Spawn`] when the command cannot start - a gate that
/// cannot run is a refusal, not a pass; the turn loop escalates on it.
pub fn run_gate(
    kind: Kind,
    source: &'static str,
    program: &str,
    args: &[&str],
    cwd: &Path,
) -> Result<GateRun, IntrospectError> {
    let mut command = Command::new(program);
    command
        .args(args)
        .current_dir(cwd)
        .env("CARGO_TERM_COLOR", "never")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt as _;
        command.process_group(0);
    }
    let mut child = command.spawn().map_err(|error| IntrospectError::Spawn(format!("{program}: {error}")))?;

    let piped: Vec<Box<dyn std::io::Read + Send>> = [
        child.stdout.take().map(|stream| Box::new(stream) as Box<dyn std::io::Read + Send>),
        child.stderr.take().map(|stream| Box::new(stream) as Box<dyn std::io::Read + Send>),
    ]
    .into_iter()
    .flatten()
    .collect();

    let readers: Vec<_> = piped
        .into_iter()
        .map(|stream| {
            let (sender, receiver) = std::sync::mpsc::channel();
            std::thread::spawn(move || {
                let captured = capture_capped(stream, GATE_OUTPUT_CAP);
                let _ = sender.send(captured);
            });
            receiver
        })
        .collect();

    let status = match child.wait() {
        Ok(status) => status,
        Err(error) => {
            kill_group(&mut child);
            return Err(IntrospectError::Spawn(format!("{program}: {error}")));
        }
    };

    let mut streams = Vec::with_capacity(readers.len());
    for receiver in readers {
        if let Ok(text) = receiver.recv_timeout(GATE_QUIESCE) {
            streams.push(text);
            continue;
        }
        kill_group(&mut child);
        let text = receiver.recv().unwrap_or_default();
        streams.push(text);
    }

    let mut findings = Vec::new();
    for stream in &streams {
        for line in stream.lines() {
            if let Some(finding) = parse_cargo_diagnostic(kind, source, line) {
                findings.push(finding);
            }
        }
    }
    findings.dedup_by(|a, b| a.summary == b.summary && a.path == b.path && a.line == b.line);
    Ok(GateRun { passed: status.success(), findings })
}

/// How much of one stream a gate keeps before it only drains.
///
/// A gate parses diagnostics, and diagnostics live in the first pages of
/// output; the megabytes a pathological build can emit are noise either
/// way, and buffering them all is how a gate turns into a memory
/// failure.
const GATE_OUTPUT_CAP: usize = 4 * 1024 * 1024;

/// How long readers wait for the process group to quiet down after the
/// direct child exits.
///
/// A grandchild that inherited stdout keeps the pipe open after cargo
/// itself is gone; without a bound, the gate would wait for it forever.
/// Killing the group ends the wait, and a builder that already reported
/// its status has nothing left to say.
const GATE_QUIESCE: std::time::Duration = std::time::Duration::from_secs(5);

/// Kill the child's whole process group, grandchildren included.
fn kill_group(child: &mut std::process::Child) {
    supra_ffi::process::kill_process_group(child.id());
    let _ = child.kill();
    let _ = child.wait();
}

/// Drain a stream to EOF, keeping at most `cap` bytes.
fn capture_capped(mut stream: impl std::io::Read, cap: usize) -> String {
    let mut kept = Vec::new();
    let mut chunk = [0u8; 8192];
    loop {
        match stream.read(&mut chunk) {
            Ok(0) | Err(_) => break,
            Ok(count) => {
                if kept.len() < cap {
                    let room = cap - kept.len();
                    kept.extend_from_slice(&chunk[..count.min(room)]);
                }
            }
        }
    }
    String::from_utf8_lossy(&kept).into_owned()
}

/// The static gate over changed files: `cargo clippy` without target
/// selection, because clippy itself runs on the workspace graph.
///
/// `--locked` pins the lockfile: a gate that silently updates
/// dependencies would certify a build nothing else reproduced.
///
/// # Errors
///
/// As [`run_gate`].
pub fn static_gate(cwd: &Path) -> Result<GateRun, IntrospectError> {
    run_gate(
        Kind::Static,
        "clippy",
        "cargo",
        &["clippy", "--quiet", "--locked", "--message-format", "short", "--", "-D", "warnings"],
        cwd,
    )
}

/// The dynamic gate: `cargo test` over the workspace.
///
/// `--locked` pins the lockfile for the same reason as [`static_gate`].
///
/// # Errors
///
/// As [`run_gate`].
pub fn dynamic_gate(cwd: &Path) -> Result<GateRun, IntrospectError> {
    run_gate(Kind::Dynamic, "cargo-test", "cargo", &["test", "--quiet", "--locked"], cwd)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_located_diagnostic_yields_a_located_finding() {
        let finding =
            parse_cargo_diagnostic(Kind::Static, "clippy", "  --> src/main.rs:12:9").expect("parses");
        assert_eq!(finding.path.as_deref(), Some("src/main.rs"));
        assert_eq!(finding.line, Some(12));
        assert_eq!(finding.kind, Kind::Static);
        assert!(finding.evidence_ref().contains("src/main.rs:12"), "{}", finding.evidence_ref());
    }

    #[test]
    fn a_short_format_diagnostic_yields_a_located_finding() {
        for line in [
            "crates/supra_x/src/lib.rs:34:5: warning: unused import",
            "crates/supra_x/src/lib.rs:34:5: error[E0308]: mismatched types",
            "crates/supra_x/src/lib.rs:34:5: error: could not compile",
        ] {
            let finding = parse_cargo_diagnostic(Kind::Static, "clippy", line).expect("parses");
            assert_eq!(finding.path.as_deref(), Some("crates/supra_x/src/lib.rs"), "{line}");
            assert_eq!(finding.line, Some(34), "{line}");
        }
    }

    #[test]
    fn an_unlocated_error_still_yields_a_finding() {
        let finding =
            parse_cargo_diagnostic(Kind::Dynamic, "cargo-test", "error: test failed").expect("parses");
        assert_eq!(finding.path, None);
        assert_eq!(finding.line, None);
        assert!(finding.summary.contains("error"), "{}", finding.summary);
    }

    #[test]
    fn noise_lines_yield_nothing() {
        assert!(parse_cargo_diagnostic(Kind::Static, "clippy", "    Checking foo v0.1.0").is_none());
        assert!(parse_cargo_diagnostic(Kind::Static, "clippy", "warning: 2 warnings emitted").is_none());
        assert!(parse_cargo_diagnostic(Kind::Static, "clippy", "   Compiling libc").is_none());
        assert!(parse_cargo_diagnostic(Kind::Static, "clippy", "").is_none());
    }

    #[test]
    fn every_finding_carries_the_evidence_shape() {
        let located = Finding::new(
            Kind::Dynamic,
            "cargo-test",
            "assertion failed",
            Some("tests/x.rs".to_owned()),
            Some(7),
        );
        assert_eq!(located.evidence_ref(), "cargo-test#tests/x.rs:7");

        let bare = Finding::new(Kind::CrossAgent, "peers", "answers diverge", None, None);
        assert_eq!(bare.evidence_ref(), "peers#peers");
    }

    #[test]
    fn a_real_gate_runs_and_reports_pass() {
        let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let run = static_gate(&manifest.join("../../")).expect("gate runs");
        assert!(run.passed, "the workspace is clippy-clean: {:?}", run.findings);
        assert!(run.findings.is_empty(), "{:?}", run.findings);
    }

    #[test]
    fn a_gate_that_cannot_run_is_a_refusal_not_a_pass() {
        let run = run_gate(Kind::Static, "clippy", "definitely-not-a-program-918273", &[], Path::new("/"));
        assert!(matches!(run, Err(IntrospectError::Spawn(_))));
    }

    #[test]
    fn a_capped_capture_keeps_the_head_and_drains_the_rest() {
        let flood: Vec<u8> = (0..100_u8).cycle().take(100_000).collect();
        let kept = capture_capped(std::io::Cursor::new(flood), 1_000);
        assert_eq!(kept.len(), 1_000, "the cap is the cap");
    }

    #[test]
    #[cfg(unix)]
    fn a_grandchild_holding_the_pipe_cannot_hang_the_gate() {
        let start = std::time::Instant::now();
        let run = run_gate(
            Kind::Dynamic,
            "cargo-test",
            "sh",
            &["-c", "sleep 300 & exec echo done"],
            Path::new("/"),
        )
        .expect("the gate returns");
        assert!(run.passed, "the direct child succeeded");
        assert!(
            start.elapsed() < std::time::Duration::from_secs(30),
            "the sleeping grandchild was killed with its group, took {:?}",
            start.elapsed()
        );
    }

    #[test]
    fn a_failing_project_yields_findings() {
        let dir = std::env::temp_dir().join(format!("supra-introspect-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src")).expect("dir");
        std::fs::write(
            dir.join("Cargo.toml"),
            "[package]\nname = \"probe\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[workspace]\n",
        )
        .expect("toml");
        std::fs::write(
            dir.join("src/lib.rs"),
            "pub fn f(x: i32) -> i32 { let y = x; y + 1 }\npub fn g() -> i32 { let z = 1; z }\n",
        )
        .expect("lib");
        let run = static_gate(&dir).expect("gate");
        assert!(!run.passed);
        assert!(!run.findings.is_empty(), "the unused y must be reported");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
