//! JSON-RPC over stdio with the LSP framing: `Content-Length` headers.
//!
//! The framing is LSP's own, not newline-delimited like MCP: every
//! message is `\r\n\r\n`-separated headers plus a byte-counted body. The
//! client sends requests with monotonically increasing ids and reads
//! responses until the matching id arrives, skipping server-initiated
//! notifications on the way.

use serde::Deserialize;

/// Serialize one request as a framed LSP message.
#[must_use]
pub fn frame(request: &serde_json::Value) -> String {
    let body = serde_json::to_string(request).unwrap_or_default();
    format!("Content-Length: {}\r\n\r\n{body}", body.len())
}

/// A parsed response: either the id's result, or a protocol error.
#[derive(Debug, PartialEq, Eq)]
pub enum Framed {
    /// A response matching an outstanding request id.
    Response {
        /// The request id the response answers.
        id: u64,
        /// The result payload.
        result: serde_json::Value,
    },
    /// A server-initiated notification, skipped by the reader.
    Notification,
    /// The stream ended or the framing broke.
    Malformed(String),
}

/// Read one framed message from `input` starting at `offset`, returning
/// the message and the new offset. Refuses short frames rather than
/// blocking: the caller retries after the next read.
pub fn read_frame(input: &[u8], offset: usize) -> (Framed, usize) {
    let rest = &input[offset..];
    let Some(header_end) = find_headers_end(rest) else {
        return (Framed::Malformed("no header terminator yet".to_owned()), offset);
    };
    let headers = String::from_utf8_lossy(&rest[..header_end]);
    let mut content_length = None;
    for header in headers.split("\r\n") {
        if let Some(value) = header.strip_prefix("Content-Length:") {
            content_length = value.trim().parse::<usize>().ok();
        }
    }
    let Some(content_length) = content_length else {
        return (Framed::Malformed("no Content-Length header".to_owned()), offset);
    };
    let body_start = header_end + "\r\n\r\n".len();
    let body_end = body_start + content_length;
    if rest.len() < body_end {
        return (Framed::Malformed("body shorter than Content-Length".to_owned()), offset);
    }
    let body = String::from_utf8_lossy(&rest[body_start..body_end]).into_owned();
    let next = offset + body_end;
    match serde_json::from_str::<serde_json::Value>(&body) {
        Ok(value) => {
            if value.get("method").is_some() && value.get("id").is_none() {
                (Framed::Notification, next)
            } else {
                let id = value.get("id").and_then(serde_json::Value::as_u64);
                match id {
                    Some(id) => (
                        Framed::Response { id, result: value.get("result").cloned().unwrap_or_default() },
                        next,
                    ),
                    None => (Framed::Malformed("response without id".to_owned()), next),
                }
            }
        }
        Err(error) => (Framed::Malformed(error.to_string()), next),
    }
}

fn find_headers_end(rest: &[u8]) -> Option<usize> {
    rest.windows(4).position(|window| window == b"\r\n\r\n")
}

/// The `textDocument/references` result: a list of locations.
#[derive(Clone, Debug, Deserialize)]
pub struct Location {
    /// The file containing the reference.
    #[serde(rename = "uri")]
    pub uri: String,
    /// The range inside it. Only the start line is read; the caller has
    /// the byte offsets already.
    #[serde(rename = "range")]
    pub range: Range,
}

/// The protocol's range object.
#[derive(Clone, Debug, Deserialize)]
pub struct Range {
    /// Zero-based start.
    #[serde(rename = "start")]
    pub start: Position,
}

/// The protocol's position object.
#[derive(Clone, Copy, Debug, Deserialize)]
pub struct Position {
    /// Zero-based line.
    #[serde(rename = "line")]
    pub line: u32,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_request_frames_with_content_length() {
        let request = json!({"jsonrpc": "2.0", "id": 1, "method": "shutdown"});
        let framed = frame(&request);
        assert!(framed.starts_with("Content-Length: "), "{framed}");
        let body = &framed[framed.find("\r\n\r\n").expect("terminator") + 4..];
        let header = &framed[..framed.find("\r\n\r\n").expect("terminator")];
        assert_eq!(header, format!("Content-Length: {}", body.len()));
    }

    #[test]
    fn a_response_parses_by_id_and_skips_notifications() {
        let notification_body = r#"{"jsonrpc":"2.0","method":"window/logMessage","params":{"message":"hi"}}"#;
        let payload = format!("Content-Length: {}\r\n\r\n{notification_body}", notification_body.len());
        let (framed, _) = read_frame(payload.as_bytes(), 0);
        assert!(matches!(framed, Framed::Notification), "{framed:?}");

        let payload = concat!(
            "Content-Length: 58\r\n\r\n",
            r#"{"jsonrpc":"2.0","id":3,"result":[{"uri":"file:///x.rs"}]}"#,
        );
        let (framed, _) = read_frame(payload.as_bytes(), 0);
        let Framed::Response { id, result } = framed else { panic!("shape") };
        assert_eq!(id, 3);
        assert_eq!(result, json!([{"uri": "file:///x.rs"}]));
    }

    #[test]
    fn short_frames_and_broken_bodies_refuse() {
        let (framed, _) = read_frame(b"Content-Length: 100\r\n\r\nshort", 0);
        assert!(matches!(framed, Framed::Malformed(_)), "{framed:?}");

        let (framed, _) = read_frame(b"X-Header: 1\r\n\r\n{}", 0);
        assert!(matches!(framed, Framed::Malformed(_)), "{framed:?}");
    }

    #[test]
    fn a_frame_without_content_length_refuses() {
        let payload = "Content-Type: text\r\n\r\n{}";
        let (framed, _) = read_frame(payload.as_bytes(), 0);
        assert!(
            matches!(&framed, Framed::Malformed(reason) if reason.contains("Content-Length")),
            "{framed:?}"
        );
    }

    #[test]
    fn locations_deserialize() {
        let locations: Vec<Location> = serde_json::from_value(json!([
            {"uri": "file:///a.rs", "range": {"start": {"line": 4, "character": 0}, "end": {}}}
        ]))
        .expect("parse");
        assert_eq!(locations[0].uri, "file:///a.rs");
        assert_eq!(locations[0].range.start.line, 4);
    }
}
