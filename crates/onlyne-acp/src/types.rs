//! Session-facing types shared with the caller: everything the caller may read
//! from the protocol, or hand to it.

use std::collections::BTreeMap;

use serde::Serialize;
use serde_json::{Value, json};

use crate::wire::RequestId;

/// What the client says about itself in `initialize`.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClientInfo {
    /// Stable identifier for this client implementation.
    pub name: String,
    /// Human-facing label, optional on the wire.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Client version, optional on the wire.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
}

impl ClientInfo {
    pub fn new(name: impl Into<String>) -> Self {
        ClientInfo {
            name: name.into(),
            title: None,
            version: None,
        }
    }
}

/// Client-side capabilities advertised in `initialize`.
///
/// Only the two axes this client can actually serve are typed. Filesystem and
/// terminal proxies are optional in ACP: an agent that is not offered them
/// performs its own file IO, which is what a real coding loop needs anyway, so
/// advertising nothing is a complete configuration, not a degraded one.
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClientCapabilities {
    /// `fs` — whether the agent may call `fs/read_text_file` / `fs/write_text_file`
    /// on us. Unset (or both false) means the agent does its own IO.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fs: Option<FsClientCapabilities>,
    /// `terminal` — whether the agent may call `terminal/*`. Unset means it runs
    /// its own processes.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub terminal: Option<TerminalClientCapabilities>,
    /// Capability keys this crate has not modelled, passed through verbatim.
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FsClientCapabilities {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub read_text_file: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub write_text_file: Option<bool>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct TerminalClientCapabilities {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub terminal: Option<bool>,
}

/// The `initialize` result, projected onto what a caller acts on, with the whole
/// result kept in [`AgentCapabilities::raw`] so nothing is lost.
///
/// The name is the contract's: this is the agent's side of the capability
/// handshake. `agentInfo`, `authMethods` and `protocolVersion` ride along because
/// `initialize` is the only place they appear and a caller needs them to decide
/// whether the agent is usable at all (an unauthenticated agent answers every
/// later request with `-32000`).
#[derive(Debug, Clone, Default)]
pub struct AgentCapabilities {
    /// `protocolVersion`; ACP v1 agents answer `1`.
    pub protocol_version: i64,
    /// `agentCapabilities.loadSession`.
    pub load_session: bool,
    /// `agentCapabilities.sessionCapabilities`, verbatim. Keys such as `close`,
    /// `resume`, `fork`, `list` appear only when the agent supports them.
    pub session_capabilities: Value,
    /// `agentCapabilities.promptCapabilities`, verbatim (`image`, `audio`,
    /// `embeddedContext`).
    pub prompt_capabilities: Value,
    /// `agentInfo`, verbatim; `{}` when the agent omitted it.
    pub agent_info: Value,
    /// `authMethods`, verbatim; empty when the agent needs no auth.
    pub auth_methods: Vec<Value>,
    /// The whole `initialize` result.
    pub raw: Value,
}

impl AgentCapabilities {
    pub(crate) fn from_result(result: &Value) -> Self {
        let capabilities = result.get("agentCapabilities").unwrap_or(&Value::Null);
        AgentCapabilities {
            protocol_version: result
                .get("protocolVersion")
                .and_then(Value::as_i64)
                .unwrap_or(0),
            load_session: capabilities
                .get("loadSession")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            session_capabilities: capabilities
                .get("sessionCapabilities")
                .cloned()
                .unwrap_or_else(|| Value::Object(Default::default())),
            prompt_capabilities: capabilities
                .get("promptCapabilities")
                .cloned()
                .unwrap_or_else(|| Value::Object(Default::default())),
            agent_info: result
                .get("agentInfo")
                .cloned()
                .unwrap_or_else(|| Value::Object(Default::default())),
            auth_methods: result
                .get("authMethods")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default(),
            raw: result.clone(),
        }
    }

    /// Whether the agent advertises the `session/close` method.
    pub fn supports_close(&self) -> bool {
        self.session_capabilities.get("close").is_some()
    }
}

/// An MCP server handed to `session/new`. ACP models these as a tagged union;
/// the tag is the `type` field on the wire.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type")]
pub enum McpServer {
    /// A stdio server the agent starts itself.
    #[serde(rename = "stdio")]
    Stdio {
        name: String,
        command: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        args: Vec<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        env: Vec<EnvVariable>,
    },
    /// An HTTP server, offered only when the agent advertises `mcpCapabilities.http`.
    #[serde(rename = "http")]
    Http {
        name: String,
        url: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        headers: Vec<HttpHeader>,
    },
    /// An SSE server, offered only when the agent advertises `mcpCapabilities.sse`.
    #[serde(rename = "sse")]
    Sse {
        name: String,
        url: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        headers: Vec<HttpHeader>,
    },
}

/// One `name`/`value` pair, the wire shape ACP uses instead of a map so ordering
/// and duplicates survive.
#[derive(Debug, Clone, Serialize)]
pub struct EnvVariable {
    pub name: String,
    pub value: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct HttpHeader {
    pub name: String,
    pub value: String,
}

/// A block of a `session/prompt` payload.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type")]
pub enum ContentBlock {
    /// Plain text — the only block every agent accepts.
    #[serde(rename = "text")]
    Text { text: String },
    /// Base64 image, gated on `promptCapabilities.image`.
    #[serde(rename = "image")]
    Image {
        #[serde(rename = "data")]
        data: String,
        #[serde(rename = "mimeType")]
        mime_type: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        uri: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        title: Option<String>,
    },
    /// Base64 audio, gated on `promptCapabilities.audio`.
    #[serde(rename = "audio")]
    Audio {
        data: String,
        #[serde(rename = "mimeType")]
        mime_type: String,
    },
    /// An inlined resource, gated on `promptCapabilities.embeddedContext`.
    #[serde(rename = "resource")]
    Resource { resource: Value },
    /// A reference to a file the agent can read itself.
    #[serde(rename = "resource_link")]
    ResourceLink {
        uri: String,
        name: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        mime_type: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        title: Option<String>,
    },
}

impl ContentBlock {
    pub fn text(text: impl Into<String>) -> Self {
        ContentBlock::Text { text: text.into() }
    }
}

/// A successful `session/new`.
#[derive(Debug, Clone)]
pub struct SessionStart {
    /// The id every later request for this session is keyed by.
    pub session_id: String,
    /// The raw result, so the caller can read `modes`, `models` and
    /// `configOptions` without this crate modelling an agent's configuration
    /// vocabulary.
    pub result: Value,
}

impl SessionStart {
    pub(crate) fn parse(result: &Value) -> anyhow::Result<SessionStart> {
        let session_id = result
            .get("sessionId")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                anyhow::anyhow!("session/new result carries no string sessionId: {result}")
            })?
            .to_string();
        Ok(SessionStart {
            session_id,
            result: result.clone(),
        })
    }

    /// `modes.currentModeId`, when the agent reports modes.
    pub fn current_mode(&self) -> Option<&str> {
        self.result.get("modes")?.get("currentModeId")?.as_str()
    }

    /// `models.currentModelId`, when the agent reports models.
    pub fn current_model(&self) -> Option<&str> {
        self.result.get("models")?.get("currentModelId")?.as_str()
    }
}

/// A finished `session/prompt` turn.
#[derive(Debug, Clone)]
pub struct PromptOutcome {
    /// `stopReason`, verbatim; empty when the agent omitted it, which a caller
    /// must treat as a failed turn rather than a silent success.
    ///
    /// The values ACP defines are [`PromptOutcome::END_TURN`],
    /// [`PromptOutcome::MAX_TOKENS`], [`PromptOutcome::MAX_TURN_REQUESTS`],
    /// [`PromptOutcome::REFUSAL`] and [`PromptOutcome::CANCELLED`].
    pub stop_reason: String,
    /// The raw result: `usage`, `userMessageId`, `_meta` and any vendor field
    /// stay readable here.
    pub result: Value,
}

impl PromptOutcome {
    pub const END_TURN: &str = "end_turn";
    pub const MAX_TOKENS: &str = "max_tokens";
    pub const MAX_TURN_REQUESTS: &str = "max_turn_requests";
    pub const REFUSAL: &str = "refusal";
    pub const CANCELLED: &str = "cancelled";

    pub(crate) fn parse(result: &Value) -> PromptOutcome {
        PromptOutcome {
            stop_reason: result
                .get("stopReason")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            result: result.clone(),
        }
    }

    /// Whether the turn ended because the agent finished speaking.
    pub fn is_end_turn(&self) -> bool {
        self.stop_reason == Self::END_TURN
    }

    /// Whether the agent stopped because the client cancelled the session.
    pub fn is_cancelled(&self) -> bool {
        self.stop_reason == Self::CANCELLED
    }
}

/// One streamed `session/update` payload.
///
/// One variant per `sessionUpdate` discriminator observed on a live agent, plus
/// [`Update::Unknown`] so an agent on a newer protocol revision extends the
/// stream without this crate dropping information or failing the turn.
#[derive(Debug, Clone)]
pub enum Update {
    /// Assistant text, arriving incrementally.
    AgentMessageChunk(ContentChunk),
    /// Assistant reasoning, arriving incrementally. Show it only if the operator
    /// asked for it; it is not the answer.
    AgentThoughtChunk(ContentChunk),
    /// The user's own text echoed back by the agent.
    UserMessageChunk(ContentChunk),
    /// A tool call the agent started. Carries `toolCallId`, `title`, `status`.
    ToolCall(Value),
    /// Progress on a tool call already announced.
    ToolCallUpdate(Value),
    /// The agent's plan, replacing any previous one wholesale.
    Plan(Value),
    /// A discriminator this crate does not model.
    Unknown(Value),
}

impl Update {
    pub(crate) fn from_params(params: &Value) -> Update {
        let kind = params.get("sessionUpdate").and_then(Value::as_str);
        let chunk = || ContentChunk {
            text: chunk_text(params).unwrap_or_default(),
            raw: params.clone(),
        };
        match kind {
            Some("agent_message_chunk") => Update::AgentMessageChunk(chunk()),
            Some("agent_thought_chunk") => Update::AgentThoughtChunk(chunk()),
            Some("user_message_chunk") => Update::UserMessageChunk(chunk()),
            Some("tool_call") => Update::ToolCall(params.clone()),
            Some("tool_call_update") => Update::ToolCallUpdate(params.clone()),
            Some("plan") => Update::Plan(params.clone()),
            _ => Update::Unknown(params.clone()),
        }
    }

    /// The text of a chunk update, `None` for every other kind.
    pub fn text(&self) -> Option<&str> {
        match self {
            Update::AgentMessageChunk(chunk)
            | Update::AgentThoughtChunk(chunk)
            | Update::UserMessageChunk(chunk) => Some(&chunk.text),
            _ => None,
        }
    }

    /// The update params, verbatim, for every kind.
    pub fn raw(&self) -> &Value {
        match self {
            Update::AgentMessageChunk(chunk)
            | Update::AgentThoughtChunk(chunk)
            | Update::UserMessageChunk(chunk) => &chunk.raw,
            Update::ToolCall(value)
            | Update::ToolCallUpdate(value)
            | Update::Plan(value)
            | Update::Unknown(value) => value,
        }
    }

    /// `toolCallId` for the two tool kinds, `None` otherwise.
    pub fn tool_call_id(&self) -> Option<&str> {
        match self {
            Update::ToolCall(value) | Update::ToolCallUpdate(value) => {
                value.get("toolCallId").and_then(Value::as_str)
            }
            _ => None,
        }
    }
}

/// A content chunk with its text already flattened out of the ACP content block.
#[derive(Debug, Clone)]
pub struct ContentChunk {
    pub text: String,
    pub raw: Value,
}

/// Pull the text out of a chunk's `content`.
///
/// ACP sends one content block; agents have been seen sending an array of them
/// instead, so both shapes flatten to one string rather than dropping the update.
fn chunk_text(params: &Value) -> Option<String> {
    match params.get("content")? {
        Value::Object(_) => block_text(params.get("content")?),
        Value::Array(blocks) => {
            let mut text = String::new();
            for block in blocks {
                text.push_str(&block_text(block)?);
            }
            Some(text)
        }
        _ => None,
    }
}

fn block_text(block: &Value) -> Option<String> {
    if let Some(text) = block.get("text").and_then(Value::as_str) {
        return Some(text.to_string());
    }
    // A non-text block still contributes its raw text field if it has one, and
    // nothing otherwise: an image chunk has no prose to surface.
    block
        .get("content")
        .and_then(Value::as_str)
        .map(str::to_string)
}

/// An option the agent offers for a permission decision.
#[derive(Debug, Clone, PartialEq)]
pub struct PermissionOption {
    /// The id to echo back in [`PermissionOutcome::Selected`].
    pub option_id: String,
    /// The option's role: `allow_once`, `allow_always`, `reject_once`,
    /// `reject_always`, or a vendor spelling of one of those.
    pub kind: String,
    /// The label, for a human-facing prompt.
    pub name: String,
    /// The option object, verbatim.
    pub raw: Value,
}

impl PermissionOption {
    pub const ALLOW_ONCE: &str = "allow_once";
    pub const ALLOW_ALWAYS: &str = "allow_always";
    pub const REJECT_ONCE: &str = "reject_once";
    pub const REJECT_ALWAYS: &str = "reject_always";

    /// Whether this option refuses rather than grants. A `kind` this crate does
    /// not know is never assumed to refuse.
    pub fn is_reject(&self) -> bool {
        self.kind == Self::REJECT_ONCE || self.kind == Self::REJECT_ALWAYS
    }
}

/// A `session/request_permission` from the agent, already paired with the id the
/// caller must hand to [`crate::Agent::answer`].
#[derive(Debug, Clone)]
pub struct PermissionRequest {
    pub request_id: RequestId,
    pub session_id: String,
    /// The `toolCall` object: `toolCallId`, `title`, `kind`, `status`, `rawInput`.
    pub tool_call: Value,
    pub options: Vec<PermissionOption>,
    /// The full params object.
    pub raw: Value,
}

impl PermissionRequest {
    pub(crate) fn parse(request_id: RequestId, params: &Value) -> PermissionRequest {
        let options = params
            .get("options")
            .and_then(Value::as_array)
            .map(|options| {
                options
                    .iter()
                    .filter_map(|option| {
                        let option_id = option.get("optionId").and_then(Value::as_str)?;
                        Some(PermissionOption {
                            option_id: option_id.to_string(),
                            kind: option
                                .get("kind")
                                .and_then(Value::as_str)
                                .unwrap_or_default()
                                .to_string(),
                            name: option
                                .get("name")
                                .and_then(Value::as_str)
                                .unwrap_or(option_id)
                                .to_string(),
                            raw: option.clone(),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();
        PermissionRequest {
            request_id,
            session_id: params
                .get("sessionId")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            tool_call: params
                .get("toolCall")
                .cloned()
                .unwrap_or(Value::Object(Default::default())),
            options,
            raw: params.clone(),
        }
    }

    /// The first option whose `kind` matches, for the caller's policy to pick a
    /// default out of (`option(PermissionOption::REJECT_ONCE)` is how a refuse
    /// by default is answered without hard-coding an agent's option ids).
    pub fn option(&self, kind: &str) -> Option<&PermissionOption> {
        self.options.iter().find(|option| option.kind == kind)
    }

    /// A rejecting option, whatever its `kind` spelling.
    pub fn rejection_option(&self) -> Option<&PermissionOption> {
        self.options.iter().find(|option| option.is_reject())
    }
}

/// The caller's answer to a [`PermissionRequest`].
#[derive(Debug, Clone, PartialEq)]
pub enum PermissionOutcome {
    /// Pick one of the agent's offered options by id.
    Selected { option_id: String },
    /// Decline to decide: ACP's `cancelled`, which every agent treats as "do not
    /// do the thing" and continues the turn.
    Cancelled,
}

impl PermissionOutcome {
    /// The `session/request_permission` result object.
    pub(crate) fn to_result(&self) -> Value {
        match self {
            PermissionOutcome::Selected { option_id } => json!({
                "outcome": {"outcome": "selected", "optionId": option_id}
            }),
            PermissionOutcome::Cancelled => json!({"outcome": {"outcome": "cancelled"}}),
        }
    }
}

/// Everything an agent sends that the caller, not this crate, decides what to do
/// with. One channel per agent process; every session's traffic carries its
/// `session_id` so a caller can demultiplex.
#[derive(Debug, Clone)]
pub enum Event {
    /// A streamed `session/update` notification.
    Update { session_id: String, update: Update },
    /// A request the agent is waiting on. Answer it with
    /// [`crate::Agent::answer`] and `request_id`, or the agent parks until it
    /// gives up on the turn.
    Permission(PermissionRequest),
    /// The agent process left. `detail` names the exit status and the tail of
    /// its stderr, which is what an operator reads when a turn dies mid-flight.
    /// Every outstanding request fails when this arrives.
    Exited { detail: String },
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_permission_outcome_takes_the_shape_the_agent_expects() {
        assert_eq!(
            PermissionOutcome::Selected {
                option_id: "proceed_once".to_string()
            }
            .to_result(),
            json!({"outcome": {"outcome": "selected", "optionId": "proceed_once"}})
        );
        assert_eq!(
            PermissionOutcome::Cancelled.to_result(),
            json!({"outcome": {"outcome": "cancelled"}})
        );
    }

    #[test]
    fn a_streamed_update_flattens_its_content_text() {
        let single = Update::from_params(&json!({
            "sessionId": "s1",
            "sessionUpdate": "agent_message_chunk",
            "content": {"type": "text", "text": "hello"}
        }));
        assert_eq!(single.text(), Some("hello"));

        let array = Update::from_params(&json!({
            "sessionUpdate": "agent_thought_chunk",
            "content": [{"type": "text", "text": "a"}, {"type": "text", "text": "b"}]
        }));
        assert_eq!(array.text(), Some("ab"));

        let image = Update::from_params(&json!({
            "sessionUpdate": "agent_message_chunk",
            "content": {"type": "image", "data": "AAA", "mimeType": "image/png"}
        }));
        assert_eq!(image.text(), Some(""));
    }

    #[test]
    fn an_unmodelled_update_kind_reaches_the_caller_intact() {
        let params = json!({"sessionUpdate": "available_commands_update", "commands": []});
        let update = Update::from_params(&params);

        assert!(matches!(update, Update::Unknown(_)));
        assert_eq!(update.raw(), &params);
        assert_eq!(update.text(), None);
    }

    #[test]
    fn a_permission_request_exposes_its_options_by_kind() {
        let request = PermissionRequest::parse(
            RequestId::Number(3),
            &json!({
                "sessionId": "s1",
                "toolCall": {"toolCallId": "t1", "title": "write"},
                "options": [
                    {"optionId": "always", "name": "Always allow", "kind": "allow_always"},
                    {"optionId": "proceed_once", "name": "Allow", "kind": "allow_once"},
                    {"optionId": "block", "name": "Reject", "kind": "reject_once"},
                ]
            }),
        );

        assert_eq!(request.session_id, "s1");
        assert_eq!(request.options.len(), 3);
        assert_eq!(
            request
                .option(PermissionOption::REJECT_ONCE)
                .map(|o| o.option_id.as_str()),
            Some("block")
        );
        assert_eq!(
            request.rejection_option().map(|o| o.option_id.as_str()),
            Some("block")
        );
        assert_eq!(request.tool_call["title"], json!("write"));
    }

    #[test]
    fn a_permission_request_without_options_still_routes() {
        let request = PermissionRequest::parse(
            RequestId::Text("p".to_string()),
            &json!({"sessionId": "s1"}),
        );

        assert!(request.options.is_empty());
        assert_eq!(request.session_id, "s1");
        assert_eq!(request.request_id, RequestId::Text("p".to_string()));
    }

    #[test]
    fn initialize_result_projects_without_losing_fields() {
        let result = json!({
            "protocolVersion": 1,
            "agentInfo": {"name": "qoder-cli-cn", "version": "1.1.55"},
            "authMethods": [{"id": "qoderclicn-login", "name": "Login", "description": "d"}],
            "agentCapabilities": {
                "loadSession": true,
                "sessionCapabilities": {"close": {}, "resume": {}},
                "promptCapabilities": {"image": true, "embeddedContext": true},
            },
        });

        let caps = AgentCapabilities::from_result(&result);

        assert_eq!(caps.protocol_version, 1);
        assert!(caps.load_session);
        assert!(caps.supports_close());
        assert_eq!(caps.agent_info["version"], json!("1.1.55"));
        assert_eq!(caps.auth_methods.len(), 1);
        assert_eq!(caps.prompt_capabilities["image"], json!(true));
        assert_eq!(caps.raw, result);
    }

    #[test]
    fn a_sparse_initialize_result_projects_to_defaults() {
        let caps = AgentCapabilities::from_result(&json!({}));

        assert_eq!(caps.protocol_version, 0);
        assert!(!caps.load_session);
        assert!(!caps.supports_close());
        assert!(caps.auth_methods.is_empty());
    }

    #[test]
    fn mcp_servers_and_content_blocks_carry_their_wire_names() {
        let servers = json!([
            McpServer::Stdio {
                name: "fs".to_string(),
                command: "server".to_string(),
                args: vec!["--stdio".to_string()],
                env: vec![EnvVariable {
                    name: "KEY".to_string(),
                    value: "v".to_string()
                }],
            },
            McpServer::Http {
                name: "remote".to_string(),
                url: "https://example.invalid/mcp".to_string(),
                headers: vec![HttpHeader {
                    name: "authorization".to_string(),
                    value: "Bearer x".to_string()
                }],
            },
        ]);
        assert_eq!(
            servers[0],
            json!({"type": "stdio", "name": "fs", "command": "server", "args": ["--stdio"],
                   "env": [{"name": "KEY", "value": "v"}]})
        );
        assert_eq!(
            servers[1],
            json!({"type": "http", "name": "remote", "url": "https://example.invalid/mcp",
                   "headers": [{"name": "authorization", "value": "Bearer x"}]})
        );

        assert_eq!(
            serde_json::to_value(ContentBlock::text("hi")).unwrap(),
            json!({"type": "text", "text": "hi"})
        );
    }

    #[test]
    fn advertised_capabilities_omit_what_is_not_set() {
        let value = serde_json::to_value(ClientCapabilities::default()).unwrap();
        assert_eq!(value, json!({}));

        let value = serde_json::to_value(ClientCapabilities {
            fs: Some(FsClientCapabilities {
                read_text_file: Some(false),
                write_text_file: None,
            }),
            extra: BTreeMap::from([("custom".to_string(), json!(true))]),
            ..Default::default()
        })
        .unwrap();
        assert_eq!(
            value,
            json!({"fs": {"readTextFile": false}, "custom": true})
        );
    }

    #[test]
    fn session_start_reads_the_ids_an_agent_reports() {
        let start = SessionStart::parse(&json!({
            "sessionId": "s1",
            "modes": {"availableModes": [], "currentModeId": "default"},
            "models": {"availableModels": [], "currentModelId": "gmodel"},
        }))
        .unwrap();

        assert_eq!(start.session_id, "s1");
        assert_eq!(start.current_mode(), Some("default"));
        assert_eq!(start.current_model(), Some("gmodel"));
    }

    #[test]
    fn session_start_without_a_session_id_is_refused() {
        assert!(SessionStart::parse(&json!({"modes": {}})).is_err());
    }
}
