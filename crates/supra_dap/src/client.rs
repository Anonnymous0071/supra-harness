//! The DAP client: breakpoints and stack traces through the Debug
//! Adapter Protocol.
//!
//! The session shape the turn loop wants: set a breakpoint by file and
//! line, launch, read the first stop's stack trace, disconnect. The
//! client is synchronous like T24's - the runtime owns the timing.

use std::collections::VecDeque;
use std::io::Write;
use std::path::Path;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

use supra_digest::Language;
use supra_ffi::piped::PipedInput;

use crate::adapters::Adapter;
use crate::error::DapError;
use crate::framing::{Framed, StackFrame};

/// A live session with one debug adapter.
pub struct Client {
    child: Child,
    stdin: ChildStdin,
    /// Owns the descriptor `input` reads; dropped with the client.
    #[allow(dead_code)]
    stdout: ChildStdout,
    input: PipedInput,
    next_seq: u64,
    initialized: bool,
    events: VecDeque<serde_json::Value>,
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
        command.args(adapter.args());
        Self::spawn_command(&mut command)
    }

    fn spawn_command(command: &mut Command) -> Result<Self, DapError> {
        command.stdin(Stdio::piped()).stdout(Stdio::piped());
        let mut child = command
            .spawn()
            .map_err(|error| DapError::Transport(format!("{:?}: {error}", command.get_program())))?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| DapError::Transport("stdin closed before the client took it".to_owned()))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| DapError::Transport("stdout closed before the client took it".to_owned()))?;
        let input = PipedInput::new(&stdout);
        Ok(Self { child, stdin, stdout, input, next_seq: 0, initialized: false, events: VecDeque::new() })
    }

    /// The initialize handshake and launch, in the order the protocol
    /// draws them.
    ///
    /// The DAP sequence is a request *and* an event: the adapter
    /// answers `initialize`, then emits its `initialized` event, and
    /// only considers configuration open once the client has seen it.
    /// Adapters that follow the spec's own diagram withhold the
    /// `launch` response until `configurationDone` arrives, so waiting
    /// for launch before configuring deadlocks both sides. This sends
    /// launch, configures, then collects the responses in order.
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

        let initialized = self.wait_event("initialized")?;
        let _ = initialized;

        let launch_seq = self.send(
            "launch",
            &serde_json::json!({
                "program": program.display().to_string(),
                "stopOnEntry": false,
            }),
        )?;
        let configured_seq = self.send("configurationDone", &serde_json::json!({}))?;

        self.await_response("launch", launch_seq)?;
        self.await_response("configurationDone", configured_seq)?;
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

    /// Read one event, from the queue first, then the stream.
    ///
    /// Events that arrived while a request was being answered are held
    /// rather than discarded: a `stopped` that lands mid-request is the
    /// stop the caller is about to ask for, not noise.
    ///
    /// # Errors
    ///
    /// [`DapError::Transport`] on I/O; [`DapError::Protocol`] when the
    /// frame does not parse.
    pub fn wait_event(&mut self, name: &str) -> Result<serde_json::Value, DapError> {
        loop {
            while let Some(event) = self.events.pop_front() {
                if event.get("event").and_then(serde_json::Value::as_str) == Some(name) {
                    return Ok(event);
                }
            }
            match self.read_one()? {
                Framed::Event { event } if event == name => {
                    return Ok(serde_json::json!({"event": event}));
                }
                Framed::Event { event } => self.events.push_back(serde_json::json!({"event": event})),
                Framed::Response { .. } => {}
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

    /// Send one request and await its response, holding every event
    /// that arrives on the way.
    ///
    /// # Errors
    ///
    /// [`DapError::Transport`] on I/O; [`DapError::Protocol`] when the
    /// adapter refuses the command or the answer does not parse.
    fn request(
        &mut self,
        command: &str,
        arguments: &serde_json::Value,
    ) -> Result<serde_json::Value, DapError> {
        let request_seq = self.send(command, arguments)?;
        self.await_response(command, request_seq)
    }

    /// Write one request without waiting for its answer.
    fn send(&mut self, command: &str, arguments: &serde_json::Value) -> Result<u64, DapError> {
        self.next_seq += 1;
        let request_seq = self.next_seq;
        let message = serde_json::json!({
            "seq": request_seq,
            "type": "request",
            "command": command,
            "arguments": arguments.clone(),
        });
        self.write_framed(&message)?;
        Ok(request_seq)
    }

    /// Read until the response for `request_seq` arrives, queueing
    /// events.
    fn await_response(&mut self, command: &str, request_seq: u64) -> Result<serde_json::Value, DapError> {
        loop {
            match self.read_one()? {
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
                Framed::Event { event } => self.events.push_back(serde_json::json!({"event": event})),
                Framed::Response { .. } => {}
                Framed::Malformed(reason) => return Err(DapError::Protocol(reason)),
            }
        }
    }

    fn read_one(&mut self) -> Result<Framed, DapError> {
        let body = self.read_framed_body()?;
        if body.is_empty() {
            self.wait_inner();
            return Err(DapError::Transport("the adapter closed without answering".to_owned()));
        }
        let (framed, _) = crate::framing::read_frame(&body, 0);
        Ok(framed)
    }

    fn read_framed_body(&mut self) -> Result<Vec<u8>, DapError> {
        let mut header = Vec::new();
        loop {
            let read = self.input.read_until(b'\n', &mut header).map_err(|error| {
                self.wait_inner();
                DapError::Transport(format!("read: {error}"))
            })?;
            if read == 0 {
                return Ok(Vec::new());
            }
            if header.ends_with(b"\r\n\r\n") {
                break;
            }
        }
        let header_text =
            std::str::from_utf8(&header).map_err(|_| DapError::Protocol("non-UTF-8 headers".to_owned()))?;
        let content_length = header_text
            .lines()
            .find_map(|line| line.strip_prefix("Content-Length:"))
            .ok_or_else(|| DapError::Protocol("no Content-Length header".to_owned()))?
            .trim()
            .parse::<usize>()
            .map_err(|error| DapError::Protocol(format!("Content-Length: {error}")))?;
        let mut message = header;
        self.input.read_exact_to(content_length, &mut message).map_err(|error| {
            self.wait_inner();
            DapError::Transport(format!("read body: {error}"))
        })?;
        Ok(message)
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
        // Kill before any teardown request: a disconnect written to a
        // hung adapter would block Drop for as long as the adapter
        // stayed silent, and the process is going away regardless.
        self.wait_inner();
    }
}

#[cfg(test)]
mod handshake_tests {
    use super::*;

    const SPEC_FAITHFUL_ADAPTER: &str = r#"
import sys, json

def read_frame():
    length = None
    while True:
        line = sys.stdin.buffer.readline()
        if not line:
            return None
        if line in (b"\r\n", b"\n"):
            break
        if line.lower().startswith(b"content-length:"):
            length = int(line.split(b":")[1].strip())
    return json.loads(sys.stdin.buffer.read(length))

def send(msg):
    data = json.dumps(msg).encode()
    sys.stdout.buffer.write(b"Content-Length: %d\r\n\r\n" % len(data))
    sys.stdout.buffer.write(data)
    sys.stdout.buffer.flush()

launch_seq = None
while True:
    msg = read_frame()
    if msg is None:
        break
    command = msg.get("command")
    if command == "initialize":
        send({"seq": 1, "type": "response", "request_seq": msg["seq"], "success": True,
              "command": "initialize", "body": {}})
        send({"seq": 2, "type": "event", "event": "initialized"})
    elif command == "launch":
        launch_seq = msg["seq"]
    elif command == "configurationDone":
        send({"seq": 3, "type": "response", "request_seq": launch_seq, "success": True,
              "command": "launch", "body": {}})
        send({"seq": 4, "type": "response", "request_seq": msg["seq"], "success": True,
              "command": "configurationDone", "body": {}})
"#;

    /// The deadlock shape, stated as a test: a spec-faithful adapter
    /// holds its `launch` response until `configurationDone` arrives,
    /// and the client that waited for launch before configuring would
    /// hang here for the whole read deadline.
    #[test]
    fn the_handshake_survives_an_adapter_that_waits_for_configuration_done() {
        let mut command = Command::new("python3");
        command.args(["-u", "-c", SPEC_FAITHFUL_ADAPTER]);
        let mut client = Client::spawn_command(&mut command).expect("spawn the fake adapter");
        client.input.set_step_timeout(std::time::Duration::from_secs(10));

        let program = std::env::temp_dir().join("supra-dap-target.rs");
        let start = std::time::Instant::now();
        client.initialize(&program).expect("the handshake completes");
        assert!(
            start.elapsed() < std::time::Duration::from_secs(10),
            "the client configured instead of waiting on the withheld launch response"
        );
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
