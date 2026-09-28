//! `onlyne mcp`: one session's obligations, as MCP tools on stdio.
//!
//! An ACP session mounts this process through its runtime's MCP server list, so
//! the session's three obligations reach the model as tools rather than as a
//! plugin's registered functions (`docs/v2-CONTRACT.md` §3b). The names,
//! arguments, and meanings are pi's plugin's, verbatim: one obligation
//! vocabulary, two drives.
//!
//! The process reads JSON-RPC 2.0 messages from stdin and writes answers to
//! stdout. The client's adapter socket and the session's token arrive in the
//! environment, and the socket is dialed lazily on the first tool call and held
//! for the life of the process: a session that never calls a tool never costs a
//! connection.

use crate::{media, runtime};
use onlyne_adapter::ToolsMount;
use onlyne_adapter::prelude::{
    AdapterError, AdapterIo, AdapterMsg, HostOp, Mount, MountKind, PluginOp, ReportSender,
};
use onlyne_proto::{
    Body, Causality, Envelope, HandoffArgs, HelloAck, HelloArgs, ImagePart, MsgKind, Outcome,
    PROTOCOL_VERSION, Principal, new_id, new_op_id,
};
use serde_json::{Value, json};
use std::path::Path;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

/// The MCP revisions this bridge speaks, newest first.
///
/// The revision the agent asks for is answered with when it is one of these, so a
/// client on an older revision is not pushed onto a newer one's shape; anything
/// else is answered with the newest, which is the revision this bridge was
/// written against.
const PROTOCOL_VERSIONS: [&str; 3] = ["2025-06-18", "2025-03-26", "2024-11-05"];

/// One paragraph for `onlyne mcp --help`.
pub const LONG_ABOUT: &str = "\
Serve one session's obligations as MCP tools on stdio.

The client spawns this process as the session's MCP server, with the client's \
adapter socket in ONLYNE_SOCKET and the session's token in ONLYNE_MCP_TOKEN. The \
three tools are onlyne_send, onlyne_handoff, and onlyne_complete; a call the \
client refuses comes back as the tool result's error text, in the client's own \
words.

This is not an operator verb: it takes no flags, prints no answer of its own on \
stdout, and ends when its stdin closes.";

/// Run the bridge until stdin ends.
pub fn run() -> i32 {
    runtime::block_on(serve())
}

/// Answer MCP messages until the agent closes the pipe.
async fn serve() -> i32 {
    let mut lines = BufReader::new(tokio::io::stdin()).lines();
    let mut out = tokio::io::stdout();
    let mut bridge = Bridge::default();
    loop {
        let line = match lines.next_line().await {
            Ok(Some(line)) => line,
            // The agent that mounted this process is gone, so the session has
            // nobody left to speak for. Dropping the connection is the client's
            // own signal that this tools mount has left.
            Ok(None) => return runtime::EXIT_OK,
            Err(error) => {
                return runtime::usage_error(format!("onlyne: mcp stdin failed: {error}"));
            }
        };
        if line.trim().is_empty() {
            continue;
        }
        let message = match serde_json::from_str::<Value>(&line) {
            Ok(message) => message,
            Err(_) => {
                let answer = error_response(Value::Null, -32700, "parse error");
                if write_message(&mut out, &answer).await.is_err() {
                    return runtime::EXIT_OK;
                }
                continue;
            }
        };
        if !message.is_object() {
            let answer = error_response(Value::Null, -32600, "invalid request");
            if write_message(&mut out, &answer).await.is_err() {
                return runtime::EXIT_OK;
            }
            continue;
        }
        let Some(method) = message.get("method").and_then(Value::as_str) else {
            // A response, or a message naming no method: this bridge sends no
            // requests of its own, so there is nothing it could be answering.
            continue;
        };
        let id = message.get("id").cloned().filter(|id| !id.is_null());
        let params = message.get("params").cloned().unwrap_or(Value::Null);
        let answer = match method {
            "initialize" => Some(initialize_result(&params)),
            "tools/list" => Some(json!({ "tools": tools() })),
            "tools/call" => Some(bridge.tool_call(&params).await),
            "ping" => Some(json!({})),
            other if other.starts_with("notifications/") => None,
            other => Some(error_response(
                Value::Null,
                -32601,
                &format!("unknown method {other}"),
            )),
        };
        // A notification is answered by nothing; a request is answered under the
        // id it carried.
        let (Some(answer), Some(id)) = (answer, id) else {
            continue;
        };
        let message = match answer.get("error") {
            Some(error) => json!({ "jsonrpc": "2.0", "id": id, "error": error }),
            None => json!({ "jsonrpc": "2.0", "id": id, "result": answer }),
        };
        if write_message(&mut out, &message).await.is_err() {
            return runtime::EXIT_OK;
        }
    }
}

/// Write one JSON-RPC message as one line.
async fn write_message(out: &mut tokio::io::Stdout, message: &Value) -> std::io::Result<()> {
    let mut line = serde_json::to_string(message).map_err(std::io::Error::other)?;
    line.push('\n');
    out.write_all(line.as_bytes()).await?;
    out.flush().await
}

fn error_response(id: Value, code: i64, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

/// The `initialize` answer: the negotiated revision, the tool surface, and who
/// this server is.
fn initialize_result(params: &Value) -> Value {
    let requested = params.get("protocolVersion").and_then(Value::as_str);
    json!({
        "protocolVersion": protocol_version(requested),
        "capabilities": { "tools": {} },
        "serverInfo": { "name": "onlyne", "version": env!("CARGO_PKG_VERSION") },
    })
}

/// The revision to answer with: the agent's own, when this bridge speaks it.
fn protocol_version(requested: Option<&str>) -> &'static str {
    match requested {
        Some(version) => PROTOCOL_VERSIONS
            .iter()
            .find(|known| **known == version)
            .copied()
            .unwrap_or(PROTOCOL_VERSIONS[0]),
        None => PROTOCOL_VERSIONS[0],
    }
}

/// The three obligations, carrying pi's plugin's descriptions and argument names.
fn tools() -> Value {
    json!([
        {
            "name": "onlyne_send",
            "title": "Onlyne send",
            "description": "Send one message to another role. kind=note (default) is free text; kind=task hands work to that role and opens a task for it.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "to": { "type": "string", "description": "target role name, e.g. builder" },
                    "text": { "type": "string", "description": "message body" },
                    "kind": {
                        "type": "string",
                        "enum": ["note", "task"],
                        "description": "\"note\" (default) or \"task\""
                    },
                    "image": {
                        "type": "string",
                        "description": "absolute path to a png/jpeg/gif/webp image to attach"
                    }
                },
                "required": ["to", "text"],
                "additionalProperties": false
            }
        },
        {
            "name": "onlyne_handoff",
            "title": "Onlyne handoff",
            "description": "Hand the current task on to another role, which continues it. Call it when this task's work goes on to another role.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "to": { "type": "string", "description": "target role name, e.g. builder" },
                    "text": {
                        "type": "string",
                        "description": "handoff text for the receiving role"
                    },
                    "image": {
                        "type": "string",
                        "description": "absolute path to a png/jpeg/gif/webp image to attach"
                    }
                },
                "required": ["to", "text"],
                "additionalProperties": false
            }
        },
        {
            "name": "onlyne_complete",
            "title": "Onlyne complete",
            "description": "End the current task with an explicit outcome: done (the work is finished), failed (it is provably impossible), cancelled (it was withdrawn), or blocked (something outside this session stops it). summary is the one-line result and details is the full one; files names the paths the result rests on. If the workspace requires a handoff before the task may end, the call is refused until that handoff has gone out.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "outcome": {
                        "type": "string",
                        "enum": ["done", "failed", "cancelled", "blocked"],
                        "description": "\"done\", \"failed\", \"cancelled\", or \"blocked\""
                    },
                    "summary": { "type": "string", "description": "one-line result summary" },
                    "details": {
                        "type": "string",
                        "description": "the full result, delivered as it stands"
                    },
                    "files": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "absolute paths of the files the result names"
                    }
                },
                "required": ["outcome", "summary"],
                "additionalProperties": false
            }
        }
    ])
}

/// The connection to the client, opened on the first tool call.
#[derive(Default)]
struct Bridge {
    session: Option<Session>,
}

impl Bridge {
    /// Answer one `tools/call` with a tool result, or with the protocol error an
    /// unknown tool earns.
    async fn tool_call(&mut self, params: &Value) -> Value {
        let name = params
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if !matches!(name, "onlyne_send" | "onlyne_handoff" | "onlyne_complete") {
            return error_response(Value::Null, -32602, &format!("unknown tool: {name}"));
        }
        let arguments = params
            .get("arguments")
            .cloned()
            .unwrap_or_else(|| json!({}));
        let outcome = self.dispatch(name, &arguments).await;
        // A refusal is the host's own sentence, in the shape the model reads on
        // pi's drive too: what the client said, not what this bridge made of it.
        match outcome {
            Ok(text) => json!({ "content": [{ "type": "text", "text": text }] }),
            Err(text) => json!({
                "content": [{ "type": "text", "text": text }],
                "isError": true,
            }),
        }
    }

    /// Run one tool call against the one connection, dialed now when it is not
    /// up yet.
    async fn dispatch(&mut self, name: &str, arguments: &Value) -> Result<String, String> {
        if self.session.is_none() {
            self.session = Some(Session::dial().await?);
        }
        let Some(session) = &mut self.session else {
            return Err("onlyne: no client connection".to_string());
        };
        match name {
            "onlyne_send" => session.send(arguments).await,
            "onlyne_handoff" => session.handoff(arguments).await,
            _ => session.complete(arguments).await,
        }
    }
}

/// The one connection this process holds.
struct Session {
    io: AdapterIo,
    reports: ReportSender,
}

impl Session {
    /// Mount on the client's adapter socket with this session's token.
    ///
    /// The token is the whole binding: it names the role, the session, and the
    /// generation, so this process names nothing of its own
    /// (`docs/v2-CONTRACT.md` §3b). A token the client does not know is refused
    /// here, and that refusal is what the model reads on its first tool call.
    async fn dial() -> Result<Session, String> {
        let socket = std::env::var("ONLYNE_SOCKET").map_err(|_| {
            "onlyne: ONLYNE_SOCKET is not set, so there is no client to reach".to_string()
        })?;
        let token = std::env::var("ONLYNE_MCP_TOKEN").map_err(|_| {
            "onlyne: ONLYNE_MCP_TOKEN is not set, so this process speaks for no session".to_string()
        })?;
        let stream = onlyne_wire::socket::connect_local(Path::new(&socket))
            .await
            .map_err(|error| format!("onlyne: cannot reach the client socket {socket}: {error}"))?;
        let io = AdapterIo::new(
            stream,
            onlyne_adapter::DEFAULT_READ_TIMEOUT,
            onlyne_adapter::DEFAULT_WRITE_TIMEOUT,
        );
        let hello = HelloArgs {
            protocol: PROTOCOL_VERSION,
            plugin: "onlyne-mcp".to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            kind: MountKind::Tools,
            capabilities: Vec::new(),
            mount: Some(Mount::Tools(ToolsMount { token })),
        };
        let welcome = io
            .request_ok(AdapterMsg::Plugin(PluginOp::Hello(hello)))
            .await
            .map_err(|error| refusal(&error))?;
        let ack = welcome_ack(&welcome)?;
        let reports = ReportSender::new(io.clone(), ack.generation);
        Ok(Session { io, reports })
    }

    /// `onlyne_send`: name the recipient and the message; the token names the
    /// sender.
    async fn send(&self, arguments: &Value) -> Result<String, String> {
        let to = required_string(arguments, "to")?;
        let text = optional_string(arguments, "text").unwrap_or_default();
        let image = image_argument(arguments)?;
        let kind = if arguments.get("kind").and_then(Value::as_str) == Some("task") {
            MsgKind::Task
        } else {
            MsgKind::Note
        };
        let envelope = envelope(kind, &to, &text, image)?;
        self.io
            .request_ok(AdapterMsg::Plugin(PluginOp::Send(Box::new(envelope))))
            .await
            .map_err(|error| refusal(&error))?;
        Ok(format!("sent to {to}"))
    }

    /// `onlyne_handoff`: hand this session's task on; the client names the task.
    async fn handoff(&self, arguments: &Value) -> Result<String, String> {
        let to = required_string(arguments, "to")?;
        let text = optional_string(arguments, "text").unwrap_or_default();
        let image = image_argument(arguments)?;
        let args = HandoffArgs {
            // The session's own open task, by the token's record: an empty task
            // id is this path's spelling of it (`docs/v2-CONTRACT.md` §3b).
            task_id: String::new(),
            to: to.clone(),
            text,
            image,
        };
        self.io
            .request_ok(AdapterMsg::Plugin(PluginOp::Handoff(args)))
            .await
            .map_err(|error| refusal(&error))?;
        Ok(format!("handed on to {to}"))
    }

    /// `onlyne_complete`: the verdict, and the result that rides with it.
    async fn complete(&self, arguments: &Value) -> Result<String, String> {
        let outcome = outcome_argument(arguments)?;
        let summary = optional_string(arguments, "summary").unwrap_or_default();
        let head = if summary.is_empty() {
            None
        } else {
            Some(summary)
        };
        let details = optional_string(arguments, "details").filter(|text| !text.is_empty());
        let files = files_argument(arguments)?;
        self.reports
            .complete(String::new(), outcome, head, details, files)
            .await
            .map_err(|error| refusal(&error))?;
        Ok(format!("reported {}", outcome.as_str()))
    }
}

/// One outbound envelope for the `send` frame.
///
/// This process names the recipient, the kind, and the body, and nothing that
/// belongs to the session: the token does (`docs/v2-CONTRACT.md` §3b). `from` is
/// the empty principal and a task's causality is empty, both of which the client
/// stamps from the session's own record — a caller that could name its own role
/// would turn the server's acl into a check of a claim rather than of a fact.
fn envelope(
    kind: MsgKind,
    to: &str,
    text: &str,
    image: Option<ImagePart>,
) -> Result<Envelope, String> {
    let mut body = Body {
        text: None,
        head: None,
        image,
    };
    if !text.is_empty() {
        body.text = Some(text.to_string());
    }
    if body.is_empty() {
        return Err("body requires text or image".to_string());
    }
    Ok(Envelope {
        protocol: PROTOCOL_VERSION,
        id: new_id(),
        op_id: match kind {
            MsgKind::Note => None,
            _ => Some(new_op_id()),
        },
        kind,
        from: Principal::role(""),
        to: Principal::role(to),
        control: None,
        causality: match kind {
            MsgKind::Note => None,
            _ => Some(Causality::default()),
        },
        body,
        ts: chrono::Utc::now(),
        ttl_ms: None,
        admin: false,
    })
}

/// The ack a `hello` is answered with, or the sentence a broken answer earns.
fn welcome_ack(value: &Value) -> Result<HelloAck, String> {
    match serde_json::from_value::<HostOp>(value.clone()) {
        Ok(HostOp::Welcome(ack)) => Ok(ack),
        Ok(other) => Err(format!(
            "onlyne: the client answered hello with a {} frame",
            other.name()
        )),
        Err(error) => Err(format!(
            "onlyne: the client's welcome did not decode: {error}"
        )),
    }
}

/// A host refusal as the model reads it: the code and the host's own sentence,
/// the shape pi's plugin shows on the other drive.
fn refusal(error: &AdapterError) -> String {
    match error {
        AdapterError::Protocol(body) => match &body.error {
            Some(error) => format!("{}: {}", error.code, error.message),
            None => "onlyne: the client refused the call".to_string(),
        },
        AdapterError::Closed => "onlyne: the client connection ended".to_string(),
        AdapterError::Io(error) => format!("onlyne: client socket error: {error}"),
        other => format!("onlyne: {other}"),
    }
}

/// An argument the tool declares as required.
fn required_string(arguments: &Value, key: &str) -> Result<String, String> {
    optional_string(arguments, key).ok_or_else(|| format!("onlyne: {key} is required"))
}

fn optional_string(arguments: &Value, key: &str) -> Option<String> {
    arguments
        .get(key)
        .and_then(Value::as_str)
        .map(str::to_string)
}

/// The outcome, in the protocol's own four spellings.
fn outcome_argument(arguments: &Value) -> Result<Outcome, String> {
    match arguments.get("outcome").and_then(Value::as_str) {
        Some("done") => Ok(Outcome::Done),
        Some("failed") => Ok(Outcome::Failed),
        Some("cancelled") => Ok(Outcome::Cancelled),
        Some("blocked") => Ok(Outcome::Blocked),
        Some(other) => Err(format!(
            "onlyne: unknown outcome {other}; expected done, failed, cancelled, or blocked"
        )),
        None => Err("onlyne: outcome is required".to_string()),
    }
}

fn files_argument(arguments: &Value) -> Result<Vec<String>, String> {
    match arguments.get("files") {
        None | Some(Value::Null) => Ok(Vec::new()),
        Some(Value::Array(files)) => files
            .iter()
            .map(|file| {
                file.as_str()
                    .map(str::to_string)
                    .ok_or_else(|| "onlyne: files takes absolute paths".to_string())
            })
            .collect(),
        Some(_) => Err("onlyne: files takes absolute paths".to_string()),
    }
}

/// One image, read off the path the model named.
///
/// The loader is the CLI's own, so the ceiling, the mime allow-list, and the
/// sentence a refusal carries are the ones an operator sees from
/// `onlyne send --image`: this process decides nothing about an attachment.
fn image_argument(arguments: &Value) -> Result<Option<ImagePart>, String> {
    let Some(path) = optional_string(arguments, "image").filter(|path| !path.is_empty()) else {
        return Ok(None);
    };
    match media::load_image_part(Path::new(&path)) {
        Ok(part) => Ok(Some(part)),
        Err(error) => Err(error.message()),
    }
}
