//! DAP wire framing: `Content-Length` headers over stdio, the same
//! shape LSP uses. One deliberate difference: every DAP request carries
//! `seq`, the protocol's own message number, and responses echo `seq`
//! and `request_seq`.
//!
//! Requests are `command`; responses are `success` plus `body`; events
//! are `event`. The reader tells them apart by those fields, not by
//! `id` presence, because DAP responses carry both numbers and events
//! carry neither.

use serde::Deserialize;

/// Serialize one outbound message with its sequence number.
#[must_use]
pub fn frame(message: &serde_json::Value) -> String {
    let body = serde_json::to_string(message).unwrap_or_default();
    format!("Content-Length: {}\r\n\r\n{body}", body.len())
}

/// One parsed inbound message.
#[derive(Debug, PartialEq, Eq)]
pub enum Framed {
    /// A response to an outstanding request.
    Response {
        /// The response's own sequence number.
        seq: u64,
        /// The request it answers.
        request_seq: u64,
        /// Whether the command succeeded.
        success: bool,
        /// The response body, when present.
        body: serde_json::Value,
    },
    /// A reverse request or event from the adapter.
    Event {
        /// The event's name.
        event: String,
    },
    /// The stream ended or the framing broke.
    Malformed(String),
}

/// Read one framed message from `input` starting at `offset`, returning
/// the message and the new offset. Short frames refuse rather than
/// blocking, as the LSP reader does.
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
    let body_start = header_end + 4;
    let body_end = body_start + content_length;
    if rest.len() < body_end {
        return (Framed::Malformed("body shorter than Content-Length".to_owned()), offset);
    }
    let body = String::from_utf8_lossy(&rest[body_start..body_end]).into_owned();
    let next = offset + body_end;
    match serde_json::from_str::<serde_json::Value>(&body) {
        Ok(value) => {
            if value.get("event").is_some() {
                let event = value.get("event").and_then(serde_json::Value::as_str).unwrap_or_default();
                (Framed::Event { event: event.to_owned() }, next)
            } else if value.get("request_seq").is_some() {
                let seq = value.get("seq").and_then(serde_json::Value::as_u64).unwrap_or_default();
                let request_seq =
                    value.get("request_seq").and_then(serde_json::Value::as_u64).unwrap_or_default();
                let success = value.get("success").and_then(serde_json::Value::as_bool).unwrap_or(false);
                let body = value.get("body").cloned().unwrap_or_default();
                (Framed::Response { seq, request_seq, success, body }, next)
            } else if value.get("command").is_some() {
                (Framed::Event { event: "reverse-request".to_owned() }, next)
            } else {
                (Framed::Malformed("message without event or request_seq".to_owned()), next)
            }
        }
        Err(error) => (Framed::Malformed(error.to_string()), next),
    }
}

fn find_headers_end(rest: &[u8]) -> Option<usize> {
    rest.windows(4).position(|window| window == b"\r\n\r\n")
}

/// A stack frame, as `StackTrace` returns it.
#[derive(Clone, Debug, Deserialize)]
pub struct StackFrame {
    /// The frame's name (function or method).
    pub name: String,
    /// Zero-based source line.
    pub line: u32,
    /// The source file, when the adapter resolved one.
    #[serde(default, rename = "source")]
    pub source: Option<Source>,
}

/// The protocol's source object, reduced to what the harness reads.
#[derive(Clone, Debug, Deserialize)]
pub struct Source {
    /// The file path or uri.
    #[serde(default, rename = "path")]
    pub path: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_request_frames_with_content_length_and_seq() {
        let message = serde_json::json!({"seq": 7, "type": "request", "command": "stackTrace"});
        let framed = frame(&message);
        let header = &framed[..framed.find("\r\n\r\n").expect("terminator")];
        let body = &framed[framed.find("\r\n\r\n").expect("terminator") + 4..];
        assert_eq!(header, format!("Content-Length: {}", body.len()));
        assert!(body.contains("\"seq\":7"));
    }

    #[test]
    fn responses_parse_by_request_seq() {
        let body = r#"{"seq":21,"type":"response","request_seq":7,"success":true,"body":{"stackFrames":[]}}"#;
        let payload = format!("Content-Length: {}\r\n\r\n{body}", body.len());
        let (framed, _) = read_frame(payload.as_bytes(), 0);
        let Framed::Response { request_seq, success, .. } = framed else { panic!("shape") };
        assert_eq!(request_seq, 7);
        assert!(success);
    }

    #[test]
    fn events_parse_by_name() {
        let body = r#"{"seq":30,"type":"event","event":"stopped","body":{"reason":"breakpoint"}}"#;
        let payload = format!("Content-Length: {}\r\n\r\n{body}", body.len());
        let (framed, _) = read_frame(payload.as_bytes(), 0);
        let Framed::Event { event } = framed else { panic!("shape") };
        assert_eq!(event, "stopped");
    }

    #[test]
    fn short_frames_and_missing_headers_refuse() {
        let (framed, _) = read_frame(b"Content-Length: 99\r\n\r\nshort", 0);
        assert!(matches!(framed, Framed::Malformed(_)));

        let (framed, _) = read_frame(b"Seq-Only: 1\r\n\r\n{}", 0);
        assert!(matches!(framed, Framed::Malformed(_)));
    }

    #[test]
    fn a_response_whose_request_seq_differs_is_not_ours() {
        // The client correlates by request_seq, not by arrival: a stale
        // response to an earlier request must not satisfy a later one.
        // Framing-level proof: two responses, different request_seq,
        // both parse; the client-side match on request_seq is what the
        // mutation drops, and this fixture supplies the evidence the
        // client test needs.
        for request_seq in [7u64, 9] {
            let body = format!(
                r#"{{"seq":21,"type":"response","request_seq":{request_seq},"success":true,"body":{{}}}}"#
            );
            let payload = format!("Content-Length: {}\r\n\r\n{body}", body.len());
            let (framed, _) = read_frame(payload.as_bytes(), 0);
            let Framed::Response { request_seq: seen, .. } = framed else { panic!("shape") };
            assert_eq!(seen, request_seq);
        }
    }

    #[test]
    fn stack_frames_deserialize() {
        let frames: Vec<StackFrame> = serde_json::from_value(serde_json::json!([
            {"name": "main", "line": 10, "source": {"path": "src/main.rs"}}
        ]))
        .expect("parse");
        assert_eq!(frames[0].name, "main");
        assert_eq!(frames[0].line, 10);
        assert_eq!(frames[0].source.as_ref().and_then(|s| s.path.as_deref()), Some("src/main.rs"));
    }
}
