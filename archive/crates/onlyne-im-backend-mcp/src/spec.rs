//! Declarative TOML rule specification for `onlyne-im-backend-mcp`.

use anyhow::{Context, Result, bail};
use onlyne_im_backend::payload::{ImAttachment, ImEvent, ImEventKind, ImReply, ImSender};
use serde::Deserialize;
use serde_json::{Map, Value};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

fn default_platform() -> String {
    "mcp".to_string()
}

fn default_poll_every() -> u64 {
    5
}

fn default_empty_object() -> Value {
    Value::Object(Map::new())
}

#[derive(Debug, Clone, Deserialize)]
pub struct McpImSpec {
    #[serde(default = "default_platform")]
    pub platform: String,
    pub mcp: McpServerConfig,
    #[serde(default)]
    pub inbound: InboundSpec,
    #[serde(default)]
    pub outbound: OutboundSpec,
    #[serde(default)]
    pub conversations: ConversationsSpec,
}

#[derive(Debug, Clone, Deserialize)]
pub struct McpServerConfig {
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    #[serde(default)]
    pub env_file: Option<PathBuf>,
    #[serde(default)]
    pub cwd: Option<PathBuf>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct InboundSpec {
    #[serde(default)]
    pub notification: Vec<NotificationRule>,
    #[serde(default)]
    pub poll: Option<PollRule>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct NotificationRule {
    pub method: String,
    #[serde(flatten)]
    pub mapping: EventMapping,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PollRule {
    pub tool: String,
    #[serde(default = "default_poll_every")]
    pub every_secs: u64,
    #[serde(default = "default_empty_object")]
    pub args: Value,
    #[serde(default)]
    pub cursor_pointer: Option<String>,
    #[serde(flatten)]
    pub mapping: EventMapping,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct EventMapping {
    #[serde(default)]
    pub items_pointer: Option<String>,
    pub id: String,
    pub conversation: String,
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default)]
    pub sender_id: Option<String>,
    #[serde(default)]
    pub sender_name: Option<String>,
    #[serde(default)]
    pub reply_parent: Option<String>,
    #[serde(default)]
    pub reply_root: Option<String>,
    #[serde(default)]
    pub reply_thread: Option<String>,
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub action: Option<String>,
    #[serde(default)]
    pub event_type: Option<String>,
    #[serde(default)]
    pub attachments_pointer: Option<String>,
    #[serde(default)]
    pub attachment_path: Option<String>,
    #[serde(default)]
    pub attachment_kind: Option<String>,
    #[serde(default)]
    pub attachment_alt: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct OutboundSpec {
    #[serde(default)]
    pub send: Option<SendRule>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SendRule {
    pub tool: String,
    #[serde(default = "default_empty_object")]
    pub args: Value,
    #[serde(default)]
    pub message_id_pointer: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct ConversationsSpec {
    #[serde(default)]
    pub list: Option<ListConversationsRule>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ListConversationsRule {
    pub tool: String,
    #[serde(default = "default_empty_object")]
    pub args: Value,
    #[serde(default)]
    pub items_pointer: Option<String>,
    pub id: String,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub kind: Option<String>,
}

impl McpImSpec {
    pub fn load(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("reading MCP-IM spec {}", path.display()))?;
        let mut spec: Self = toml::from_str(&text)
            .with_context(|| format!("parsing MCP-IM spec {}", path.display()))?;
        if spec.mcp.command.trim().is_empty() {
            bail!("{}: `[mcp].command` must not be empty", path.display());
        }
        if let Some(base) = path.parent() {
            if let Some(env_file) = &mut spec.mcp.env_file
                && env_file.is_relative()
            {
                *env_file = base.join(&env_file);
            }
            if let Some(cwd) = &mut spec.mcp.cwd
                && cwd.is_relative()
            {
                *cwd = base.join(&cwd);
            }
        }
        Ok(spec)
    }
}

impl EventMapping {
    pub fn extract(&self, gateway: &str, platform: &str, item: &Value) -> Option<ImEvent> {
        let id = select_str(item, &self.id)?;
        let conversation = select_str(item, &self.conversation)?;
        let text = self.text.as_deref().and_then(|p| select_str(item, p));
        let sender = self
            .sender_id
            .as_deref()
            .and_then(|p| select_str(item, p))
            .map(|sid| ImSender {
                id: sid,
                name: self
                    .sender_name
                    .as_deref()
                    .and_then(|np| select_str(item, np)),
            });
        let action = self.action.as_deref().and_then(|p| select_str(item, p));
        let kind = match self
            .kind
            .as_deref()
            .and_then(|p| select_str(item, p).or_else(|| Some(p.to_string())))
            .as_deref()
        {
            Some("action") => ImEventKind::Action,
            _ if action.is_some() => ImEventKind::Action,
            _ => ImEventKind::Message,
        };
        let reply = {
            let r = ImReply {
                parent: self
                    .reply_parent
                    .as_deref()
                    .and_then(|p| select_str(item, p)),
                root: self.reply_root.as_deref().and_then(|p| select_str(item, p)),
                thread: self
                    .reply_thread
                    .as_deref()
                    .and_then(|p| select_str(item, p)),
                quoted_text: None,
                self_handle: None,
            };
            (!r.is_empty()).then_some(r)
        };
        let event_type = self
            .event_type
            .as_deref()
            .and_then(|p| select_str(item, p).or_else(|| Some(p.to_string())));
        let mut attachments = Vec::new();
        if let Some(att_ptr) = self.attachments_pointer.as_deref()
            && let Some(Value::Array(arr)) = select_value(item, att_ptr)
        {
            let path_sel = self.attachment_path.as_deref().unwrap_or("/path");
            let kind_sel = self.attachment_kind.as_deref().unwrap_or("/kind");
            for a in arr {
                if let Some(path) = select_str(a, path_sel) {
                    let akind = select_str(a, kind_sel).unwrap_or_else(|| "file".to_string());
                    let alt = self
                        .attachment_alt
                        .as_deref()
                        .and_then(|p| select_str(a, p));
                    attachments.push(ImAttachment {
                        kind: akind,
                        path,
                        alt,
                    });
                }
            }
        }
        Some(ImEvent {
            gateway: gateway.to_string(),
            platform: platform.to_string(),
            channel: platform.to_string(),
            conversation,
            kind,
            id,
            from: sender,
            text,
            attachments,
            action,
            reply,
            event_type,
            raw: Some(item.clone()),
            at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
        })
    }
}

/// Select a value by JSON Pointer (`/a/b`) or dot path (`a.b`).
pub fn select_value<'a>(root: &'a Value, selector: &str) -> Option<&'a Value> {
    let trimmed = selector.trim();
    if trimmed.is_empty() || trimmed == "." || trimmed == "/" {
        return Some(root);
    }
    if trimmed.starts_with('/') {
        return root.pointer(trimmed);
    }
    let mut current = root;
    for segment in trimmed.split('.') {
        match current {
            Value::Object(map) => current = map.get(segment)?,
            Value::Array(arr) => {
                let idx: usize = segment.parse().ok()?;
                current = arr.get(idx)?;
            }
            _ => return None,
        }
    }
    Some(current)
}

/// Select a scalar value as a `String`.
pub fn select_str(root: &Value, selector: &str) -> Option<String> {
    let val = select_value(root, selector)?;
    match val {
        Value::String(s) if !s.is_empty() => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        _ => None,
    }
}

/// Render a JSON template by substituting `"{{var}}"` placeholders with values
/// from `vars`.
///
/// - When a string value is exactly `"{{var}}"`, it is replaced by `vars[var]`
///   preserving its JSON type (`Null`, `Array`, `String`, etc.), and object keys
///   whose rendered value is `Value::Null` are omitted so optional parameters
///   (`reply_to`) are not sent as `null` unless desired.
/// - When a string contains `"{{var}}"` inside larger text, each placeholder is
///   interpolated as text.
pub fn render_template(template: &Value, vars: &BTreeMap<String, Value>) -> Value {
    match template {
        Value::String(s) => {
            if let Some(var_name) = s.strip_prefix("{{").and_then(|r| r.strip_suffix("}}"))
                && !var_name.contains("{{")
            {
                return vars.get(var_name.trim()).cloned().unwrap_or(Value::Null);
            }
            let mut out = s.clone();
            for (k, v) in vars {
                let token = format!("{{{{{k}}}}}");
                if out.contains(&token) {
                    let replacement = match v {
                        Value::Null => String::new(),
                        Value::String(text) => text.clone(),
                        other => other.to_string(),
                    };
                    out = out.replace(&token, &replacement);
                }
            }
            Value::String(out)
        }
        Value::Array(arr) => Value::Array(
            arr.iter()
                .map(|item| render_template(item, vars))
                .filter(|v| !v.is_null())
                .collect(),
        ),
        Value::Object(map) => {
            let mut next = Map::new();
            for (k, v) in map {
                let rendered = render_template(v, vars);
                if !rendered.is_null() {
                    next.insert(k.clone(), rendered);
                }
            }
            Value::Object(next)
        }
        other => other.clone(),
    }
}
