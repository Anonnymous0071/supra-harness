//! REPL integration: slash dispatch, typo refusal, session guards.

#![allow(clippy::expect_used, clippy::panic)]

fn supra() -> std::path::PathBuf {
    let mut path = std::env::current_exe().expect("test binary");
    path.pop();
    if path.file_name().is_some_and(|name| name == "deps") {
        path.pop();
    }
    path.join("supra")
}

fn repl_output(input: &str) -> (bool, String, String) {
    let mut child = std::process::Command::new(supra())
        .arg("run")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("spawn supra run");
    {
        use std::io::Write as _;
        child.stdin.as_mut().expect("stdin").write_all(input.as_bytes()).expect("write");
    }
    let output = child.wait_with_output().expect("wait");
    (
        output.status.success(),
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

#[test]
fn the_repl_lists_every_builtin_and_reports_mode() {
    let (ok, out, _) = repl_output("/help\n/mode auto\n/telemetry\n/sandbox\n/exit\n");
    assert!(ok, "{out}");
    for builtin in ["/help", "/exit", "/clear", "/model", "/resume", "/branch", "/export"] {
        assert!(out.contains(builtin), "{builtin} missing: {out}");
    }
    assert!(out.contains("mode auto"), "{out}");
    assert!(out.contains("telemetry off"), "{out}");
    assert!(out.contains("sandbox on"), "{out}");
}

#[test]
fn a_typo_is_a_named_error_not_a_billable_task() {
    let (ok, out, err) = repl_output("/resum typo\n/exit\n");
    assert!(!ok, "a typo must fail the run, not bill: {out} {err}");
    assert!(err.contains("/resum"), "{err}");
}

#[test]
fn branch_and_export_without_a_session_refuse() {
    let (ok, out, err) = repl_output("/branch\n/export\n/new\n/exit\n");
    assert!(!ok, "{out} {err}");
    assert!(err.contains("needs a live session"), "{err}");
}

#[test]
fn resume_without_a_task_refuses_before_any_provider_call() {
    let output = std::process::Command::new(supra())
        .args(["run", "--resume", "01J00000000000000000000000"])
        .output()
        .expect("spawn");
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("--resume needs a task"), "{stderr}");
}
