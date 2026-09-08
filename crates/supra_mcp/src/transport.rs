//! The transports: stdio and HTTP, one `call` shape.
//!
//! MCP servers speak either stdio (a child process, one JSON document per
//! line) or streamable HTTP (POST to an endpoint). The gateway drives
//! both through the same [`Transport`] trait so the layer above negotiates,
//! lists, and calls without knowing which one it is talking to.
//!
//! stdio runs through the T16 sandbox: an MCP server is a subprocess the
//! harness did not write, which is exactly what the sandbox exists for.
//! The pipe carrying JSON-RPC is the parent's side of a `spawn` whose
//! stdio descriptors are CLOEXEC pipes - the hygiene T16.5 and T16 built.

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, ChildStdout};

use super::error::McpError;
use super::rpc::{Request, Response};

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
        /// The server process; killed and reaped on drop.
        child: Child,
        /// The pipe the gateway writes requests to.
        stdin: ChildStdin,
        /// The pipe the gateway reads answers from, buffered per line.
        stdout: BufReader<ChildStdout>,
        /// The monotonic JSON-RPC id for the next request.
        id: u64,
    },
    /// An HTTP endpoint. `id` keeps the same monotonic correlation the
    /// stdio side has; the connection itself is stateless from this
    /// side.
    Http {
        /// The HTTP client (connection pooling is the client's own).
        client: reqwest::Client,
        /// The endpoint URL requests POST to.
        url: String,
        /// The monotonic JSON-RPC id for the next request.
        id: u64,
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
        let mut command = std::process::Command::new(&argv[0]);
        command
            .args(&argv[1..])
            .env_clear()
            .envs(env.iter().map(|(k, v)| (k, v)))
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null());
        let mut child =
            command.spawn().map_err(|error| McpError::Transport(format!("spawn {}: {error}", argv[0])))?;
        let stdin = child.stdin.take().ok_or_else(|| {
            McpError::Transport("the child closed stdin before the gateway could take it".to_owned())
        })?;
        let stdout = BufReader::new(child.stdout.take().ok_or_else(|| {
            McpError::Transport("the child closed stdout before the gateway could take it".to_owned())
        })?);
        Ok(Self::Stdio { child, stdin, stdout, id: 0 })
    }

    /// An HTTP transport to `url`.
    ///
    /// # Errors
    ///
    /// [`McpError::Transport`] when the client cannot be constructed.
    pub fn http(url: String) -> Result<Self, McpError> {
        let client = reqwest::Client::new();
        Ok(Self::Http { client, url, id: 0 })
    }

    /// Issue one JSON-RPC request and await its response.
    ///
    /// # Errors
    ///
    /// [`McpError::Transport`] on I/O; [`McpError::Protocol`] when the
    /// answer is not a JSON-RPC response or carries the error object.
    pub async fn call(
        &mut self,
        method: &str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, McpError> {
        match self {
            Self::Stdio { child, stdin, stdout, id } => {
                *id += 1;
                let request = Request::new(*id, method, params);
                let line = serde_json::to_string(&request)
                    .map_err(|error| McpError::Protocol(format!("serialise: {error}")))?;
                stdin
                    .write_all(line.as_bytes())
                    .and_then(|()| stdin.write_all(b"\n"))
                    .and_then(|()| stdin.flush())
                    .map_err(|error| McpError::Transport(format!("write: {error}")))?;

                let mut buffer = String::new();
                stdout
                    .read_line(&mut buffer)
                    .map_err(|error| McpError::Transport(format!("read: {error}")))?;
                if buffer.is_empty() {
                    // The child closed stdout without answering - a crash,
                    // and the child is reaped so no zombie survives the
                    // refusal.
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(McpError::Transport("the server closed without answering".to_owned()));
                }
                let response: Response = serde_json::from_str(&buffer)
                    .map_err(|error| McpError::Protocol(format!("the answer is not JSON-RPC: {error}")))?;
                answer(response)
            }
            Self::Http { client, url, id } => {
                *id += 1;
                let request = Request::new(*id, method, params);
                let response = client
                    .post(url.clone())
                    .json(&request)
                    .send()
                    .await
                    .map_err(|error| McpError::Transport(format!("http: {error}")))?;
                let status = response.status();
                let body = response
                    .text()
                    .await
                    .map_err(|error| McpError::Transport(format!("http body: {error}")))?;
                if !status.is_success() {
                    return Err(McpError::Transport(format!("http {status}: {body}")));
                }
                let response: Response = serde_json::from_str(&body)
                    .map_err(|error| McpError::Protocol(format!("the answer is not JSON-RPC: {error}")))?;
                answer(response)
            }
        }
    }
}

impl Drop for Transport {
    fn drop(&mut self) {
        if let Self::Stdio { child, .. } = self {
            // Close stdin (drop is implicit at the end of this scope),
            // then reap: a server that exits on EOF is waited for; one
            // that lingers is killed so no zombie outlives the gateway.
            let _ = child.kill();
            let _ = child.wait();
        }
    }
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
        let result = transport.call("initialize", serde_json::json!({})).await.expect("call");
        assert_eq!(result["echo"], serde_json::json!(true));
    }

    #[tokio::test]
    async fn stdio_reports_a_protocol_error() {
        let endpoint = python_responder(
            r#"import sys, json
line = sys.stdin.readline()
request = json.loads(line)
print(json.dumps({"jsonrpc": "2.0", "id": request["id"], "error": {"code": -32601, "message": "no such method"}}))
"#,
        );
        let mut transport = Transport::stdio(&endpoint).expect("spawn");
        let error = transport.call("tools/list", serde_json::json!({})).await.expect_err("error");
        assert!(error.to_string().contains("no such method"), "{error}");
    }

    #[tokio::test]
    async fn a_closed_child_is_a_transport_error_not_a_hang() {
        // The child answers nothing and exits; the gateway must refuse,
        // not block - and must reap the child.
        let endpoint = python_responder("import sys\nsys.exit(0)");
        let mut transport = Transport::stdio(&endpoint).expect("spawn");
        let error = transport.call("tools/list", serde_json::json!({})).await.expect_err("closed");
        assert!(matches!(error, McpError::Transport(_)), "{error}");
    }

    #[tokio::test]
    async fn http_posts_and_parses() {
        // A local HTTP MCP server on an ephemeral port: the same shape a
        // remote endpoint has, driven end to end.
        use std::io::{Read, Write};
        use std::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = listener.local_addr().expect("addr").port();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept");
            let mut buffer = [0u8; 4096];
            let read = stream.read(&mut buffer).expect("read");
            let request = std::str::from_utf8(&buffer[..read]).expect("utf8");
            // The JSON-RPC body rides at the end of the HTTP request.
            let body_start = request.find('{').expect("body");
            let request: serde_json::Value = serde_json::from_str(&request[body_start..]).expect("json");
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
                format!(r#"{{"jsonrpc":"2.0","id":{},"result":{{"served":true}}}}"#, request["id"]).len(),
                format!(r#"{{"jsonrpc":"2.0","id":{},"result":{{"served":true}}}}"#, request["id"]),
            );
            stream.write_all(response.as_bytes()).expect("write");
        });

        let mut transport = Transport::http(format!("http://127.0.0.1:{port}/mcp")).expect("client");
        let result = transport.call("tools/list", serde_json::json!({})).await.expect("call");
        assert_eq!(result["served"], serde_json::json!(true));
        server.join().expect("server thread");
    }

    #[test]
    fn drop_reaps_the_child() {
        let endpoint = python_responder("import sys\nsys.stdin.readline()\nsys.exit(0)");
        let transport = Transport::stdio(&endpoint);
        let pid = match &transport {
            Ok(Transport::Stdio { child, .. }) => child.id(),
            _ => panic!("stdio"),
        };
        drop(transport);
        // The child may exit on its own or be killed; either way, drop
        // waits, so the pid leaves the process table.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while std::path::Path::new(&format!("/proc/{pid}")).exists() {
            assert!(std::time::Instant::now() < deadline, "the child was not reaped");
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }
}
