//! onlyne-gateway-feishu — Feishu (Lark) platform gateway plugin (S10).
//!
//! Implements [`onlyne_adapter::GatewayPlugin`] for the Feishu IM platform:
//!
//! - **Inbound**: Feishu websocket events (`im.message.receive_v1`) are
//!   translated into [`Envelope`]s with [`Principal::Gateway`] from/to and a
//!   `Note` or `Task` [`MsgKind`], then pushed through
//!   [`GatewayHost::deliver_inbound`]. Unsupported event/message types are
//!   rejected with a clean `AdapterError` instead of half-parsed envelopes.
//! - **Outbound**: [`Outbound`] messages render to Feishu text or interactive
//!   card request JSON (plus a multipart image upload for `Body.image`). The
//!   2 MiB image ceiling is enforced by the pure translation helpers *before*
//!   any upload, using `onlyne_proto::IMAGE_DATA_MAX_BYTES` — the gateway kit
//!   is never imported.
//! - **Correlation**: a local [`GatewayRefTable`] maps
//!   `(channel, conversation, external_id, scene)` with a lossless
//!   `gateway_ref` encode/decode round trip, so cross-process traffic only
//!   carries `Principal::Gateway` and `reply_to` (§10.3).
//!
//! Platform SDK boundary: `open_lark` is this crate's own dependency;
//! `onlyne-adapter` / `onlyne-proto` are the only workspace dependencies.

pub mod auth;

use std::collections::HashMap;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use onlyne_adapter::AdapterError;
use onlyne_adapter::plugin::{
    AdapterHealth, GatewayHost, GatewayPlugin, OnboardingKind, OnboardingPrompt, Outbound,
    SendReceipt,
};
use onlyne_proto::{
    Body, Capability, Causality, Envelope, ErrorCode, GatewayHealth, IMAGE_DATA_MAX_BYTES, MsgKind,
    Principal, RegisterChannelArgs, new_envelope, new_task_id,
};
use serde::Deserialize;
use serde_json::{Value, json};

pub use auth::{APP_ID_ENV, APP_SECRET_ENV, DEFAULT_DOMAIN, DOMAIN_ENV, FeishuCredentials};

pub const PLATFORM: &str = "feishu";
pub const CHANNEL: &str = "feishu";

/// Feishu message types translated into envelopes; anything else (image,
/// file, audio, media, location, sticker, redpacket, system, …) is rejected
/// cleanly instead of half-parsed.
pub const SUPPORTED_MESSAGE_TYPES: [&str; 2] = ["text", "post"];

/// The 2 MiB decoded-image ceiling, from the shared protocol.
pub const IMAGE_MAX_BYTES: usize = IMAGE_DATA_MAX_BYTES;

/// Default role inbound Feishu messages are addressed to when the host does
/// not supply one.
pub const DEFAULT_TARGET_ROLE: &str = "planner";

// ---------------------------------------------------------------------------
// Correlation table (§10.3)
// ---------------------------------------------------------------------------

/// Local association between an external platform message and the Onlyne
/// conversation it belongs to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GatewayRef {
    pub channel: String,
    pub conversation: String,
    pub external_id: String,
    pub scene: Option<String>,
}

/// Encode a ref into an opaque, lossless string for host-side storage.
///
/// Fields are base64url-encoded and `|`-joined; every input round-trips
/// through [`parse_gateway_ref`].
pub fn gateway_ref(
    channel: &str,
    conversation: &str,
    external_id: &str,
    scene: Option<&str>,
) -> String {
    let enc = |s: &str| URL_SAFE_NO_PAD.encode(s.as_bytes());
    format!(
        "{}|{}|{}|{}",
        enc(channel),
        enc(conversation),
        enc(external_id),
        enc(scene.unwrap_or(""))
    )
}

/// Decode a string produced by [`gateway_ref`]. Returns `None` on any
/// malformed input.
pub fn parse_gateway_ref(encoded: &str) -> Option<GatewayRef> {
    let mut parts = encoded.split('|');
    let channel = decode_part(parts.next()?)?;
    let conversation = decode_part(parts.next()?)?;
    let external_id = decode_part(parts.next()?)?;
    let scene = decode_part(parts.next()?)?;
    if parts.next().is_some() {
        return None;
    }
    Some(GatewayRef {
        channel,
        conversation,
        external_id,
        scene: (!scene.is_empty()).then_some(scene),
    })
}

fn decode_part(part: &str) -> Option<String> {
    if part.is_empty() {
        return Some(String::new());
    }
    let bytes = URL_SAFE_NO_PAD.decode(part).ok()?;
    String::from_utf8(bytes).ok()
}

/// Local mapping table for the gateway process (§10.3). The host may store
/// refs in its own DB instead; this in-memory table covers the common case.
#[derive(Debug, Default)]
pub struct GatewayRefTable {
    by_external: HashMap<String, GatewayRef>,
    by_conversation: HashMap<(String, String), GatewayRef>,
}

impl GatewayRefTable {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record a ref and return its encoded [`gateway_ref`] string.
    pub fn insert(&mut self, r: GatewayRef) -> String {
        let key = (r.channel.clone(), r.conversation.clone());
        let encoded = gateway_ref(
            &r.channel,
            &r.conversation,
            &r.external_id,
            r.scene.as_deref(),
        );
        self.by_external.insert(r.external_id.clone(), r.clone());
        self.by_conversation.insert(key, r);
        encoded
    }

    /// Look up by external platform message id.
    pub fn by_external(&self, external_id: &str) -> Option<&GatewayRef> {
        self.by_external.get(external_id)
    }

    /// Look up by (channel, conversation).
    pub fn by_conversation(&self, channel: &str, conversation: &str) -> Option<&GatewayRef> {
        self.by_conversation
            .get(&(channel.to_string(), conversation.to_string()))
    }

    pub fn len(&self) -> usize {
        self.by_external.len()
    }

    pub fn is_empty(&self) -> bool {
        self.by_external.is_empty()
    }
}

// ---------------------------------------------------------------------------
// Inbound: Feishu event → Envelope
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct FeishuEvent {
    pub header: FeishuHeader,
    pub event: FeishuEventBody,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct FeishuHeader {
    #[serde(default)]
    pub event_id: Option<String>,
    #[serde(default)]
    pub event_type: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct FeishuEventBody {
    #[serde(default)]
    pub sender: Option<FeishuSender>,
    #[serde(default)]
    pub message: Option<FeishuMessage>,
    #[serde(default)]
    pub chat: Option<FeishuChat>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct FeishuSender {
    #[serde(default)]
    pub sender_id: Option<FeishuSenderId>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct FeishuSenderId {
    #[serde(default)]
    pub open_id: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct FeishuChat {
    #[serde(default)]
    pub chat_id: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct FeishuMessage {
    #[serde(default)]
    pub message_id: Option<String>,
    #[serde(default)]
    pub message_type: Option<String>,
    #[serde(default)]
    pub chat_type: Option<String>,
    #[serde(default)]
    pub chat_id: Option<String>,
    #[serde(default)]
    pub content: Option<String>,
}

/// Which [`MsgKind`] an inbound event maps to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InboundKind {
    Note,
    Task,
}

/// Translate a decoded Feishu event payload into an envelope addressed from
/// the gateway principal.
///
/// `gateway_id` names this gateway process (used in `Principal::Gateway`),
/// `target_role` is the role inbound conversations are handed to, and `kind`
/// selects `Note` (no causality) or `Task` (root causality chain).
pub fn inbound_update(
    payload: &Value,
    gateway_id: &str,
    target_role: &str,
    kind: InboundKind,
) -> Result<Envelope, AdapterError> {
    let ev: FeishuEvent = serde_json::from_value(payload.clone())
        .map_err(|e| AdapterError::new(ErrorCode::Invalid, format!("feishu event decode: {e}")))?;
    inbound_event(&ev, gateway_id, target_role, kind)
}

pub fn inbound_update_bytes(
    payload: &[u8],
    gateway_id: &str,
    target_role: &str,
    kind: InboundKind,
) -> Result<Envelope, AdapterError> {
    let ev: FeishuEvent = serde_json::from_slice(payload)
        .map_err(|e| AdapterError::new(ErrorCode::Invalid, format!("feishu event decode: {e}")))?;
    inbound_event(&ev, gateway_id, target_role, kind)
}

fn unsupported(what: &str) -> AdapterError {
    AdapterError::new(
        ErrorCode::Invalid,
        format!("feishu unsupported message type: {what}"),
    )
}

fn inbound_event(
    ev: &FeishuEvent,
    gateway_id: &str,
    target_role: &str,
    kind: InboundKind,
) -> Result<Envelope, AdapterError> {
    if ev.header.event_type != "im.message.receive_v1" {
        return Err(AdapterError::new(
            ErrorCode::Invalid,
            format!("feishu unsupported event type: {}", ev.header.event_type),
        ));
    }
    let msg = ev.event.message.as_ref().ok_or_else(|| {
        AdapterError::new(
            ErrorCode::Invalid,
            "feishu event missing message".to_string(),
        )
    })?;
    let msg_type = msg
        .message_type
        .as_deref()
        .ok_or_else(|| unsupported("missing"))?;
    if !SUPPORTED_MESSAGE_TYPES.contains(&msg_type) {
        return Err(unsupported(msg_type));
    }
    let conversation = resolve_conversation(ev).ok_or_else(|| {
        AdapterError::new(
            ErrorCode::Invalid,
            "feishu event has no resolvable conversation".to_string(),
        )
    })?;
    let text = message_text(msg).ok_or_else(|| {
        AdapterError::new(
            ErrorCode::Invalid,
            "feishu message has no readable text".to_string(),
        )
    })?;

    let external_id = msg
        .message_id
        .clone()
        .or_else(|| ev.header.event_id.clone());
    let from = Principal::gateway(
        gateway_id.to_string(),
        CHANNEL.to_string(),
        Some(conversation.clone()),
    );
    let to = Principal::role(target_role.to_string());
    let (msg_kind, mut causality) = match kind {
        InboundKind::Note => (MsgKind::Note, None),
        InboundKind::Task => (MsgKind::Task, Some(Causality::root(new_task_id()))),
    };
    if let (Some(chain), Some(external_id)) = (causality.as_mut(), external_id.as_deref()) {
        chain.reply_to = Some(gateway_ref(CHANNEL, &conversation, external_id, None));
    }
    new_envelope(msg_kind, from, to, Body::text(text), causality)
        .map_err(|e| AdapterError::new(ErrorCode::Invalid, format!("feishu envelope: {e}")))
}

fn resolve_conversation(ev: &FeishuEvent) -> Option<String> {
    let chat_type = ev
        .event
        .message
        .as_ref()
        .and_then(|m| m.chat_type.as_deref())
        .unwrap_or("");
    let mut candidates = Vec::new();
    if chat_type == "p2p" {
        if let Some(open_id) = ev
            .event
            .sender
            .as_ref()
            .and_then(|s| s.sender_id.as_ref())
            .and_then(|id| id.open_id.clone())
        {
            candidates.push(open_id);
        }
    } else if let Some(chat_id) = ev.event.chat.as_ref().and_then(|c| c.chat_id.clone()) {
        candidates.push(chat_id);
    }
    if let Some(chat_id) = ev.event.message.as_ref().and_then(|m| m.chat_id.clone()) {
        candidates.push(chat_id);
    }
    candidates.first().cloned()
}

/// Extract readable text from a `text` or `post` message content.
fn message_text(msg: &FeishuMessage) -> Option<String> {
    let content = msg.content.as_deref()?;
    let v: Value = serde_json::from_str(content).ok()?;
    match msg.message_type.as_deref() {
        Some("text") => v.get("text").and_then(Value::as_str).map(str::to_string),
        Some("post") => post_text(&v),
        _ => None,
    }
}

fn post_text(v: &Value) -> Option<String> {
    let mut out = String::new();
    if let Some(title) = v.get("title").and_then(Value::as_str) {
        out.push_str(title);
        out.push('\n');
    }
    if let Some(lines) = v.get("content").and_then(Value::as_array) {
        for line in lines {
            if let Some(segs) = line.as_array() {
                for seg in segs {
                    if let Some(t) = seg.get("text").and_then(Value::as_str) {
                        out.push_str(t);
                    }
                }
            }
            out.push('\n');
        }
    }
    let t = out.trim();
    (!t.is_empty()).then(|| t.to_string())
}

// ---------------------------------------------------------------------------
// Outbound: Outbound → Feishu request JSON
// ---------------------------------------------------------------------------

/// What one outbound [`Outbound`] turns into on the wire.
#[derive(Debug, Clone, PartialEq)]
pub enum FeishuPart {
    /// Full send-message request JSON (`POST /open-apis/im/v1/messages`).
    Text(Value),
    /// Image that must be uploaded first (2 MiB ceiling already enforced).
    Image(ImageUpload),
}

/// Pure pre-upload image payload; the size limit is checked by
/// [`image_upload`] before this is ever built.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageUpload {
    pub bytes: Vec<u8>,
    pub mime: String,
    pub file_name: String,
}

fn check_kind(msg: &Outbound) -> Result<(), AdapterError> {
    if !matches!(msg.kind, MsgKind::Note | MsgKind::Task) {
        return Err(AdapterError::new(
            ErrorCode::Invalid,
            format!("feishu unsupported outbound kind: {}", msg.kind),
        ));
    }
    Ok(())
}

/// Validate and prepare the optional image, enforcing the 2 MiB ceiling
/// *before* any upload. Returns `Ok(None)` when the outbound has no image.
pub fn image_upload(msg: &Outbound) -> Result<Option<ImageUpload>, AdapterError> {
    let Some(img) = &msg.image else {
        return Ok(None);
    };
    if !onlyne_proto::IMAGE_MIMES.contains(&img.mime.as_str()) {
        return Err(AdapterError::new(
            ErrorCode::Invalid,
            format!("feishu image mime unsupported: {}", img.mime),
        ));
    }
    let bytes = img
        .decode()
        .map_err(|e| AdapterError::new(ErrorCode::Invalid, format!("feishu image decode: {e}")))?;
    if bytes.len() > IMAGE_MAX_BYTES {
        return Err(AdapterError::new(
            ErrorCode::Invalid,
            format!("feishu image exceeds {IMAGE_MAX_BYTES} bytes; refusing to upload"),
        ));
    }
    Ok(Some(ImageUpload {
        bytes,
        mime: img.mime.clone(),
        file_name: img.name.clone().unwrap_or_else(|| "image".to_string()),
    }))
}

/// Split an outbound into the ordered parts `send()` posts: optional text
/// (plain or markdown card) then optional image. Errors on unsupported kinds,
/// empty messages, or oversized images (before any upload).
pub fn outbound_parts(msg: &Outbound) -> Result<Vec<FeishuPart>, AdapterError> {
    check_kind(msg)?;
    let mut parts = Vec::new();
    let text = msg.text.trim();
    if !text.is_empty() {
        parts.push(FeishuPart::Text(if looks_like_markdown(text) {
            card_request(&msg.conversation, text, msg.reply_to.as_deref())
        } else {
            text_request(&msg.conversation, text, msg.reply_to.as_deref())
        }));
    }
    if let Some(upload) = image_upload(msg)? {
        parts.push(FeishuPart::Image(upload));
    }
    if parts.is_empty() {
        return Err(AdapterError::new(
            ErrorCode::Invalid,
            "feishu outbound message has no text or image".to_string(),
        ));
    }
    Ok(parts)
}

/// Telegram-parallel helper: the primary wire request for an outbound without
/// an image. Markdown routes to the interactive card; plain text to `text`.
pub fn outbound_request(msg: &Outbound) -> Result<Value, AdapterError> {
    outbound_parts(msg)?
        .into_iter()
        .find_map(|part| match part {
            FeishuPart::Text(request) => Some(request),
            FeishuPart::Image(_) => None,
        })
        .ok_or_else(|| {
            AdapterError::new(
                ErrorCode::Invalid,
                "feishu outbound message has no text or image".to_string(),
            )
        })
}

/// The wire request body for a plain-text message.
pub fn text_request(conversation: &str, text: &str, reply_to: Option<&str>) -> Value {
    wire_request(conversation, "text", json!({"text": text}), reply_to)
}

/// The wire request body for an interactive card built from markdown.
pub fn card_request(conversation: &str, markdown: &str, reply_to: Option<&str>) -> Value {
    wire_request(
        conversation,
        "interactive",
        markdown_card(markdown),
        reply_to,
    )
}

/// The wire request body for an uploaded image, keyed by `image_key`.
pub fn image_message_request(conversation: &str, image_key: &str, reply_to: Option<&str>) -> Value {
    wire_request(
        conversation,
        "image",
        json!({"image_key": image_key}),
        reply_to,
    )
}

fn wire_request(
    conversation: &str,
    msg_type: &str,
    content: Value,
    reply_to: Option<&str>,
) -> Value {
    let mut body = json!({
        "receive_id": conversation,
        "msg_type": msg_type,
        "content": content.to_string(),
    });
    if let Some(reply_to) = reply_to.filter(|r| !r.is_empty()) {
        body["reply_id"] = Value::String(reply_to.to_string());
    }
    body
}

/// Heuristic: does this text contain markdown worth rendering as a card?
fn looks_like_markdown(text: &str) -> bool {
    text.contains("**")
        || text.contains('`')
        || text.lines().any(|l| {
            let t = l.trim_start();
            t.starts_with('#')
                || t.starts_with("```")
                || t.starts_with('|')
                || t.starts_with("- ")
                || t.starts_with("* ")
                || t.starts_with("> ")
                || (t.starts_with(|c: char| c.is_ascii_digit()) && t.contains(". "))
        })
}

/// Render markdown to a Feishu interactive card: first `# ` heading becomes
/// the header; tables become card tables; the rest stays a markdown element.
/// Ported locally (the gateway kit is off-limits for plugins).
pub fn markdown_card(markdown: &str) -> Value {
    let (title, body) = split_first_heading(markdown);
    let mut card = json!({
        "config": {"wide_screen_mode": true},
        "elements": markdown_elements(&body),
    });
    if let Some(title) = title {
        card["header"] =
            json!({"template": "blue", "title": {"tag": "plain_text", "content": title}});
    }
    card
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Segment {
    Text(String),
    Table(String),
}

fn split_tables(input: &str) -> Vec<Segment> {
    let lines: Vec<&str> = input.lines().collect();
    let mut out = Vec::new();
    let mut buf = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        if i + 1 < lines.len() && looks_like_table_header(lines[i], lines[i + 1]) {
            flush_text(&mut out, &mut buf);
            let mut table = vec![lines[i], lines[i + 1]];
            i += 2;
            while i < lines.len() && lines[i].contains('|') && !lines[i].trim().is_empty() {
                table.push(lines[i]);
                i += 1;
            }
            out.push(Segment::Table(table.join("\n")));
            continue;
        }
        buf.push(lines[i]);
        i += 1;
    }
    flush_text(&mut out, &mut buf);
    out
}

fn looks_like_table_header(header: &str, sep: &str) -> bool {
    header.contains('|')
        && sep.contains('|')
        && sep
            .chars()
            .all(|c| matches!(c, '|' | '-' | ':' | ' ' | '\t'))
        && sep.contains("---")
}

fn flush_text(out: &mut Vec<Segment>, buf: &mut Vec<&str>) {
    let text = buf.join("\n").trim().to_string();
    if !text.is_empty() {
        out.push(Segment::Text(text));
    }
    buf.clear();
}

fn split_first_heading(input: &str) -> (Option<String>, String) {
    let mut lines = input.lines();
    if let Some(first) = lines.next() {
        if let Some(title) = first.strip_prefix("# ").filter(|s| !s.trim().is_empty()) {
            return (
                Some(title.trim().to_string()),
                lines.collect::<Vec<_>>().join("\n").trim().to_string(),
            );
        }
    }
    (None, input.to_string())
}

fn markdown_elements(markdown: &str) -> Vec<Value> {
    let mut elements = Vec::new();
    for segment in split_tables(markdown) {
        match segment {
            Segment::Text(text) => {
                let content = markdown_body(&text);
                if !content.is_empty() {
                    elements.push(json!({"tag": "markdown", "content": content}));
                }
            }
            Segment::Table(table) => {
                if let Some(table) = table_element(&table) {
                    elements.push(table);
                }
            }
        }
    }
    if elements.is_empty() {
        elements.push(json!({"tag": "markdown", "content": markdown_body(markdown)}));
    }
    elements
}

/// Turn one backtick-inline code span into a Feishu-markdown code block
/// (ported from the legacy adapter).
fn markdown_body(markdown: &str) -> String {
    let chars: Vec<char> = markdown.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] != '`' {
            out.push(chars[i]);
            i += 1;
            continue;
        }
        let mut run = 1;
        while i + run < chars.len() && chars[i + run] == '`' {
            run += 1;
        }
        if run != 1 {
            out.extend(std::iter::repeat_n('`', run));
            i += run;
            continue;
        }
        i += 1;
        let mut code = String::new();
        while i < chars.len() && chars[i] != '`' {
            code.push(chars[i]);
            i += 1;
        }
        if i < chars.len() {
            i += 1;
            out.push_str("\n```\n");
            out.push_str(&code);
            out.push_str("\n```\n");
        } else {
            out.push('`');
            out.push_str(&code);
        }
    }
    out.trim().to_string()
}

fn table_element(table: &str) -> Option<Value> {
    let rows = parse_table_rows(table);
    let header = rows.first()?;
    let width = header.len();
    if width == 0 {
        return None;
    }
    let columns: Vec<Value> = header
        .iter()
        .enumerate()
        .map(|(i, name)| {
            json!({
                "name": format!("col_{i}"),
                "display_name": if name.is_empty() { format!("Column {}", i + 1) } else { name.clone() },
                "data_type": "text",
                "width": "auto",
            })
        })
        .collect();
    let data_rows: Vec<Value> = rows
        .iter()
        .skip(1)
        .map(|row| {
            let mut obj = serde_json::Map::new();
            for i in 0..width {
                obj.insert(
                    format!("col_{i}"),
                    Value::String(row.get(i).cloned().unwrap_or_default()),
                );
            }
            Value::Object(obj)
        })
        .collect();
    Some(json!({
        "tag": "table",
        "page_size": data_rows.len().clamp(1, 10),
        "row_height": "low",
        "header_style": {"background_style": "grey", "bold": true},
        "columns": columns,
        "rows": data_rows,
    }))
}

fn parse_table_rows(table: &str) -> Vec<Vec<String>> {
    table
        .lines()
        .map(str::trim)
        .filter(|line| line.contains('|'))
        .filter_map(|line| {
            let cells: Vec<String> = line
                .trim_matches('|')
                .split('|')
                .map(|cell| cell.trim().to_string())
                .collect();
            if cells.iter().all(|cell| {
                let c = cell.replace(':', "");
                c.contains('-') && c.chars().all(|ch| ch == '-')
            }) {
                None
            } else {
                Some(cells)
            }
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Plugin
// ---------------------------------------------------------------------------

/// Feishu gateway plugin.
pub struct FeishuPlugin {
    creds: FeishuCredentials,
    http: reqwest::Client,
    refs: GatewayRefTable,
    started_at: Instant,
}

impl FeishuPlugin {
    pub fn new(creds: FeishuCredentials) -> Self {
        Self {
            creds,
            http: reqwest::Client::new(),
            refs: GatewayRefTable::new(),
            started_at: Instant::now(),
        }
    }

    pub fn from_env() -> Result<Self, AdapterError> {
        Ok(Self::new(auth::credentials_from_env()?))
    }

    pub fn refs(&self) -> &GatewayRefTable {
        &self.refs
    }

    #[allow(deprecated)]
    fn lark_config(&self) -> Result<open_lark::Config, AdapterError> {
        #[allow(deprecated)]
        open_lark::Config::builder()
            .app_id(self.creds.app_id.clone())
            .app_secret(self.creds.app_secret.clone())
            .base_url(self.creds.base_url())
            .timeout(Duration::from_secs(30))
            .max_response_size(100 * 1024 * 1024)
            .build()
            .map_err(|e| AdapterError::new(ErrorCode::Invalid, format!("feishu lark config: {e}")))
    }

    /// Fetch a tenant access token. Called by [`GatewayPlugin::send`] for
    /// every outbound; kept public so the host can preflight credentials.
    pub async fn tenant_token(&self) -> Result<String, AdapterError> {
        let url = format!(
            "{}/open-apis/auth/v3/tenant_access_token/internal",
            self.creds.base_url()
        );
        let v: Value = self
            .http
            .post(url)
            .json(&json!({"app_id": self.creds.app_id, "app_secret": self.creds.app_secret}))
            .send()
            .await
            .map_err(|e| AdapterError::Unexpected(format!("feishu token request: {e}")))?
            .error_for_status()
            .map_err(|e| AdapterError::Unexpected(format!("feishu token request: {e}")))?
            .json()
            .await
            .map_err(|e| AdapterError::Unexpected(format!("feishu token decode: {e}")))?;
        v.get("tenant_access_token")
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| {
                AdapterError::Unexpected(format!(
                    "feishu token response missing tenant_access_token: {v}"
                ))
            })
    }
}

#[async_trait]
impl GatewayPlugin for FeishuPlugin {
    fn platform(&self) -> &'static str {
        PLATFORM
    }

    fn capabilities(&self) -> Vec<Capability> {
        vec![Capability::Probe]
    }

    async fn start(&mut self, host: &mut dyn GatewayHost) -> Result<(), AdapterError> {
        host.register_channel(&RegisterChannelArgs {
            platform: PLATFORM.to_string(),
            channel: CHANNEL.to_string(),
            conversations: None,
        })
        .await?;
        // Preflight credentials before declaring the channel healthy.
        self.tenant_token().await?;
        let started = self.started_at;
        let payload_tx = spawn_websocket(self.lark_config()?);
        let mut rx = payload_tx;
        let mut interval = tokio::time::interval(Duration::from_secs(30));
        // Advance past the immediate tick so the first health lands after 30s.
        interval.tick().await;
        loop {
            tokio::select! {
                payload = rx.recv() => {
                    let Some(payload) = payload else { break };
                    let ev: FeishuEvent = match serde_json::from_slice(&payload) {
                        Ok(ev) => ev,
                        Err(_) => continue,
                    };
                    if ev.header.event_type != "im.message.receive_v1" {
                        continue;
                    }
                    let external_id = ev.event.message.as_ref()
                        .and_then(|m| m.message_id.clone())
                        .or_else(|| ev.header.event_id.clone());
                    match inbound_event(&ev, "feishu", DEFAULT_TARGET_ROLE, InboundKind::Note) {
                        Ok(envelope) => {
                            if let (Some(conversation), Some(external_id)) = (
                                gateway_conversation(&envelope).map(str::to_string),
                                external_id,
                            ) {
                                self.refs.insert(GatewayRef {
                                    channel: CHANNEL.to_string(),
                                    conversation,
                                    external_id,
                                    scene: None,
                                });
                            }
                            if let Err(e) = host.deliver_inbound(&envelope).await {
                                tracing::warn!(error = %e, "feishu deliver_inbound failed");
                            }
                        }
                        Err(e) => {
                            tracing::debug!(error = %e, "feishu inbound skipped");
                        }
                    }
                }
                _ = interval.tick() => {
                    let health = onlyne_proto::HealthArgs {
                        state: GatewayHealth::Online.as_str().to_string(),
                        detail: None,
                        uptime_s: started.elapsed().as_secs(),
                    };
                    if let Err(e) = host.report_health(&health).await {
                        tracing::warn!(error = %e, "feishu report_health failed");
                    }
                }
            }
        }
        Ok(())
    }

    async fn send(&mut self, msg: &Outbound) -> Result<SendReceipt, AdapterError> {
        let token = self.tenant_token().await?;
        let mut last_external = None;
        for part in outbound_parts(msg)? {
            let request = match part {
                FeishuPart::Text(request) => request,
                FeishuPart::Image(upload) => {
                    let key = self.upload_image(&token, &upload).await?;
                    image_message_request(&msg.conversation, &key, msg.reply_to.as_deref())
                }
            };
            last_external = Some(
                self.post_message(&token, &msg.conversation, request)
                    .await?,
            );
        }
        Ok(SendReceipt {
            external_id: last_external.unwrap_or_default(),
        })
    }

    async fn probe(&mut self) -> Result<AdapterHealth, AdapterError> {
        Ok(AdapterHealth {
            state: "online".to_string(),
            detail: None,
            uptime_s: self.started_at.elapsed().as_secs(),
        })
    }

    async fn stop(&mut self, reason: &str) -> Result<(), AdapterError> {
        tracing::info!(reason, "feishu plugin stopped");
        Ok(())
    }

    fn onboarding(&mut self) -> Result<Option<OnboardingPrompt>, AdapterError> {
        Ok(Some(OnboardingPrompt {
            kind: OnboardingKind::ManualCode,
            payload: format!(
                "Create a Feishu custom app at {DEFAULT_DOMAIN}/app, grant im:message and im:resource permissions, then export {APP_ID_ENV} and {APP_SECRET_ENV} (set {DOMAIN_ENV}=https://open.larksuite.com for Lark International)."
            ),
            expires_in: None,
        }))
    }
}

impl FeishuPlugin {
    async fn upload_image(
        &self,
        token: &str,
        upload: &ImageUpload,
    ) -> Result<String, AdapterError> {
        let url = format!("{}/open-apis/im/v1/images", self.creds.base_url());
        let form = reqwest::multipart::Form::new()
            .text("image_type", "message")
            .part(
                "image",
                reqwest::multipart::Part::bytes(upload.bytes.clone())
                    .file_name(upload.file_name.clone()),
            );
        let v: Value = self
            .http
            .post(url)
            .bearer_auth(token)
            .multipart(form)
            .send()
            .await
            .map_err(|e| AdapterError::Unexpected(format!("feishu image upload: {e}")))?
            .error_for_status()
            .map_err(|e| AdapterError::Unexpected(format!("feishu image upload: {e}")))?
            .json()
            .await
            .map_err(|e| AdapterError::Unexpected(format!("feishu image upload decode: {e}")))?;
        v.pointer("/data/image_key")
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| {
                AdapterError::Unexpected(format!("feishu image upload missing image_key: {v}"))
            })
    }

    async fn post_message(
        &self,
        token: &str,
        conversation: &str,
        request: Value,
    ) -> Result<String, AdapterError> {
        let receive_id_type = if conversation.starts_with("ou_") {
            "open_id"
        } else {
            "chat_id"
        };
        let url = format!(
            "{}/open-apis/im/v1/messages?receive_id_type={receive_id_type}",
            self.creds.base_url()
        );
        let v: Value = self
            .http
            .post(url)
            .bearer_auth(token)
            .json(&request)
            .send()
            .await
            .map_err(|e| AdapterError::Unexpected(format!("feishu send: {e}")))?
            .error_for_status()
            .map_err(|e| AdapterError::Unexpected(format!("feishu send: {e}")))?
            .json()
            .await
            .map_err(|e| AdapterError::Unexpected(format!("feishu send decode: {e}")))?;
        if v.get("code").and_then(Value::as_i64).unwrap_or(0) != 0 {
            return Err(AdapterError::Unexpected(format!("feishu send failed: {v}")));
        }
        v.pointer("/data/message_id")
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| AdapterError::Unexpected(format!("feishu send missing message_id: {v}")))
    }
}

fn gateway_conversation(envelope: &Envelope) -> Option<&str> {
    match &envelope.from {
        Principal::Gateway {
            conversation: Some(c),
            ..
        } => Some(c),
        _ => None,
    }
}

// The open_lark WebSocket client takes its own deprecated config type until
// the upstream client builder replaces it.
#[allow(deprecated)]
fn spawn_websocket(config: open_lark::Config) -> tokio::sync::mpsc::UnboundedReceiver<Vec<u8>> {
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<Vec<u8>>();
    tokio::spawn(async move {
        #[allow(deprecated)]
        let handler = open_lark::ws_client::EventDispatcherHandler::builder()
            .payload_sender(tx)
            .build();
        #[allow(deprecated)]
        let result =
            open_lark::ws_client::LarkWsClient::open(std::sync::Arc::new(config), handler).await;
        if let Err(e) = result {
            tracing::warn!(error = ?e, "feishu websocket closed");
        }
    });
    rx
}

#[cfg(test)]
mod tests {
    use super::*;
    use onlyne_adapter::plugin::Outbound;
    use onlyne_proto::ImagePart;

    fn text_event() -> Value {
        json!({
            "header": {"event_id": "evt-1", "event_type": "im.message.receive_v1"},
            "event": {
                "sender": {"sender_id": {"open_id": "ou_user"}},
                "message": {
                    "message_id": "om_1",
                    "message_type": "text",
                    "chat_type": "p2p",
                    "chat_id": "oc_hidden",
                    "content": "{\"text\":\"hi\"}"
                }
            }
        })
    }

    fn plain_outbound() -> Outbound {
        Outbound {
            conversation: "oc_group".into(),
            text: "hello".into(),
            image: None,
            reply_to: None,
            kind: MsgKind::Note,
        }
    }

    #[test]
    fn inbound_text_maps_to_gateway_note() {
        let env = inbound_update(&text_event(), "gw", "planner", InboundKind::Note)
            .expect("inbound text");
        assert_eq!(env.kind, MsgKind::Note);
        match &env.from {
            Principal::Gateway {
                gateway,
                channel,
                conversation,
            } => {
                assert_eq!(gateway, "gw");
                assert_eq!(channel, CHANNEL);
                assert_eq!(conversation.as_deref(), Some("ou_user"));
            }
            other => panic!("unexpected from: {other:?}"),
        }
        assert_eq!(env.body.text.as_deref(), Some("hi"));
    }

    #[test]
    fn inbound_task_carries_causality() {
        let env = inbound_update(&text_event(), "gw", "planner", InboundKind::Task)
            .expect("inbound task");
        assert_eq!(env.kind, MsgKind::Task);
        assert!(env.causality.is_some());
    }

    #[test]
    fn outbound_plain_text_routes_to_text_request() {
        let request = outbound_request(&plain_outbound()).expect("text request");
        assert_eq!(request["msg_type"], Value::String("text".into()));
        assert_eq!(request["receive_id"], Value::String("oc_group".into()));
        assert!(request["content"].as_str().unwrap().contains("hello"));
    }

    #[test]
    fn outbound_markdown_routes_to_interactive_card() {
        let msg = Outbound {
            text: "# hi\n\n**bold**".into(),
            ..plain_outbound()
        };
        let request = outbound_request(&msg).expect("card request");
        assert_eq!(request["msg_type"], Value::String("interactive".into()));
        let card: Value =
            serde_json::from_str(request["content"].as_str().unwrap()).expect("card json");
        assert_eq!(
            card.pointer("/header/title/content")
                .and_then(Value::as_str),
            Some("hi")
        );
    }

    #[test]
    fn gateway_ref_round_trip() {
        let encoded = gateway_ref("feishu", "oc_group", "om_1", Some("reply"));
        let decoded = parse_gateway_ref(&encoded).expect("decode");
        assert_eq!(
            decoded,
            GatewayRef {
                channel: "feishu".into(),
                conversation: "oc_group".into(),
                external_id: "om_1".into(),
                scene: Some("reply".into()),
            }
        );
        let mut table = GatewayRefTable::new();
        let stored = table.insert(decoded.clone());
        assert_eq!(table.by_external("om_1"), Some(&decoded));
        assert_eq!(table.by_conversation("feishu", "oc_group"), Some(&decoded));
        assert_eq!(parse_gateway_ref(&stored), Some(decoded));
        assert!(parse_gateway_ref("not-a-ref").is_none());
    }

    #[test]
    fn unsupported_message_type_is_clean_error() {
        let mut payload = text_event();
        payload["event"]["message"]["message_type"] = Value::String("image".into());
        let err = inbound_update(&payload, "gw", "planner", InboundKind::Note).unwrap_err();
        assert!(
            err.to_string().contains("unsupported message type"),
            "{err}"
        );
    }

    #[test]
    fn oversized_image_rejected_before_upload() {
        let big = vec![0u8; IMAGE_MAX_BYTES + 1];
        let msg = Outbound {
            image: Some(ImagePart {
                data_base64: base64::engine::general_purpose::STANDARD.encode(&big),
                mime: "image/png".into(),
                name: None,
            }),
            ..plain_outbound()
        };
        let err = outbound_parts(&msg).unwrap_err();
        assert!(err.to_string().contains("exceeds"), "{err}");
    }
}
