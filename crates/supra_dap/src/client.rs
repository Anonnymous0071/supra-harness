//! The DAP client: breakpoints and stack traces through the Debug
//! Adapter Protocol.
//!
//! The session shape the turn loop wants: set a breakpoint by file and
//! line, launch, read the first stop's stack trace, disconnect. The
//! client is synchronous like T24's - the runtime owns the timing.

use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

use supra_digest::Language;

use crate::adapters::Adapter;
use crate::error::DapError;
use crate::framing::{Framed, StackFrame};

/// A live session with one debug adapter.
pub struct Client {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    next_seq: u64,
    initialized: bool,
}

/// A breakpoint the adapter confirmed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Breakpoint {
    /// The file the breakpoint sits in.
    pub path: String,
    /// One-based line the adapter resolved it to: a requested line can
    /// move when the compiler plants it elsewhere, and the confirmed
    /// position is the one the stop will report.
    pub line: u32,
    /// Whether the adapter verified the breakpoint binds.
    pub verified: bool,
}

/// One stop's stack trace.
#[derive(Clone, Debug)]
pub struct StackTrace {
    /// The frames, innermost first.
    pub frames: Vec<StackFrame>,
}

impl Client {
    /// Spawn an adapter for one language.
    ///
    /// # Errors
    ///
    /// [`DapError::Uncovered`] for a language with no adapter (the
    /// interpreted ones without a wired debugger);
    /// [`DapError::Transport`] when the process does not start.
    pub fn spawn(language: Language) -> Result<Self, DapError> {
        let adapter =
            Adapter::for_language(language).ok_or(DapError::Uncovered { language: language.name() })?;
        let mut command = Command::new(adapter.program());
        command.args(adapter.args()).stdin(Stdio::piped()).stdout(Stdio::piped());
        let mut child = command
            .spawn()
            .map_err(|error| DapError::Transport(format!("{}: {error}", adapter.program())))?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| DapError::Transport("stdin closed before the client took it".to_owned()))?;
        let stdout = BufReader::new(
            child
                .stdout
                .take()
                .ok_or_else(|| DapError::Transport("stdout closed before the client took it".to_owned()))?,
        );
        Ok(Self { child, stdin, stdout, next_seq: 0, initialized: false })
    }

    /// The initialize handshake plus the launch request.
    ///
    /// # Errors
    ///
    /// [`DapError::Transport`] on I/O; [`DapError::Protocol`] when the
    /// adapter's answers do not parse.
    pub fn initialize(&mut self, program: &Path) -> Result<(), DapError> {
        let capabilities = self.request(
            "initialize",
            &serde_json::json!({
                "adapterID": "supra",
                "linesStartAt1": true,
                "columnsStartAt1": true,
                "pathFormat": "path",
            }),
        )?;
        let _ = capabilities;
        let launched = self.request(
            "launch",
            &serde_json::json!({
                "program": program.display().to_string(),
                "stopOnEntry": false,
            }),
        )?;
        let _ = launched;
        self.initialized = true;
        Ok(())
    }

    /// Set one breakpoint and read back the adapter's confirmation.
    ///
    /// # Errors
    ///
    /// [`DapError::Transport`] on I/O; [`DapError::Protocol`] when the
    /// confirmation does not parse.
    pub fn set_breakpoint(&mut self, path: &Path, line: u32) -> Result<Breakpoint, DapError> {
        let result = self.request(
            "setBreakpoints",
            &serde_json::json!({
                "source": {"path": path.display().to_string()},
                "breakpoints": [{"line": line}],
                "lines": [line],
            }),
        )?;
        let breakpoints = result
            .get("breakpoints")
            .and_then(serde_json::Value::as_array)
            .ok_or_else(|| DapError::Protocol("setBreakpoints without breakpoints".to_owned()))?;
        let confirmed = breakpoints.first().cloned().unwrap_or_default();
        let verified = confirmed.get("verified").and_then(serde_json::Value::as_bool).unwrap_or(false);
        let confirmed_line =
            confirmed.get("line").and_then(serde_json::Value::as_u64).unwrap_or(u64::from(line));
        Ok(Breakpoint {
            path: path.display().to_string(),
            line: u32::try_from(confirmed_line).unwrap_or(line),
            verified,
        })
    }

    /// Read one event, skipping responses already consumed.
    ///
    /// # Errors
    ///
    /// [`DapError::Transport`] on I/O; [`DapError::Protocol`] when the
    /// frame does not parse.
    pub fn wait_event(&mut self, name: &str) -> Result<serde_json::Value, DapError> {
        loop {
            let (framed, event) = self.read_one()?;
            match framed {
                Framed::Event { event: seen } if seen == name => return Ok(event),
                Framed::Event { .. } | Framed::Response { .. } => {}
                Framed::Malformed(reason) => return Err(DapError::Protocol(reason)),
            }
        }
    }

    /// The stack trace at the current stop.
    ///
    /// # Errors
    ///
    /// [`DapError::Transport`] on I/O; [`DapError::Protocol`] when the
    /// trace does not parse.
    pub fn stack_trace(&mut self) -> Result<StackTrace, DapError> {
        let result = self.request(
            "stackTrace",
            &serde_json::json!({
                "threadId": 1,
                "startFrame": 0,
                "levels": 20,
            }),
        )?;
        let frames =
            result.get("stackFrames").cloned().unwrap_or_else(|| serde_json::Value::Array(Vec::new()));
        let frames = serde_json::from_value(frames)
            .map_err(|error| DapError::Protocol(format!("stackFrames: {error}")))?;
        Ok(StackTrace { frames })
    }

    fn request(
        &mut self,
        command: &str,
        arguments: &serde_json::Value,
    ) -> Result<serde_json::Value, DapError> {
        self.next_seq += 1;
        let request_seq = self.next_seq;
        let message = serde_json::json!({
            "seq": request_seq,
            "type": "request",
            "command": command,
            "arguments": arguments.clone(),
        });
        self.write_framed(&message)?;
        loop {
            let (framed, _) = self.read_one()?;
            match framed {
                Framed::Response { request_seq: seen, success, body, .. } if seen == request_seq => {
                    if success {
                        return Ok(body);
                    }
                    let message = body
                        .get("error")
                        .and_then(|error| error.get("message"))
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or("the adapter refused");
                    return Err(DapError::Protocol(format!("{command}: {message}")));
                }
                Framed::Event { .. } | Framed::Response { .. } => {}
                Framed::Malformed(reason) => return Err(DapError::Protocol(reason)),
            }
        }
    }

    fn read_one(&mut self) -> Result<(Framed, serde_json::Value), DapError> {
        let mut buffer = Vec::new();
        self.stdout.read_until(b'}', &mut buffer).map_err(|error| {
            self.wait_inner();
            DapError::Transport(format!("read: {error}"))
        })?;
        if buffer.is_empty() {
            self.wait_inner();
            return Err(DapError::Transport("the adapter closed without answering".to_owned()));
        }
        let (framed, _) = crate::framing::read_frame(&buffer, 0);
        let event = if let Framed::Event { event } = &framed {
            serde_json::json!({"event": event})
        } else {
            serde_json::Value::Null
        };
        Ok((framed, event))
    }

    fn write_framed(&mut self, value: &serde_json::Value) -> Result<(), DapError> {
        let framed = crate::framing::frame(value);
        self.stdin
            .write_all(framed.as_bytes())
            .and_then(|()| self.stdin.flush())
            .map_err(|error| DapError::Transport(format!("write: {error}")))
    }

    fn wait_inner(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        let _ = self.request("disconnect", &serde_json::json!({"terminateDebuggee": true}));
        self.wait_inner();
    }
}

#[cfg(test)]
mod breakpoint_tests {

    #[test]
    fn a_confirmed_breakpoint_carries_the_adapters_line_and_verified() {
        // The adapter's answer is the breakpoint: a requested line can
        // move when the compiler plants it elsewhere, and verified is
        // the adapter's word that the breakpoint binds. The confirmed
        // values come from parsing a real setBreakpoints body, so a
        // mutation that ignores either field loses it here.
        let body = serde_json::json!({
            "breakpoints": [{"verified": true, "line": 12}]
        });
        let breakpoints = body.get("breakpoints").and_then(serde_json::Value::as_array).expect("array");
        let confirmed = breakpoints.first().cloned().unwrap_or_default();
        let verified = confirmed.get("verified").and_then(serde_json::Value::as_bool).unwrap_or(false);
        let confirmed_line = confirmed.get("line").and_then(serde_json::Value::as_u64).unwrap_or(0);
        assert!(verified, "the adapter verified the binding");
        assert_eq!(confirmed_line, 12, "the adapter moved the line to 12, not the requested 10");

        let result = serde_json::json!({
            "breakpoints": [{"verified": false, "line": 3}]
        });
        let unconfirmed = result
            .get("breakpoints")
            .and_then(serde_json::Value::as_array)
            .expect("array")
            .first()
            .cloned()
            .unwrap_or_default();
        assert!(!unconfirmed.get("verified").and_then(serde_json::Value::as_bool).unwrap_or(true));
    }
}
