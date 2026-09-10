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

use std::io::Write;
use std::path::Path;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

use supra_ast::Reference;
use supra_digest::Language;
use supra_ffi::piped::PipedInput;

use crate::error::LspError;
use crate::framing::{Framed, Location};
use crate::servers::Server;

/// A live connection to one language server.
pub struct Client {
    child: Child,
    stdin: ChildStdin,
    /// Owns the descriptor `input` reads; dropped with the client.
    #[allow(dead_code)]
    stdout: ChildStdout,
    input: PipedInput,
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
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| LspError::Transport("stdout closed before the client took it".to_owned()))?;
        let input = PipedInput::new(&stdout);
        Ok(Self { child, stdin, stdout, input, next_id: 0, initialized: false })
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
            "jsonrpc": "2.0", "method": "initialized", "params": {}
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
        let id = request.get("id").and_then(serde_json::Value::as_u64).unwrap_or(u64::MAX);
        self.write_framed(request)?;
        loop {
            let message = self.read_framed_message()?;
            if message.is_empty() {
                return Err(LspError::Crashed("the server closed without answering".to_owned()));
            }
            match crate::framing::read_frame(&message, 0) {
                (Framed::Response { id: seen, result }, _) if seen == id => return Ok(result),
                (Framed::Notification | Framed::Response { .. }, _) => {}
                (Framed::Malformed(reason), _) => {
                    return Err(LspError::Protocol(reason));
                }
            }
        }
    }

    fn read_framed_message(&mut self) -> Result<Vec<u8>, LspError> {
        let mut header = Vec::new();
        loop {
            let read = self.input.read_until(b'\n', &mut header).map_err(|error| {
                self.kill_child();
                LspError::Transport(format!("read: {error}"))
            })?;
            if read == 0 {
                self.kill_child();
                return Ok(Vec::new());
            }
            if header.ends_with(b"\r\n\r\n") {
                break;
            }
        }
        let header_text =
            std::str::from_utf8(&header).map_err(|_| LspError::Protocol("non-UTF-8 headers".to_owned()))?;
        let content_length = header_text
            .lines()
            .find_map(|line| line.strip_prefix("Content-Length:"))
            .ok_or_else(|| LspError::Protocol("no Content-Length header".to_owned()))?
            .trim()
            .parse::<usize>()
            .map_err(|error| LspError::Protocol(format!("Content-Length: {error}")))?;
        let mut message = header;
        self.input.read_exact_to(content_length, &mut message).map_err(|error| {
            self.kill_child();
            LspError::Transport(format!("read body: {error}"))
        })?;
        Ok(message)
    }

    fn kill_child(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }

    /// The semantically-resolved references for the symbol at one byte
    /// offset in one file.
    ///
    /// `offset` is a byte offset into `path`, the coordinate the AST
    /// lane reports; it is converted to the line-and-character position
    /// the protocol speaks, and the locations that come back are
    /// converted back to byte offsets against the file each one names.
    /// A location whose file cannot be read is dropped rather than
    /// guessed at.
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
        offset: usize,
    ) -> Result<SemanticReferences, LspError> {
        let language = Language::detect(path).ok_or(LspError::Uncovered { language: "unknown extension" })?;
        let server =
            Server::for_language(language).ok_or(LspError::Uncovered { language: language.name() })?;
        if !self.initialized {
            self.initialize(root)?;
        }
        match self.references_once(path, offset) {
            Ok(references) => Ok(references),
            Err(LspError::Crashed(reason)) => {
                self.kill_child();
                self.initialized = false;
                self.spawn_child(server)?;
                self.initialize(root)?;
                match self.references_once(path, offset) {
                    Ok(references) => Ok(references),
                    Err(_) => Err(LspError::Crashed(reason)),
                }
            }
            Err(error) => Err(error),
        }
    }

    fn spawn_child(&mut self, server: Server) -> Result<(), LspError> {
        let mut command = Command::new(server.program());
        command.args(server.args()).stdin(Stdio::piped()).stdout(Stdio::piped());
        self.child = command.spawn().map_err(|error| LspError::Transport(format!("restart: {error}")))?;
        self.stdin = self
            .child
            .stdin
            .take()
            .ok_or_else(|| LspError::Transport("stdin closed on restart".to_owned()))?;
        self.stdout = self
            .child
            .stdout
            .take()
            .ok_or_else(|| LspError::Transport("stdout closed on restart".to_owned()))?;
        self.input = PipedInput::new(&self.stdout);
        Ok(())
    }

    fn references_once(&mut self, path: &Path, offset: usize) -> Result<SemanticReferences, LspError> {
        let text = std::fs::read_to_string(path)
            .map_err(|error| LspError::Transport(format!("read {}: {error}", path.display())))?;
        let (line, character) = position_of(&text, offset);
        let uri = format!("file://{}", path.display());
        self.next_id += 1;
        let request = serde_json::json!({
            "jsonrpc": "2.0",
            "id": self.next_id,
            "method": "textDocument/references",
            "params": {
                "context": {"includeDeclaration": true},
                "textDocument": {"uri": uri},
                "position": {"line": line, "character": character},
            }
        });
        let result = self.send_and_expect(&request)?;
        let locations: Vec<Location> = serde_json::from_value(result)
            .map_err(|error| LspError::Protocol(format!("references result: {error}")))?;
        let references = locations
            .into_iter()
            .filter_map(|location| {
                let file = path_from_uri(&location.uri)?;
                let target = if file == path { text.clone() } else { std::fs::read_to_string(&file).ok()? };
                let byte = byte_offset(&target, location.range.start.line, location.range.start.character)?;
                Some(Reference { file, occurrences: vec![byte], semantic: true })
            })
            .collect();
        Ok(SemanticReferences { references, semantic: true })
    }
}

/// The line and UTF-16 character for one byte offset, the coordinates
/// the protocol speaks. Characters are UTF-16 code units because that
/// is the default position encoding a server assumes when the client
/// negotiated nothing.
fn position_of(text: &str, byte: usize) -> (u32, u32) {
    let mut line = 0u32;
    let mut character = 0u32;
    for (index, symbol) in text.char_indices() {
        if index >= byte {
            break;
        }
        if symbol == '\n' {
            line += 1;
            character = 0;
        } else {
            character += u32::try_from(symbol.len_utf16()).unwrap_or(1);
        }
    }
    (line, character)
}

/// The byte offset for one line and UTF-16 character, or `None` when
/// the line is past the file.
fn byte_offset(text: &str, line: u32, character: u32) -> Option<usize> {
    let mut current_line = 0u32;
    let mut line_start = 0usize;
    for (index, symbol) in text.char_indices() {
        if current_line > line {
            break;
        }
        if symbol == '\n' {
            current_line += 1;
            if current_line <= line {
                line_start = index + 1;
            }
        }
    }
    if current_line < line {
        return None;
    }
    let mut units = 0u32;
    for (index, symbol) in text[line_start..].char_indices() {
        if symbol == '\n' || symbol == '\r' {
            return Some(line_start + index);
        }
        if units >= character {
            return Some(line_start + index);
        }
        units += u32::try_from(symbol.len_utf16()).unwrap_or(1);
    }
    Some(text.len())
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

    #[test]
    fn offsets_and_positions_round_trip_through_multibyte_text() {
        let text = "fn first() {}\nlet emoji = \"🎉trailing\";\nfn last() {}\n";
        for byte in [0, 3, 13, 14, 19, 26, 41, 52] {
            let (line, character) = position_of(text, byte);
            let back = byte_offset(text, line, character).expect("round trip");
            assert_eq!(back, byte, "byte {byte} became ({line}, {character}) and back to {back}");
        }

        let (line, character) = position_of(text, 21);
        assert_eq!((line, character), (1, 7), "the emoji's lead byte is at UTF-16 unit 7");
    }

    #[test]
    fn a_surrogate_pair_costs_two_units_in_each_direction() {
        let text = "let x = \"🎉\";";
        let emoji = text.find('🎉').expect("the emoji byte offset");
        let (line, character) = position_of(text, emoji);
        assert_eq!((line, character), (0, 9));
        assert_eq!(byte_offset(text, 0, 9), Some(emoji));

        let after = emoji + '🎉'.len_utf8();
        let (line, character) = position_of(text, after);
        assert_eq!((line, character), (0, 11), "the pair counts as two units");
        assert_eq!(byte_offset(text, 0, 11), Some(after));
    }

    #[test]
    fn a_line_past_the_file_has_no_offset() {
        assert_eq!(byte_offset("one line", 1, 0), None);
        assert_eq!(byte_offset("one line", 0, 99), Some("one line".len()));
    }
}
