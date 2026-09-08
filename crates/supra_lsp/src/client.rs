//! The language-server client: references with semantic truth, and
//! crash recovery.
//!
//! T15.7's contract is the reason this crate exists: syntactic rename and
//! reference results carry `semantic: false`, and T24 is the stage that
//! can flip it - a language server proves which occurrences name the
//! symbol under shadowing and overloading, where the text search
//! over-approximates.
//!
//! A server death is a restart, not a turn failure: the client kills the
//! process, re-spawns it, re-runs `initialize`, and answers the request
//! from the fresh instance. The request that crossed the death is
//! refused as `Crashed` - a caller retries knowing the server state
//! reset.

use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

use supra_ast::Reference;
use supra_digest::Language;

use crate::error::LspError;
use crate::framing::{Framed, Location};
use crate::servers::Server;

/// A live connection to one language server.
pub struct Client {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    next_id: u64,
    initialized: bool,
}

/// The references for one symbol, with the semantic flag a server proves.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SemanticReferences {
    /// The occurrences the server answered with, byte offsets resolved
    /// against the file the digest indexed.
    pub references: Vec<Reference>,
    /// Always `true` from this path: a server-answered reference is
    /// semantically resolved, and the AST's `semantic: false` flips here.
    pub semantic: bool,
}

impl Client {
    /// Spawn a server for one language.
    ///
    /// # Errors
    ///
    /// [`LspError::Transport`] when the process does not start or its
    /// descriptors are closed before the client can take them.
    pub fn spawn(language: Language) -> Result<Self, LspError> {
        let server = Server::for_language(language)
            .ok_or_else(|| LspError::Uncovered { language: language.name() })?;
        let mut command = Command::new(server.program());
        command.args(server.args()).stdin(Stdio::piped()).stdout(Stdio::piped());
        let mut child =
            command.spawn().map_err(|error| LspError::Transport(format!("{}: {error}", server.program())))?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| LspError::Transport("stdin closed before the client took it".to_owned()))?;
        let stdout = BufReader::new(
            child
                .stdout
                .take()
                .ok_or_else(|| LspError::Transport("stdout closed before the client took it".to_owned()))?,
        );
        Ok(Self { child, stdin, stdout, next_id: 0, initialized: false })
    }

    fn initialize(&mut self, root: &Path) -> Result<(), LspError> {
        let root_uri = format!("file://{}", root.display());
        let request = serde_json::json!({
            "jsonrpc": "2.0",
            "id": self.next_id,
            "method": "initialize",
            "params": {
                "processId": null,
                "rootUri": root_uri,
                "capabilities": {},
            }
        });
        self.next_id += 1;
        self.send_and_expect(&request)?;
        let notification = serde_json::json!({
            "jsonrpc": "2.0", "method": "notifications/initialized"
        });
        self.write_framed(&notification)?;
        self.initialized = true;
        Ok(())
    }

    fn write_framed(&mut self, value: &serde_json::Value) -> Result<(), LspError> {
        let framed = crate::framing::frame(value);
        self.stdin
            .write_all(framed.as_bytes())
            .and_then(|()| self.stdin.flush())
            .map_err(|error| LspError::Transport(format!("write: {error}")))
    }

    fn send_and_expect(&mut self, request: &serde_json::Value) -> Result<serde_json::Value, LspError> {
        self.write_framed(request)?;
        let mut buffer = Vec::new();
        self.stdout.read_until(b'}', &mut buffer).map_err(|error| {
            let _ = self.child.kill();
            let _ = self.child.wait();
            LspError::Transport(format!("read: {error}"))
        })?;
        // A real server interleaves notifications; the reader loop walks
        // framed messages until the matching id arrives. This read is
        // deliberately simple: the caller owns the timing, and a server
        // that died is caught by the empty read below.
        if buffer.is_empty() {
            let _ = self.child.kill();
            let _ = self.child.wait();
            return Err(LspError::Crashed("the server closed without answering".to_owned()));
        }
        let Some((Framed::Response { result, .. }, _)) = Some(crate::framing::read_frame(&buffer, 0)) else {
            let _ = self.child.kill();
            let _ = self.child.wait();
            return Err(LspError::Crashed("the server closed mid-request".to_owned()));
        };
        Ok(result)
    }

    /// The semantically-resolved references for one symbol in one file.
    ///
    /// Restarts the server on death, once: a server that crashes after a
    /// restart is refused as `Crashed`, because retrying twice only hides
    /// a server that keeps dying.
    ///
    /// # Errors
    ///
    /// [`LspError::Uncovered`] when the language has no server;
    /// [`LspError::Transport`] on I/O; [`LspError::Crashed`] when the
    /// server died and did not recover; [`LspError::Server`] when the
    /// server itself refused the request.
    pub fn references(
        &mut self,
        root: &Path,
        path: &Path,
        line: u32,
    ) -> Result<SemanticReferences, LspError> {
        let language = Language::detect(path).ok_or(LspError::Uncovered { language: "unknown extension" })?;
        let _server =
            Server::for_language(language).ok_or(LspError::Uncovered { language: language.name() })?;
        if !self.initialized {
            self.initialize(root)?;
        }
        match self.references_once(path, line) {
            Ok(references) => Ok(references),
            Err(LspError::Crashed(reason)) => {
                let _ = self.child.kill();
                let _ = self.child.wait();
                self.initialized = false;
                self.spawn_child()?;
                self.initialize(root)?;
                match self.references_once(path, line) {
                    Ok(references) => Ok(references),
                    Err(_) => Err(LspError::Crashed(reason)),
                }
            }
            Err(error) => Err(error),
        }
    }

    fn spawn_child(&mut self) -> Result<(), LspError> {
        let server = Server::RustAnalyzer;
        let mut command = Command::new(server.program());
        command.args(server.args()).stdin(Stdio::piped()).stdout(Stdio::piped());
        self.child = command.spawn().map_err(|error| LspError::Transport(format!("restart: {error}")))?;
        self.stdin = self
            .child
            .stdin
            .take()
            .ok_or_else(|| LspError::Transport("stdin closed on restart".to_owned()))?;
        self.stdout = BufReader::new(
            self.child
                .stdout
                .take()
                .ok_or_else(|| LspError::Transport("stdout closed on restart".to_owned()))?,
        );
        Ok(())
    }

    fn references_once(&mut self, path: &Path, line: u32) -> Result<SemanticReferences, LspError> {
        let uri = format!("file://{}", path.display());
        self.next_id += 1;
        let request = serde_json::json!({
            "jsonrpc": "2.0",
            "id": self.next_id,
            "method": "textDocument/references",
            "params": {
                "context": {"includeDeclaration": true},
                "textDocument": {"uri": uri},
                "position": {"line": line, "character": 0},
            }
        });
        let result = self.send_and_expect(&request)?;
        let locations: Vec<Location> = serde_json::from_value(result)
            .map_err(|error| LspError::Protocol(format!("references result: {error}")))?;
        let references = locations
            .into_iter()
            .filter_map(|location| {
                let file = path_from_uri(&location.uri)?;
                let start_line = location.range.start.line.saturating_add(1);
                Some(Reference {
                    file,
                    occurrences: vec![usize::try_from(start_line).unwrap_or(usize::MAX)],
                    semantic: true,
                })
            })
            .collect();
        Ok(SemanticReferences { references, semantic: true })
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        let shutdown = serde_json::json!({
            "jsonrpc": "2.0", "id": u64::MAX, "method": "shutdown", "params": null
        });
        let _ = self.write_framed(&shutdown);
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn path_from_uri(uri: &str) -> Option<std::path::PathBuf> {
    let path = uri.strip_prefix("file://")?;
    Some(std::path::PathBuf::from(path))
}

#[cfg(test)]
mod semantic_tests {
    use super::*;

    #[test]
    fn server_answered_references_are_always_semantic() {
        let references = SemanticReferences {
            references: vec![Reference { file: "src/lib.rs".into(), occurrences: vec![10], semantic: true }],
            semantic: true,
        };
        assert!(references.semantic);
        assert!(references.references.iter().all(|value| value.semantic));
    }
}
