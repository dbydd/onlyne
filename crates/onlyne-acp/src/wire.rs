//! JSON-RPC 2.0 codec: one message per line, UTF-8, newline-delimited.
//!
//! ACP carries its whole protocol in this envelope, so this module owns the only
//! two things that must never disagree with an agent: how a frame is written
//! (exactly one line, no embedded newline, `jsonrpc`/`id`/`method`/`params`
//! present as the spec requires) and how a frame is classified when it comes
//! back. Classification is by *shape*, never by trust:
//!
//! * a frame with a `method` and a non-null `id` is a request and must be
//!   answered, by us when it arrives and by the agent when we send it;
//! * a frame with a `method` and no `id` is a notification and is fire-and-forget;
//! * a frame with `result` or `error` and a non-null `id` is a response.
//!
//! That shape rule is what keeps the pending-request map sound. An agent is free
//! to pick its own request ids, including one that collides with an id we are
//! holding outstanding, and routing stays correct because a response is only ever
//! matched against a request we registered and a request is only ever handled
//! locally.
//!
//! Strictness is deliberate and refuses at the door: a frame that declares a
//! `jsonrpc` version other than `2.0`, a response with no routable id, or a
//! response carrying both `result` and `error` is rejected as a codec error. The
//! reader thread logs such a line and keeps its stream open, because a single bad
//! line is not a reason to lose a live agent.

use std::fmt;
use std::io::{ErrorKind, Read};

use anyhow::{Result, bail};
use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::{Map, Value};

/// Largest protocol line the reader accepts. An agent that exceeds it has either
/// produced a frame we could not answer anyway or is not speaking ACP, and the
/// stream cannot be resynchronised mid-line, so the reader stops instead of
/// growing its buffer forever.
pub(crate) const MAX_LINE_BYTES: usize = 32 * 1024 * 1024;

/// JSON-RPC 2.0 request id. ACP allows an integer or a string; an id is only
/// useful if it can be echoed back, so `null` is not a request id at all.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum RequestId {
    /// Numeric id, as emitted by most agents.
    Number(i64),
    /// String id, as emitted by agents that namespace their ids.
    Text(String),
}

impl RequestId {
    /// Decode from a JSON value. `None` means the value is not a usable id,
    /// including an integer too large for `i64` — an id we cannot echo back
    /// verbatim is worse than no id, because the agent would never see our reply.
    pub fn from_value(value: &Value) -> Option<RequestId> {
        match value {
            Value::Number(number) => number
                .as_i64()
                .or(number.as_u64().and_then(|raw| i64::try_from(raw).ok()))
                .map(RequestId::Number),
            Value::String(text) => Some(RequestId::Text(text.clone())),
            _ => None,
        }
    }

    /// The integer behind a numeric id.
    pub fn as_i64(&self) -> Option<i64> {
        match self {
            RequestId::Number(number) => Some(*number),
            RequestId::Text(_) => None,
        }
    }

    /// The text behind a string id.
    pub fn as_str(&self) -> Option<&str> {
        match self {
            RequestId::Number(_) => None,
            RequestId::Text(text) => Some(text),
        }
    }
}

impl From<i64> for RequestId {
    fn from(value: i64) -> Self {
        RequestId::Number(value)
    }
}

impl From<&str> for RequestId {
    fn from(value: &str) -> Self {
        RequestId::Text(value.to_string())
    }
}

impl From<String> for RequestId {
    fn from(value: String) -> Self {
        RequestId::Text(value)
    }
}

impl From<RequestId> for Value {
    fn from(value: RequestId) -> Self {
        match value {
            RequestId::Number(number) => Value::from(number),
            RequestId::Text(text) => Value::String(text),
        }
    }
}

impl fmt::Display for RequestId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RequestId::Number(number) => write!(formatter, "{number}"),
            RequestId::Text(text) => formatter.write_str(text),
        }
    }
}

impl Serialize for RequestId {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            RequestId::Number(number) => serializer.serialize_i64(*number),
            RequestId::Text(text) => serializer.serialize_str(text),
        }
    }
}

impl<'de> Deserialize<'de> for RequestId {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = Value::deserialize(deserializer)?;
        RequestId::from_value(&value)
            .ok_or_else(|| D::Error::custom("request id must be an integer or a string"))
    }
}

/// A JSON-RPC error object. Public because a caller must be able to tell
/// `-32601` (the agent lacks a method we asked for, so the feature is absent)
/// from `-32000` (the agent wants credentials) from a transport failure, and an
/// error string is a poor way to carry that distinction.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RpcError {
    /// The JSON-RPC error code.
    pub code: i64,
    /// The agent's own message, verbatim.
    pub message: String,
    /// Structured detail, when the agent sends any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

impl RpcError {
    /// Reserved: invalid request.
    pub const INVALID_REQUEST: i64 = -32600;
    /// Reserved: the peer does not implement this method. Both directions use it:
    /// we answer an unknown agent request with it so the agent falls back to its
    /// own default, and an agent answers an optional method we asked for with it.
    pub const METHOD_NOT_FOUND: i64 = -32601;
    /// Reserved: our params did not satisfy the method.
    pub const INVALID_PARAMS: i64 = -32602;
    /// Reserved: the request was cancelled.
    pub const REQUEST_CANCELLED: i64 = -32800;
    /// Reserved in the agent-error range: the agent needs an auth method run.
    pub const AUTH_REQUIRED: i64 = -32000;

    pub(crate) fn method_not_found(method: &str) -> Self {
        RpcError {
            code: Self::METHOD_NOT_FOUND,
            message: format!("client does not implement {method}"),
            data: None,
        }
    }
}

impl fmt::Display for RpcError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.data {
            Some(data) => write!(
                formatter,
                "{} (code {}, data {data})",
                self.message, self.code
            ),
            None => write!(formatter, "{} (code {})", self.message, self.code),
        }
    }
}

impl std::error::Error for RpcError {}

/// One decoded frame.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Message {
    /// Agent-to-client request: carries an id we must answer.
    Request {
        id: RequestId,
        method: String,
        params: Value,
    },
    /// Agent-to-client notification: no answer expected.
    Notification { method: String, params: Value },
    /// Response to one of our requests.
    Result { id: RequestId, result: Value },
    /// Error response to one of our requests.
    Failure { id: RequestId, error: RpcError },
}

fn envelope() -> Map<String, Value> {
    let mut object = Map::new();
    object.insert("jsonrpc".to_string(), Value::from("2.0"));
    object
}

pub(crate) fn encode_request(id: &RequestId, method: &str, params: Value) -> String {
    let mut object = envelope();
    object.insert("id".to_string(), Value::from(id.clone()));
    object.insert("method".to_string(), Value::from(method));
    object.insert("params".to_string(), params);
    Value::Object(object).to_string()
}

pub(crate) fn encode_notification(method: &str, params: Value) -> String {
    let mut object = envelope();
    object.insert("method".to_string(), Value::from(method));
    object.insert("params".to_string(), params);
    Value::Object(object).to_string()
}

pub(crate) fn encode_result(id: &RequestId, result: Value) -> String {
    let mut object = envelope();
    object.insert("id".to_string(), Value::from(id.clone()));
    object.insert("result".to_string(), result);
    Value::Object(object).to_string()
}

pub(crate) fn encode_failure(id: &RequestId, error: &RpcError) -> String {
    let mut object = envelope();
    object.insert("id".to_string(), Value::from(id.clone()));
    object.insert(
        "error".to_string(),
        serde_json::to_value(error).unwrap_or(Value::Null),
    );
    Value::Object(object).to_string()
}

/// Leading slice of a line for a log or an error message, so a rejected 30 MB
/// frame cannot also become a 30 MB log line.
pub(crate) fn preview(line: &str) -> String {
    const LIMIT: usize = 200;
    if line.len() <= LIMIT {
        return line.to_string();
    }
    let mut cut = LIMIT;
    while !line.is_char_boundary(cut) {
        cut -= 1;
    }
    format!("{}…", &line[..cut])
}

/// Decode one line. Blank input is an error rather than an empty frame: the
/// reader skips blank lines itself, so a caller that reaches `decode` with one
/// has not done its job.
pub(crate) fn decode(line: &str) -> Result<Message> {
    let trimmed = line.trim_end_matches(['\n', '\r']);
    if trimmed.trim().is_empty() {
        bail!("empty frame");
    }
    let value: Value = serde_json::from_str(trimmed)
        .map_err(|error| anyhow::anyhow!("malformed JSON frame ({error}): {}", preview(trimmed)))?;
    let object = value
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("frame is not a JSON object: {}", preview(trimmed)))?;
    if let Some(version) = object.get("jsonrpc")
        && version.as_str() != Some("2.0")
    {
        bail!(
            "frame declares jsonrpc {version}, only 2.0 is spoken: {}",
            preview(trimmed)
        );
    }
    let id = object
        .get("id")
        .filter(|value| !value.is_null())
        .map(|value| {
            RequestId::from_value(value)
                .ok_or_else(|| anyhow::anyhow!("unusable request id {value}: {}", preview(trimmed)))
        })
        .transpose()?;
    let method = object.get("method").and_then(Value::as_str);
    match (method, id) {
        (Some(method), Some(id)) => Ok(Message::Request {
            id,
            method: method.to_string(),
            params: params_of(object),
        }),
        (Some(method), None) => Ok(Message::Notification {
            method: method.to_string(),
            params: params_of(object),
        }),
        (None, Some(id)) => {
            let has_result = object.contains_key("result");
            let error = object.get("error");
            match (has_result, error) {
                (true, Some(_)) => bail!(
                    "response carries both result and error: {}",
                    preview(trimmed)
                ),
                (true, None) => Ok(Message::Result {
                    id,
                    result: object.get("result").cloned().unwrap_or(Value::Null),
                }),
                (false, Some(error)) => {
                    let error: RpcError =
                        serde_json::from_value(error.clone()).map_err(|error| {
                            anyhow::anyhow!(
                                "malformed error object ({error}): {}",
                                preview(trimmed)
                            )
                        })?;
                    Ok(Message::Failure { id, error })
                }
                (false, None) => bail!(
                    "response carries neither result nor error: {}",
                    preview(trimmed)
                ),
            }
        }
        (None, None) => bail!(
            "frame has neither a method nor a routable id: {}",
            preview(trimmed)
        ),
    }
}

fn params_of(object: &Map<String, Value>) -> Value {
    object.get("params").cloned().unwrap_or(Value::Null)
}

// ------------------------------------------------------------- line framing

/// One line pulled out of a byte stream.
#[derive(Debug)]
pub(crate) enum FrameRead {
    /// A whole line, without its newline. `truncated` means the line ran past the
    /// reader's cap and `text` is only its first `cap` bytes; the remainder was
    /// discarded, so the stream stays synchronised either way.
    Line { text: String, truncated: bool },
    /// The writer closed the pipe. Terminal.
    Eof,
    /// The read failed for a reason the caller should log before giving up.
    Failed(std::io::Error),
}

/// Newline-delimited reader with a hard per-line cap.
///
/// `BufRead::lines` cannot be used here: a peer that streams bytes with no
/// newline at all makes it grow until the process is killed. The cap turns that
/// into one truncated line plus a resynchronised stream, which is what a stdout
/// drain needs; a protocol reader treats truncation as fatal because a JSON frame
/// cannot be repaired. Accumulation is reused across lines so a chatty agent does
/// not allocate per update.
pub(crate) struct LineReader<R> {
    source: R,
    buffer: Vec<u8>,
    scratch: Vec<u8>,
    cap: usize,
    dropping: bool,
}

const READ_CHUNK_BYTES: usize = 8 * 1024;

impl<R: Read> LineReader<R> {
    pub(crate) fn new(source: R, cap: usize) -> Self {
        LineReader {
            source,
            buffer: Vec::new(),
            scratch: vec![0u8; READ_CHUNK_BYTES],
            cap,
            dropping: false,
        }
    }

    pub(crate) fn next(&mut self) -> FrameRead {
        loop {
            if let Some(position) = self.buffer.iter().position(|byte| *byte == b'\n') {
                if self.dropping {
                    // The prefix of this line was already handed back.
                    self.dropping = false;
                    self.buffer.drain(..position + 1);
                    continue;
                }
                let too_long = position > self.cap;
                let taken = if too_long { self.cap } else { position };
                let text = String::from_utf8_lossy(&self.buffer[..taken]).into_owned();
                self.buffer.drain(..position + 1);
                return FrameRead::Line {
                    text,
                    truncated: too_long,
                };
            }
            if self.buffer.len() >= self.cap {
                // No newline inside the cap: hand back what is buffered and skip
                // the rest of the line, so the cap bounds memory and the stream
                // stays synchronised on the same frame boundary either way.
                let text = String::from_utf8_lossy(&self.buffer).into_owned();
                self.buffer.clear();
                self.dropping = true;
                return FrameRead::Line {
                    text,
                    truncated: true,
                };
            }
            match self.source.read(&mut self.scratch) {
                Ok(0) => {
                    let final_line = !self.buffer.is_empty();
                    let dropping = std::mem::replace(&mut self.dropping, false);
                    if !final_line || dropping {
                        return FrameRead::Eof;
                    }
                    let text = String::from_utf8_lossy(&self.buffer).into_owned();
                    self.buffer.clear();
                    // A peer that exits without a trailing newline still meant to
                    // say something; drop the last frame and the message is lost.
                    return FrameRead::Line {
                        text,
                        truncated: false,
                    };
                }
                Ok(read) => self.buffer.extend_from_slice(&self.scratch[..read]),
                Err(error) if error.kind() == ErrorKind::Interrupted => continue,
                Err(error) => {
                    self.buffer.clear();
                    return FrameRead::Failed(error);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::io::Cursor;

    fn reader(cap: usize, bytes: &'static [u8]) -> LineReader<Cursor<&'static [u8]>> {
        LineReader::new(Cursor::new(bytes), cap)
    }

    fn lines(reader: &mut LineReader<Cursor<&'static [u8]>>) -> Vec<(String, bool)> {
        let mut out = Vec::new();
        loop {
            match reader.next() {
                FrameRead::Line { text, truncated } => out.push((text, truncated)),
                FrameRead::Eof => return out,
                FrameRead::Failed(error) => panic!("unexpected read failure: {error}"),
            }
        }
    }

    #[test]
    fn a_stream_is_split_on_newlines_and_the_last_frame_without_one_survives() {
        let mut reader = reader(1024, b"one\ntwo\nthree");

        assert_eq!(
            lines(&mut reader),
            vec![
                ("one".to_string(), false),
                ("two".to_string(), false),
                ("three".to_string(), false),
            ]
        );
    }

    #[test]
    fn an_over_long_line_is_cut_at_the_cap_and_the_stream_resynchronises() {
        let mut reader = reader(8, b"0123456789abcdefgh\nnext\n");

        assert_eq!(
            lines(&mut reader),
            vec![("01234567".to_string(), true), ("next".to_string(), false),]
        );
    }

    #[test]
    fn empty_lines_come_back_as_empty_lines_for_the_caller_to_skip() {
        let mut reader = reader(8, b"\n\n");

        assert_eq!(
            lines(&mut reader),
            vec![("".to_string(), false), ("".to_string(), false)]
        );
        assert!(decode("\n").is_err());
    }

    #[test]
    fn invalid_utf8_survives_as_replacement_text_rather_than_killing_the_stream() {
        let mut reader = reader(64, b"ok\xff\nstill-here\n");
        let first = reader.next();

        match first {
            FrameRead::Line { text, truncated } => {
                assert!(text.starts_with("ok"), "{text}");
                assert!(!truncated);
            }
            other => panic!("expected a line, got {other:?}"),
        }
        assert!(matches!(
            reader.next(),
            FrameRead::Line { text, .. } if text == "still-here"
        ));
    }
    #[test]
    fn request_frame_is_a_single_line_envelope() {
        let line = encode_request(
            &RequestId::Number(7),
            "session/prompt",
            json!({"sessionId": "s1", "prompt": [{"type": "text", "text": "a\nb"}]}),
        );

        assert!(!line.contains('\n'), "a frame must never embed a newline");
        assert_eq!(
            serde_json::from_str::<Value>(&line).unwrap(),
            json!({
                "jsonrpc": "2.0",
                "id": 7,
                "method": "session/prompt",
                "params": {"sessionId": "s1", "prompt": [{"type": "text", "text": "a\nb"}]},
            })
        );
        assert_eq!(
            decode(&line).unwrap(),
            Message::Request {
                id: RequestId::Number(7),
                method: "session/prompt".to_string(),
                params: json!({"sessionId": "s1", "prompt": [{"type": "text", "text": "a\nb"}]}),
            }
        );
    }

    #[test]
    fn notification_frame_omits_the_id() {
        let line = encode_notification("session/cancel", json!({"sessionId": "s1"}));

        assert!(
            !serde_json::from_str::<Value>(&line)
                .unwrap()
                .as_object()
                .unwrap()
                .contains_key("id")
        );
        assert_eq!(
            decode(&line).unwrap(),
            Message::Notification {
                method: "session/cancel".to_string(),
                params: json!({"sessionId": "s1"}),
            }
        );
    }

    #[test]
    fn result_frame_round_trips_including_an_explicit_null_result() {
        let line = encode_result(&RequestId::Text("perm-1".into()), Value::Null);

        assert_eq!(
            serde_json::from_str::<Value>(&line).unwrap(),
            json!({"jsonrpc": "2.0", "id": "perm-1", "result": null})
        );
        assert_eq!(
            decode(&line).unwrap(),
            Message::Result {
                id: RequestId::Text("perm-1".to_string()),
                result: Value::Null,
            }
        );
    }

    #[test]
    fn error_frame_carries_code_message_and_data() {
        let error = RpcError {
            code: RpcError::AUTH_REQUIRED,
            message: "run qoderclicn login".to_string(),
            data: Some(json!({"methodId": "qoderclicn-login"})),
        };

        let decoded = decode(&encode_failure(&RequestId::Number(-3), &error)).unwrap();

        assert_eq!(
            decoded,
            Message::Failure {
                id: RequestId::Number(-3),
                error
            }
        );
    }

    #[test]
    fn error_frame_omits_absent_data() {
        let line = encode_failure(
            &RequestId::Number(1),
            &RpcError {
                code: RpcError::METHOD_NOT_FOUND,
                message: "nope".to_string(),
                data: None,
            },
        );

        assert_eq!(
            serde_json::from_str::<Value>(&line).unwrap()["error"],
            json!({"code": -32601, "message": "nope"})
        );
    }

    #[test]
    fn a_malformed_line_is_a_codec_error_and_never_a_panic() {
        for line in [
            "{not json",
            "",
            "   ",
            "[1,2,3]",
            "\"a bare string\"",
            r#"{"jsonrpc":"2.0","id":1,"result":{},"error":{"code":-1,"message":"x"}}"#,
            r#"{"jsonrpc":"2.0","id":1}"#,
            r#"{"jsonrpc":"1.0","method":"session/update"}"#,
            r#"{"jsonrpc":"2.0","result":{"stopReason":"end_turn"}}"#,
            r#"{"jsonrpc":"2.0","id":{"bad":"shape"},"result":{}}"#,
            r#"{"jsonrpc":"2.0","id":1,"error":"a string is not an error object"}"#,
        ] {
            let error = decode(line).expect_err("frame must be rejected");
            assert!(
                !error.to_string().is_empty(),
                "a rejection must name its reason for {line:?}"
            );
        }
    }

    #[test]
    fn a_response_with_a_null_id_is_not_routable_and_is_rejected() {
        assert!(decode(r#"{"jsonrpc":"2.0","id":null,"result":{}}"#).is_err());
    }

    #[test]
    fn a_request_without_params_decodes_with_a_null_params_object() {
        assert_eq!(
            decode(r#"{"jsonrpc":"2.0","id":4,"method":"initialize"}"#).unwrap(),
            Message::Request {
                id: RequestId::Number(4),
                method: "initialize".to_string(),
                params: Value::Null,
            }
        );
    }

    #[test]
    fn ids_that_cannot_be_echoed_back_are_refused() {
        assert_eq!(
            RequestId::from_value(&json!(9_223_372_036_854_775_807_u64)),
            Some(RequestId::Number(i64::MAX))
        );
        assert_eq!(
            RequestId::from_value(&json!(18_446_744_073_709_551_615_u64)),
            None,
            "a u64 above i64::MAX cannot be echoed as an i64"
        );
        assert_eq!(RequestId::from_value(&json!(true)), None);
        assert_eq!(RequestId::from_value(&json!(null)), None);
    }

    #[test]
    fn preview_bounds_a_rejected_frame_before_it_reaches_a_log() {
        let long = "x".repeat(5_000);
        assert_eq!(preview(&long).chars().count(), 201);
        assert_eq!(preview("short"), "short");

        let multibyte = "é".repeat(300);
        assert!(
            preview(&multibyte).starts_with(&"é".repeat(50)),
            "a cut must land on a character boundary, never panic: {}",
            preview(&multibyte)
        );
        assert_eq!(preview(&multibyte).chars().count(), 101);
    }
}
