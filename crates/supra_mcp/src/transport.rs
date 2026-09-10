//! The transports: stdio and HTTP, one `call` shape.
//!
//! MCP servers speak either stdio (a child process, one JSON document per
//! line) or streamable HTTP (POST to an endpoint). The gateway drives
//! both through the same [`Transport`] trait shape so the layer above
//! negotiates, lists, and calls without knowing which one it is talking
//! to.
//!
//! stdio runs as a spawned child whose environment is explicit - the
//! host's env holds secrets no third-party server needs - and whose
//! answers are read under a deadline and a frame cap: a server that
//! stays silent or streams an endless line is an error, not a hang.
//!
//! Every response is correlated: the reader skips notifications and
//! responses to other requests, and refuses a payload that does not
//! claim JSON-RPC 2.0. HTTP captures the session id the server assigns
//! at `initialize` and presents it on every later call, and answers
//! arriving as `text/event-stream` are scanned for the response the
//! call is waiting on.

use std::time::Duration;

use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};

use super::error::McpError;
use super::rpc::{Request, Response};

/// How long one call waits for its answer.
pub const CALL_TIMEOUT: Duration = Duration::from_secs(60);

/// The largest line a stdio server may send before the transport
/// refuses it.
///
/// A JSON-RPC document beyond this size is not an answer anyone
/// intended to read; buffering it anyway is how a rogue server turns a
/// client into an out-of-memory failure.
pub const MAX_LINE_BYTES: usize = 4 * 1024 * 1024;

/// What a server configuration resolves to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Endpoint {
    /// A command to run: argv and an explicit environment. The
    /// environment is explicit for the same reason every child's is -
    /// the host's env holds secrets no third-party server needs.
    Stdio {
        /// The argv to run.
        argv: Vec<String>,
        /// The env the child sees.
        env: Vec<(String, String)>,
    },
    /// An HTTP(S) URL to POST JSON-RPC documents to.
    Http {
        /// The endpoint URL.
        url: String,
    },
}

/// One live connection to a server.
///
/// The enum, not a trait object: the two transports have nothing to share
/// but `call`, and a trait would only re-abstract what this switch states
/// in one place each.
pub enum Transport {
    /// A child process speaking newline-delimited JSON on stdio.
    Stdio {
        /// The server process; killed on drop.
        child: Child,
        /// The pipe the gateway writes requests to.
        stdin: ChildStdin,
        /// The pipe the gateway reads answers from, buffered per line.
        stdout: BufReader<ChildStdout>,
        /// The monotonic JSON-RPC id for the next request.
        id: u64,
        /// The deadline one call waits under; production holds
        /// [`CALL_TIMEOUT`], a probe may shorten it.
        timeout: Duration,
    },
    /// An HTTP endpoint. `id` keeps the same monotonic correlation the
    /// stdio side has; `session` is the id the server assigned at
    /// `initialize`, presented on every later call.
    Http {
        /// The HTTP client (connection pooling is the client's own).
        client: reqwest::Client,
        /// The endpoint URL requests POST to.
        url: String,
        /// The monotonic JSON-RPC id for the next request.
        id: u64,
        /// The session the server assigned, once it has.
        session: Option<String>,
        /// The deadline one call waits under.
        timeout: Duration,
    },
}

impl Transport {
    /// Start a stdio transport: spawn the server and wire its stdio.
    ///
    /// # Errors
    ///
    /// [`McpError::Transport`] when the process does not start. The
    /// failed child is dropped (reaped) on every error path.
    pub fn stdio(endpoint: &Endpoint) -> Result<Self, McpError> {
        let Endpoint::Stdio { argv, env } = endpoint else {
            return Err(McpError::Transport("stdio transport needs a command".to_owned()));
        };
        let Some(program) = argv.first() else {
            return Err(McpError::Transport("an empty argv names no program".to_owned()));
        };
        let mut command = Command::new(program);
        command
            .args(&argv[1..])
            .env_clear()
            .envs(env.iter().map(|(key, value)| (key, value)))
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true);
        let mut child =
            command.spawn().map_err(|error| McpError::Transport(format!("spawn {program}: {error}")))?;
        let stdin = child.stdin.take().ok_or_else(|| {
            McpError::Transport("the child closed stdin before the gateway could take it".to_owned())
        })?;
        let stdout = BufReader::new(child.stdout.take().ok_or_else(|| {
            McpError::Transport("the child closed stdout before the gateway could take it".to_owned())
        })?);
        Ok(Self::Stdio { child, stdin, stdout, id: 0, timeout: CALL_TIMEOUT })
    }

    /// An HTTP transport to `url`.
    ///
    /// # Errors
    ///
    /// [`McpError::Transport`] when the client cannot be constructed.
    pub fn http(url: String) -> Result<Self, McpError> {
        let client = reqwest::Client::new();
        Ok(Self::Http { client, url, id: 0, session: None, timeout: CALL_TIMEOUT })
    }

    /// Shorten the call deadline, for probes that must fail fast.
    pub fn set_timeout(&mut self, timeout: Duration) {
        match self {
            Self::Stdio { timeout: slot, .. } | Self::Http { timeout: slot, .. } => *slot = timeout,
        }
    }

    /// Issue one JSON-RPC request and await its response.
    ///
    /// # Errors
    ///
    /// [`McpError::Transport`] on I/O or when the deadline passes;
    /// [`McpError::Protocol`] when the answer is not a JSON-RPC 2.0
    /// response for this request or carries the error object.
    pub async fn call(
        &mut self,
        method: &str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, McpError> {
        match self {
            Self::Stdio { stdin, stdout, id, timeout, .. } => {
                *id += 1;
                let request = Request::new(*id, method, params);
                let line = serde_json::to_string(&request)
                    .map_err(|error| McpError::Protocol(format!("serialise: {error}")))?;
                stdin
                    .write_all(line.as_bytes())
                    .await
                    .map_err(|error| McpError::Transport(format!("write: {error}")))?;
                stdin
                    .write_all(b"\n")
                    .await
                    .map_err(|error| McpError::Transport(format!("write: {error}")))?;
                stdin.flush().await.map_err(|error| McpError::Transport(format!("write: {error}")))?;

                loop {
                    let line = read_bounded_line(stdout, *timeout).await?;
                    if line.is_empty() {
                        return Err(McpError::Transport("the server closed without answering".to_owned()));
                    }
                    let value: serde_json::Value = serde_json::from_str(&line)
                        .map_err(|error| McpError::Protocol(format!("the answer is not JSON: {error}")))?;
                    if value.get("id").is_some() && value.get("jsonrpc").is_none() {
                        return Err(McpError::Protocol("the answer does not claim JSON-RPC 2.0".to_owned()));
                    }
                    if is_response_for(&value, *id) {
                        let response: Response = serde_json::from_value(value).map_err(|error| {
                            McpError::Protocol(format!("the answer is not JSON-RPC: {error}"))
                        })?;
                        return answer(response);
                    }
                }
            }
            Self::Http { client, url, id, session, timeout } => {
                *id += 1;
                let request = Request::new(*id, method, params);
                let mut outbound = client
                    .post(url.clone())
                    .timeout(*timeout)
                    .json(&request)
                    .header("Accept", "application/json, text/event-stream");
                if let Some(session) = session.as_deref() {
                    outbound = outbound.header("Mcp-Session-Id", session);
                }
                let response =
                    outbound.send().await.map_err(|error| McpError::Transport(format!("http: {error}")))?;
                if method == "initialize" {
                    if let Some(assigned) =
                        response.headers().get("Mcp-Session-Id").and_then(|value| value.to_str().ok())
                    {
                        *session = Some(assigned.to_owned());
                    }
                }
                let status = response.status();
                let content_type = response
                    .headers()
                    .get(reqwest::header::CONTENT_TYPE)
                    .and_then(|value| value.to_str().ok())
                    .unwrap_or_default()
                    .to_owned();
                let body = response
                    .text()
                    .await
                    .map_err(|error| McpError::Transport(format!("http body: {error}")))?;
                if !status.is_success() {
                    return Err(McpError::Transport(format!("http {status}: {body}")));
                }
                let value = if content_type.contains("text/event-stream") {
                    parse_sse(&body, *id)?
                } else {
                    serde_json::from_str(&body)
                        .map_err(|error| McpError::Protocol(format!("the answer is not JSON: {error}")))?
                };
                if !is_response_for(&value, *id) {
                    return Err(McpError::Protocol(
                        "the response does not answer the request that was sent".to_owned(),
                    ));
                }
                let response: Response = serde_json::from_value(value)
                    .map_err(|error| McpError::Protocol(format!("the answer is not JSON-RPC: {error}")))?;
                answer(response)
            }
        }
    }

    /// Send one notification - a message with no id and no answer.
    ///
    /// stdio writes the line and reads nothing back; HTTP POSTs it and
    /// discards whatever comes home, because a notification is fire and
    /// forget by definition.
    ///
    /// # Errors
    ///
    /// [`McpError::Transport`] on I/O.
    pub async fn notify(&mut self, method: &str) -> Result<(), McpError> {
        match self {
            Self::Stdio { stdin, .. } => {
                let notification = serde_json::json!({"jsonrpc": "2.0", "method": method});
                let line = serde_json::to_string(&notification)
                    .map_err(|error| McpError::Protocol(format!("serialise: {error}")))?;
                stdin
                    .write_all(line.as_bytes())
                    .await
                    .map_err(|error| McpError::Transport(format!("write: {error}")))?;
                stdin
                    .write_all(b"\n")
                    .await
                    .map_err(|error| McpError::Transport(format!("write: {error}")))?;
                stdin.flush().await.map_err(|error| McpError::Transport(format!("write: {error}")))?;
                Ok(())
            }
            Self::Http { client, url, session, .. } => {
                let notification = serde_json::json!({"jsonrpc": "2.0", "method": method});
                let mut outbound = client.post(url.clone()).json(&notification);
                if let Some(session) = session.as_deref() {
                    outbound = outbound.header("Mcp-Session-Id", session);
                }
                outbound.send().await.map_err(|error| McpError::Transport(format!("http: {error}")))?;
                Ok(())
            }
        }
    }
}

/// Whether a decoded payload is the response to `id`.
///
/// Notifications (a `method`, no `id`) and responses to other requests
/// are skipped on stdio; a payload that does not claim JSON-RPC 2.0 is
/// never a response, whatever else it says about itself.
fn is_response_for(value: &serde_json::Value, id: u64) -> bool {
    if value.get("id").is_none() {
        return false;
    }
    if value.get("jsonrpc").and_then(serde_json::Value::as_str) != Some("2.0") {
        return false;
    }
    value.get("id").and_then(serde_json::Value::as_u64) == Some(id)
}

/// Read one line, bounded and under the call deadline.
///
/// # Errors
///
/// [`McpError::Transport`] when the deadline passes or the pipe breaks;
/// [`McpError::Protocol`] when the line outgrew [`MAX_LINE_BYTES`].
async fn read_bounded_line(
    stdout: &mut BufReader<ChildStdout>,
    timeout: Duration,
) -> Result<String, McpError> {
    let mut line = Vec::new();
    tokio::time::timeout(timeout, async {
        loop {
            let consumed = {
                let available =
                    stdout.fill_buf().await.map_err(|error| McpError::Transport(format!("read: {error}")))?;
                if available.is_empty() {
                    return Ok(());
                }
                if let Some(position) = available.iter().position(|&byte| byte == b'\n') {
                    line.extend_from_slice(&available[..=position]);
                    position + 1
                } else {
                    line.extend_from_slice(available);
                    available.len()
                }
            };
            stdout.consume(consumed);
            if line.last() == Some(&b'\n') {
                break;
            }
            if line.len() > MAX_LINE_BYTES {
                return Err(McpError::Protocol(format!(
                    "a line exceeded the {MAX_LINE_BYTES}-byte frame cap"
                )));
            }
        }
        Ok(())
    })
    .await
    .map_err(|_| McpError::Transport("the server did not answer within the call timeout".to_owned()))??;
    if line.is_empty() {
        return Ok(String::new());
    }
    while line.last().is_some_and(|byte| *byte == b'\n' || *byte == b'\r') {
        line.pop();
    }
    Ok(String::from_utf8_lossy(&line).into_owned())
}

/// Find the response for `id` in one SSE body.
///
/// # Errors
///
/// [`McpError::Protocol`] when the stream ends without it.
fn parse_sse(body: &str, id: u64) -> Result<serde_json::Value, McpError> {
    for line in body.lines() {
        let Some(payload) = line.strip_prefix("data:") else { continue };
        let payload = payload.trim();
        let Ok(value) = serde_json::from_str::<serde_json::Value>(payload) else { continue };
        if is_response_for(&value, id) {
            return Ok(value);
        }
    }
    Err(McpError::Protocol("the event stream ended without the response".to_owned()))
}

fn answer(response: Response) -> Result<serde_json::Value, McpError> {
    if let Some(error) = response.error {
        return Err(McpError::Protocol(format!("json-rpc error {}: {}", error.code, error.message)));
    }
    response.result.ok_or_else(|| McpError::Protocol("the response has neither result nor error".to_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A tiny in-process MCP server as a shell command: reads one line,
    /// answers one line. This is the stdio shape end to end - spawn,
    /// write, read, reap - without a network or a real third-party
    /// binary.
    fn python_responder(script: &str) -> Endpoint {
        Endpoint::Stdio {
            argv: vec![
                "python3".to_owned(),
                // `-u`: unbuffered stdout. A piped python buffers its
                // output, and an answer sitting in the buffer is a gateway
                // that hangs on readline - the transport tests proved it.
                "-u".to_owned(),
                "-c".to_owned(),
                script.to_owned(),
            ],
            env: vec![("PATH".to_owned(), "/usr/bin:/bin".to_owned())],
        }
    }

    #[tokio::test]
    async fn stdio_round_trips_one_call() {
        let endpoint = python_responder(
            r#"import sys, json
line = sys.stdin.readline()
request = json.loads(line)
print(json.dumps({"jsonrpc": "2.0", "id": request["id"], "result": {"echo": True}}))
"#,
        );
        let mut transport = Transport::stdio(&endpoint).expect("spawn");
        let result = transport.call("tools/list", serde_json::json!({})).await.expect("call");
        assert_eq!(result, serde_json::json!({"echo": true}));
    }

    #[tokio::test]
    async fn notifications_and_foreign_ids_are_skipped_not_confused() {
        let endpoint = python_responder(
            r#"import sys, json
line = sys.stdin.readline()
request = json.loads(line)
print(json.dumps({"jsonrpc": "2.0", "method": "notifications/telemetry", "params": {}}))
print(json.dumps({"jsonrpc": "2.0", "id": 999, "result": {"not": "ours"}}))
print(json.dumps({"jsonrpc": "2.0", "id": request["id"], "result": {"ours": True}}))
"#,
        );
        let mut transport = Transport::stdio(&endpoint).expect("spawn");
        let result = transport.call("tools/list", serde_json::json!({})).await.expect("call");
        assert_eq!(result, serde_json::json!({"ours": true}));
    }

    #[tokio::test]
    async fn a_response_that_does_not_claim_jsonrpc_20_is_refused() {
        let endpoint = python_responder(
            r#"import sys, json
line = sys.stdin.readline()
request = json.loads(line)
print(json.dumps({"id": request["id"], "result": {"version": "1.0"}}))
"#,
        );
        let mut transport = Transport::stdio(&endpoint).expect("spawn");
        let error = transport.call("tools/list", serde_json::json!({})).await.expect_err("refused");
        assert!(error.to_string().contains("JSON-RPC"), "{error}");
    }

    #[tokio::test]
    async fn a_silent_server_times_out_rather_than_hanging() {
        let endpoint = python_responder("import time\ntime.sleep(60)\n");
        let mut transport = Transport::stdio(&endpoint).expect("spawn");
        transport.set_timeout(Duration::from_secs(2));
        let start = std::time::Instant::now();
        let error = transport.call("tools/list", serde_json::json!({})).await.expect_err("timeout");
        assert!(error.to_string().contains("timeout"), "{error}");
        assert!(start.elapsed() < Duration::from_secs(10), "the deadline bounded the wait");
    }

    #[tokio::test]
    async fn an_oversized_line_is_refused_not_buffered() {
        let endpoint = python_responder(
            r#"import sys, json
sys.stdin.readline()
print("x" * (5 * 1024 * 1024))
"#,
        );
        let mut transport = Transport::stdio(&endpoint).expect("spawn");
        let error = transport.call("tools/list", serde_json::json!({})).await.expect_err("refused");
        assert!(error.to_string().contains("frame cap"), "{error}");
    }

    #[test]
    fn an_event_stream_yields_the_matching_response() {
        let body = "event: message\ndata: {\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"page\":1}}\n\n\
                    event: message\ndata: {\"jsonrpc\":\"2.0\",\"id\":2,\"result\":{\"page\":2}}\n\n";
        let value = parse_sse(body, 2).expect("found");
        assert_eq!(value["result"]["page"], 2);
        assert!(parse_sse(body, 7).is_err(), "no response for 7");
    }
}
