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
/// The format every cargo tool emits for a located diagnostic:
/// ` --> path:line:col`. The parser is line-oriented and lenient: a
/// diagnostic without a location still yields an unlocated finding,
/// because a finding the reporter cannot place is still a finding.
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
    let output = Command::new(program)
        .args(args)
        .current_dir(cwd)
        .env("CARGO_TERM_COLOR", "never")
        .output()
        .map_err(|error| IntrospectError::Spawn(format!("{program}: {error}")))?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let mut findings = Vec::new();
    for line in stdout.lines().chain(stderr.lines()) {
        if let Some(finding) = parse_cargo_diagnostic(kind, source, line) {
            findings.push(finding);
        }
    }
    findings.dedup_by(|a, b| a.summary == b.summary && a.path == b.path && a.line == b.line);
    Ok(GateRun { passed: output.status.success(), findings })
}

/// The static gate over changed files: `cargo clippy` without target
/// selection, because clippy itself runs on the workspace graph.
///
/// # Errors
///
/// As [`run_gate`].
pub fn static_gate(cwd: &Path) -> Result<GateRun, IntrospectError> {
    run_gate(
        Kind::Static,
        "clippy",
        "cargo",
        &["clippy", "--quiet", "--message-format", "short", "--", "-D", "warnings"],
        cwd,
    )
}

/// The dynamic gate: `cargo test` over the workspace.
///
/// # Errors
///
/// As [`run_gate`].
pub fn dynamic_gate(cwd: &Path) -> Result<GateRun, IntrospectError> {
    run_gate(Kind::Dynamic, "cargo-test", "cargo", &["test", "--quiet"], cwd)
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
