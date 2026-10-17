//! The inbound half: turn a Feishu event into a standard [`ImEvent`].
//!
//! Three event types become events; everything else rides past, the way the
//! v1 loop skipped what it could not read:
//!
//! * `im.message.receive_v1` — a message. Text and rich text carry their words;
//!   images, files, audio, video, stickers, forwarded histories, shared chats
//!   and shared contacts become attachments or a short labelled placeholder,
//!   so no message type is dropped silently.
//! * `im.chat.member.bot.added_v1` — the bot was added to a group. The event
//!   names the chat id as its conversation, so the gateway's listen-all/adopt
//!   path mints a workspace for a group it has just joined, and a trigger
//!   script can bind it. An external (cross-tenant) group is marked in the
//!   sentence, since the platform reports that only here.
//! * `card.action.trigger` — a button press on an interactive card. It becomes
//!   `kind = action` with the button's id, the conversation and the message the
//!   card belongs to (as its reply anchor), so the answer threads under the
//!   card.
//!
//! @-mentions: Feishu puts a placeholder key (`@_user_1`) in the text and a
//! `mentions` table on the message, and a post's `at` segment carries the key
//! as `user_id`. Both are rendered as `@Name` inline, so a trigger script reads
//! the mention in the text it already matches on. The standard event has no
//! `mentioned` flag to set: inventing one would change the core protocol this
//! crate does not own.
//!
//! The conversation is Feishu's chat id: for a group chat the event's
//! `chat_id` (`oc_…`), for a p2p chat the sender's `open_id` (`ou_…`), the
//! address `send()` accepts back. The message id (`om_…`) is the dedupe key.

use onlyne_im_backend::payload::{ImAttachment, ImEvent, ImEventKind, ImReply, ImSender};
use serde_json::Value;

/// Map one decoded Feishu event payload into the event a trigger script
/// receives.
///
/// `gateway` and `platform` are the backend's own coordinates, filled from its
/// configuration. An event this backend does not translate, or one with no
/// readable content and no conversation, yields nothing.
pub fn event_from_payload(gateway: &str, platform: &str, payload: &Value) -> Option<ImEvent> {
    let header = payload.get("header")?;
    let event_type = header.get("event_type")?.as_str()?;
    let event_id = header
        .get("event_id")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let event = payload.get("event")?;
    match event_type {
        "im.message.receive_v1" => message_event(gateway, platform, event, event_id),
        "im.chat.member.bot.added_v1" => bot_added_event(gateway, platform, event, event_id),
        "card.action.trigger" => card_action_event(gateway, platform, event, event_id),
        _ => None,
    }
}

/// A message the bot received.
fn message_event(gateway: &str, platform: &str, event: &Value, event_id: &str) -> Option<ImEvent> {
    let message = event.get("message")?;
    let conversation = resolve_conversation(event)?;
    let (text, attachments) = message_content(message)?;
    let id = message
        .get("message_id")
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())
        .unwrap_or(event_id)
        .to_string();
    Some(ImEvent {
        gateway: gateway.to_string(),
        platform: platform.to_string(),
        channel: platform.to_string(),
        conversation,
        kind: ImEventKind::Message,
        id,
        from: sender_of(event),
        text,
        attachments,
        reply: reply_of(message),
        at: now(),
        ..ImEvent::default()
    })
}

/// The bot joined (or was added to) a chat.
///
/// The chat id is the conversation, which is what makes the event adoptable:
/// the gateway mints a workspace for this chat on the spot, and a trigger
/// script can bind it from the same event. `external` is the platform's own
/// statement that the group lives in another tenant, so it is carried in the
/// sentence — the standard event has no field for it.
fn bot_added_event(
    gateway: &str,
    platform: &str,
    event: &Value,
    event_id: &str,
) -> Option<ImEvent> {
    let chat_id = event.get("chat_id").and_then(Value::as_str)?.to_string();
    let mut text = format!("bot added to chat {chat_id}");
    if let Some(name) = event
        .get("name")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|name| !name.is_empty())
    {
        text.push_str(&format!(" \"{name}\""));
    }
    if event
        .get("external")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        text.push_str(" (external group)");
    }
    if let Some(tenant) = event
        .get("operator_tenant_key")
        .and_then(Value::as_str)
        .filter(|tenant| !tenant.is_empty())
    {
        text.push_str(&format!(" (operator tenant {tenant})"));
    }
    let id = if event_id.is_empty() {
        format!("bot-added:{chat_id}")
    } else {
        event_id.to_string()
    };
    Some(ImEvent {
        gateway: gateway.to_string(),
        platform: platform.to_string(),
        channel: platform.to_string(),
        conversation: chat_id,
        kind: ImEventKind::Message,
        id,
        from: operator_of(event),
        text: Some(text),
        at: now(),
        ..ImEvent::default()
    })
}

/// A button press on an interactive card.
///
/// The callback names the conversation and the message the card is attached
/// to (`context.open_chat_id` / `context.open_message_id`), so the press can
/// be answered in the same conversation and threaded under the card. The
/// button's id rides in the button's `value` as `{"action": "<id>"}` (a plain
/// string is accepted too, for cards sent by other tools).
fn card_action_event(
    gateway: &str,
    platform: &str,
    event: &Value,
    event_id: &str,
) -> Option<ImEvent> {
    let action = event.get("action")?;
    let action_id = action_id_of(action.get("value")?)?;
    let context = event.get("context").cloned().unwrap_or(Value::Null);
    let conversation = context
        .get("open_chat_id")
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())
        .map(str::to_string)
        .or_else(|| {
            event
                .pointer("/operator/open_id")
                .and_then(Value::as_str)
                .map(str::to_string)
        })?;
    let message_id = context
        .get("open_message_id")
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty());
    let id = if event_id.is_empty() {
        format!("action:{action_id}:{}", message_id.unwrap_or(""))
    } else {
        event_id.to_string()
    };
    Some(ImEvent {
        gateway: gateway.to_string(),
        platform: platform.to_string(),
        channel: platform.to_string(),
        conversation,
        kind: ImEventKind::Action,
        id,
        from: event
            .pointer("/operator/open_id")
            .and_then(Value::as_str)
            .map(|open_id| ImSender {
                id: open_id.to_string(),
                name: None,
            }),
        action: Some(action_id),
        // The card this press belongs to is the message an answer threads
        // under, so it is the reply's parent — exactly the coordinate a reply
        // sent with `reply_to = event.reply.answers()` lands on.
        reply: message_id.map(|message_id| ImReply {
            parent: Some(message_id.to_string()),
            ..ImReply::default()
        }),
        at: now(),
        ..ImEvent::default()
    })
}

/// The button id inside a card action's `value`.
fn action_id_of(value: &Value) -> Option<String> {
    match value {
        Value::String(id) => (!id.is_empty()).then(|| id.clone()),
        Value::Object(map) => map
            .get("action")
            .or_else(|| map.get("id"))
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty())
            .map(str::to_string),
        _ => None,
    }
}

/// What this message answers, as Feishu reports it.
///
/// Feishu returns `root_id` and `parent_id` **only in the reply scenario**, and
/// `thread_id` only when the message belongs to a thread — so a message that
/// opens its own discussion carries none of them and the field stays absent.
/// A reply into a discussion this cluster has never recorded still names its
/// anchor here, which is what lets the gateway start a list from the middle.
fn reply_of(message: &Value) -> Option<ImReply> {
    let field = |name: &str| {
        message
            .get(name)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
    };
    let reply = ImReply {
        parent: field("parent_id"),
        root: field("root_id"),
        thread: field("thread_id"),
        // Feishu reports the coordinate but not the quoted body. Fetching it
        // is a history call the operator authorizes; the gateway states the
        // gap rather than filling it, which is what `docs/v2.4` §8 asks for.
        quoted_text: None,
        self_handle: None,
    };
    (!reply.is_empty()).then_some(reply)
}

/// Which chat id addresses this conversation, in the order v1 resolved it: a
/// p2p chat is addressed by the sender's `open_id`, a group by the event's
/// `chat.chat_id`, falling back to `message.chat_id`.
fn resolve_conversation(event: &Value) -> Option<String> {
    let chat_type = event
        .pointer("/message/chat_type")
        .and_then(Value::as_str)
        .unwrap_or("");
    let mut candidates: Vec<Option<String>> = Vec::new();
    if chat_type == "p2p" {
        candidates.push(
            event
                .pointer("/sender/sender_id/open_id")
                .and_then(Value::as_str)
                .map(str::to_string),
        );
    } else {
        candidates.push(
            event
                .pointer("/chat/chat_id")
                .and_then(Value::as_str)
                .map(str::to_string),
        );
    }
    candidates.push(
        event
            .pointer("/message/chat_id")
            .and_then(Value::as_str)
            .map(str::to_string),
    );
    candidates.into_iter().flatten().next()
}

/// The sender, named by its `open_id`. Feishu event v2 payloads carry no
/// display name for the sender, so `name` stays `None`.
fn sender_of(event: &Value) -> Option<ImSender> {
    let open_id = event
        .pointer("/sender/sender_id/open_id")
        .and_then(Value::as_str)?;
    Some(ImSender {
        id: open_id.to_string(),
        name: None,
    })
}

/// The operator behind a membership event, named by its `open_id`.
fn operator_of(event: &Value) -> Option<ImSender> {
    event
        .pointer("/operator_id/open_id")
        .and_then(Value::as_str)
        .map(|open_id| ImSender {
            id: open_id.to_string(),
            name: None,
        })
}

/// What one message says, and what it carries.
///
/// `None` when the message type is one this backend does not read, or when it
/// carries neither words nor an attachment — the two cases where there is
/// nothing a trigger script could act on.
fn message_content(message: &Value) -> Option<(Option<String>, Vec<ImAttachment>)> {
    let message_type = message.get("message_type").and_then(Value::as_str)?;
    let content = message.get("content").and_then(Value::as_str)?;
    let value: Value = serde_json::from_str(content).ok()?;
    let mentions = mentions_of(message);
    let mut attachments = Vec::new();
    let text = match message_type {
        "text" => value
            .get("text")
            .and_then(Value::as_str)
            .map(|text| mentioned_text(text, &mentions)),
        "post" => post_text(&value, &mentions),
        "image" => {
            if let Some(key) = value.get("image_key").and_then(Value::as_str) {
                attachments.push(attachment("image", "image", key, None));
            }
            None
        }
        "file" => {
            if let Some(key) = value.get("file_key").and_then(Value::as_str) {
                attachments.push(attachment(
                    "file",
                    "file",
                    key,
                    value.get("file_name").and_then(Value::as_str),
                ));
            }
            None
        }
        "audio" => {
            if let Some(key) = value.get("file_key").and_then(Value::as_str) {
                attachments.push(attachment("audio", "file", key, None));
            }
            None
        }
        "media" => {
            if let Some(key) = value.get("file_key").and_then(Value::as_str) {
                attachments.push(attachment(
                    "video",
                    "file",
                    key,
                    value.get("file_name").and_then(Value::as_str),
                ));
            }
            None
        }
        "sticker" => {
            if let Some(key) = value.get("file_key").and_then(Value::as_str) {
                attachments.push(attachment("sticker", "file", key, None));
            }
            None
        }
        // A merged history has no words of its own; the id names the message
        // that holds it, which is what a reader asks for next.
        "merge_forward" => Some(match value.get("message_id").and_then(Value::as_str) {
            Some(id) => format!("[forwarded messages: {id}]"),
            None => "[forwarded messages]".to_string(),
        }),
        "share_chat" => Some(match value.get("chat_id").and_then(Value::as_str) {
            Some(id) => format!("[shared chat: {id}]"),
            None => "[shared chat]".to_string(),
        }),
        "share_user" => Some(match value.get("user_id").and_then(Value::as_str) {
            Some(id) => format!("[shared contact: {id}]"),
            None => "[shared contact]".to_string(),
        }),
        _ => return None,
    };
    let text = text.filter(|text| !text.trim().is_empty());
    if text.is_none() && attachments.is_empty() {
        return None;
    }
    Some((text, attachments))
}

/// One attachment named by its platform handle. Nothing is downloaded here:
/// the `path` is the handle an operator (or a later fetch) can resolve, which
/// is the same shape the Telegram backend uses for a file id.
fn attachment(kind: &str, handle: &str, key: &str, alt: Option<&str>) -> ImAttachment {
    ImAttachment {
        kind: kind.to_string(),
        path: format!("feishu:{handle}_key:{key}"),
        alt: alt.map(str::to_string),
    }
}

/// The `mentions` table: placeholder key to display name.
fn mentions_of(message: &Value) -> Vec<(String, String)> {
    message
        .get("mentions")
        .and_then(Value::as_array)
        .map(|mentions| {
            mentions
                .iter()
                .filter_map(|mention| {
                    let key = mention.get("key").and_then(Value::as_str)?.to_string();
                    let name = mention
                        .get("name")
                        .and_then(Value::as_str)
                        .filter(|name| !name.trim().is_empty())
                        .unwrap_or(&key)
                        .to_string();
                    Some((key, name))
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Replace every mention placeholder with `@Name`, which is both what a reader
/// expects and what a trigger can match on.
fn mentioned_text(text: &str, mentions: &[(String, String)]) -> String {
    let mut out = text.to_string();
    for (key, name) in mentions {
        if !key.is_empty() {
            out = out.replace(key, &format!("@{name}"));
        }
    }
    out
}

/// A `post` (rich text) message: the title, then one line per content row.
///
/// A row is its segments' text, with an `at` segment rendered as `@Name` (from
/// the segment's own `user_name` or the message's `mentions` table) and an
/// image, video or sticker segment as a short label — a post is not a place to
/// drop what a person attached. The body may sit under a locale key
/// (`zh_cn`, `en_us`) or at the top level; both are read.
fn post_text(v: &Value, mentions: &[(String, String)]) -> Option<String> {
    let (body, lines) = match v.get("content").and_then(Value::as_array) {
        Some(lines) => (v, lines),
        None => v.as_object()?.iter().find_map(|(_, locale)| {
            locale
                .get("content")
                .and_then(Value::as_array)
                .map(|lines| (locale, lines))
        })?,
    };
    let mut out = String::new();
    if let Some(title) = body
        .get("title")
        .and_then(Value::as_str)
        .filter(|title| !title.trim().is_empty())
    {
        out.push_str(title);
        out.push('\n');
    }
    for line in lines {
        if let Some(segments) = line.as_array() {
            for segment in segments {
                push_segment(&mut out, segment, mentions);
            }
        }
        out.push('\n');
    }
    let text = out.trim();
    (!text.is_empty()).then(|| text.to_string())
}

fn push_segment(out: &mut String, segment: &Value, mentions: &[(String, String)]) {
    match segment.get("tag").and_then(Value::as_str) {
        Some("at") => {
            let key = segment
                .get("user_id")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let name = segment
                .get("user_name")
                .and_then(Value::as_str)
                .filter(|name| !name.trim().is_empty())
                .or_else(|| {
                    mentions
                        .iter()
                        .find(|(mention_key, _)| mention_key == key)
                        .map(|(_, name)| name.as_str())
                })
                .unwrap_or(key);
            if !name.is_empty() {
                out.push('@');
                out.push_str(name);
            }
        }
        Some("img") => out.push_str("[image]"),
        Some("media") => out.push_str("[video]"),
        Some("emotion") => {
            if let Some(kind) = segment.get("emoji_type").and_then(Value::as_str) {
                out.push('[');
                out.push_str(kind);
                out.push(']');
            }
        }
        _ => {
            if let Some(text) = segment.get("text").and_then(Value::as_str) {
                out.push_str(text);
            }
        }
    }
}

fn now() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// A p2p text event, the fixture shape v1's own tests recorded.
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

    /// A group message of one type, so a table can vary the type and content.
    fn group_message(message_type: &str, content: Value) -> Value {
        json!({
            "header": {"event_id": "evt-t", "event_type": "im.message.receive_v1"},
            "event": {
                "sender": {"sender_id": {"open_id": "ou_user"}},
                "chat": {"chat_id": "oc_group"},
                "message": {
                    "message_id": "om_t",
                    "message_type": message_type,
                    "chat_type": "group",
                    "content": content.to_string()
                }
            }
        })
    }

    #[test]
    fn a_reply_names_the_anchor_the_platform_reported() {
        let payload = json!({
            "header": {"event_id": "evt-9", "event_type": "im.message.receive_v1"},
            "event": {
                "sender": {"sender_id": {"open_id": "ou_user"}},
                "message": {
                    "message_id": "om_2",
                    "parent_id": "om_1",
                    "root_id": "om_1",
                    "thread_id": "omt_9",
                    "chat_type": "group",
                    "chat_id": "oc_group",
                    "message_type": "text",
                    "content": "{\"text\":\"and another thing\"}"
                }
            }
        });
        let event = event_from_payload("g", "feishu", &payload).unwrap();
        let reply = event.reply.expect("a reply carries its coordinate");
        assert_eq!(reply.parent.as_deref(), Some("om_1"));
        assert_eq!(reply.root.as_deref(), Some("om_1"));
        assert_eq!(reply.thread.as_deref(), Some("omt_9"));
        assert_eq!(reply.answers(), Some("om_1"));
        assert_eq!(reply.anchor(), Some("om_1"));
    }

    #[test]
    fn a_message_that_opens_its_own_discussion_carries_no_coordinate() {
        // Feishu returns `root_id` and `parent_id` only in the reply scenario.
        let payload = json!({
            "header": {"event_id": "evt-10", "event_type": "im.message.receive_v1"},
            "event": {
                "sender": {"sender_id": {"open_id": "ou_user"}},
                "message": {
                    "message_id": "om_3",
                    "chat_type": "p2p",
                    "message_type": "text",
                    "content": "{\"text\":\"first\"}"
                }
            }
        });
        let event = event_from_payload("g", "feishu", &payload).unwrap();
        assert!(
            event.reply.is_none(),
            "nothing to anchor on: {:?}",
            event.reply
        );
    }

    #[test]
    fn a_p2p_text_message_is_addressed_by_the_sender_open_id() {
        let event = event_from_payload("fw1", "feishu", &text_event()).unwrap();
        assert_eq!(event.kind, ImEventKind::Message);
        assert_eq!(event.id, "om_1");
        assert_eq!(event.conversation, "ou_user");
        assert_eq!(event.text.as_deref(), Some("hi"));
        assert!(event.attachments.is_empty());
        assert_eq!(event.from.unwrap().id, "ou_user");
        assert_eq!(event.platform, "feishu");
        assert_eq!(event.channel, "feishu");
    }

    #[test]
    fn a_group_message_is_addressed_by_the_chat_id() {
        let payload = json!({
            "header": {"event_id": "evt-2", "event_type": "im.message.receive_v1"},
            "event": {
                "sender": {"sender_id": {"open_id": "ou_user"}},
                "chat": {"chat_id": "oc_group"},
                "message": {
                    "message_id": "om_2",
                    "message_type": "text",
                    "chat_type": "group",
                    "content": "{\"text\":\"ship it\"}"
                }
            }
        });
        let event = event_from_payload("fw1", "feishu", &payload).unwrap();
        assert_eq!(event.conversation, "oc_group");
        assert_eq!(event.text.as_deref(), Some("ship it"));
    }

    #[test]
    fn a_group_message_falls_back_to_the_message_chat_id() {
        let payload = json!({
            "header": {"event_id": "evt-3", "event_type": "im.message.receive_v1"},
            "event": {
                "message": {
                    "message_id": "om_3",
                    "message_type": "text",
                    "chat_type": "group",
                    "chat_id": "oc_fallback",
                    "content": "{\"text\":\"ping\"}"
                }
            }
        });
        let event = event_from_payload("g", "feishu", &payload).unwrap();
        assert_eq!(event.conversation, "oc_fallback");
        assert!(event.from.is_none(), "no sender, no from");
    }

    #[test]
    fn a_message_id_falls_back_to_the_event_id() {
        let mut payload = text_event();
        payload["event"]["message"]["message_id"] = Value::Null;
        let event = event_from_payload("g", "feishu", &payload).unwrap();
        assert_eq!(event.id, "evt-1");
    }

    #[test]
    fn a_post_message_yields_title_text_and_attachment_labels() {
        let payload = json!({
            "header": {"event_id": "evt-4", "event_type": "im.message.receive_v1"},
            "event": {
                "chat": {"chat_id": "oc_group"},
                "message": {
                    "message_id": "om_4",
                    "message_type": "post",
                    "chat_type": "group",
                    "content": "{\"title\":\"Report\",\"content\":[[{\"tag\":\"text\",\"text\":\"line one\"},{\"tag\":\"a\",\"text\":\" see docs\"}],[{\"tag\":\"text\",\"text\":\"line two\"},{\"tag\":\"img\",\"image_key\":\"img_v2\"}]]}"
                }
            }
        });
        let event = event_from_payload("g", "feishu", &payload).unwrap();
        assert_eq!(
            event.text.as_deref(),
            Some("Report\nline one see docs\nline two[image]")
        );
    }

    #[test]
    fn a_post_under_a_locale_key_is_read() {
        let payload = group_message(
            "post",
            json!({"zh_cn": {"title": "日报", "content": [[{"tag": "text", "text": "已发版"}]]}}),
        );
        let event = event_from_payload("g", "feishu", &payload).unwrap();
        assert_eq!(event.text.as_deref(), Some("日报\n已发版"));
    }

    #[test]
    fn a_text_mention_is_rendered_with_the_name_from_the_mentions_table() {
        let payload = json!({
            "header": {"event_id": "evt-m", "event_type": "im.message.receive_v1"},
            "event": {
                "sender": {"sender_id": {"open_id": "ou_user"}},
                "chat": {"chat_id": "oc_group"},
                "message": {
                    "message_id": "om_m",
                    "message_type": "text",
                    "chat_type": "group",
                    "content": "{\"text\":\"@_user_1 please deploy\"}",
                    "mentions": [
                        {"key": "@_user_1", "id": {"open_id": "ou_bot"}, "name": "Onlyne"}
                    ]
                }
            }
        });
        let event = event_from_payload("g", "feishu", &payload).unwrap();
        assert_eq!(event.text.as_deref(), Some("@Onlyne please deploy"));
    }

    #[test]
    fn a_post_at_segment_is_rendered_as_a_named_mention() {
        let payload = group_message(
            "post",
            json!({
                "title": "",
                "content": [[
                    {"tag": "at", "user_id": "@_user_1"},
                    {"tag": "text", "text": " take a look"}
                ]]
            }),
        );
        let mut payload = payload;
        payload["event"]["message"]["mentions"] =
            json!([{"key": "@_user_1", "id": {"open_id": "ou_bot"}, "name": "Onlyne"}]);
        let event = event_from_payload("g", "feishu", &payload).unwrap();
        assert_eq!(event.text.as_deref(), Some("@Onlyne take a look"));
    }

    /// Every widened type: what the event must carry, so none is a silent drop.
    #[test]
    fn widened_message_types_become_attachments_or_a_labelled_placeholder() {
        let cases: Vec<(&str, Value, Option<&str>, Option<&str>)> = vec![
            (
                "image",
                json!({"image_key": "img_v3_1"}),
                None,
                Some("image|feishu:image_key:img_v3_1"),
            ),
            (
                "file",
                json!({"file_key": "file_v3_1", "file_name": "plan.pdf"}),
                None,
                Some("file|feishu:file_key:file_v3_1"),
            ),
            (
                "audio",
                json!({"file_key": "file_v3_2", "duration": 3000}),
                None,
                Some("audio|feishu:file_key:file_v3_2"),
            ),
            (
                "media",
                json!({"file_key": "file_v3_3", "file_name": "demo.mp4", "duration": 5000}),
                None,
                Some("video|feishu:file_key:file_v3_3"),
            ),
            (
                "sticker",
                json!({"file_key": "file_v3_4"}),
                None,
                Some("sticker|feishu:file_key:file_v3_4"),
            ),
            (
                "merge_forward",
                json!({"message_id": "om_forward"}),
                Some("[forwarded messages: om_forward]"),
                None,
            ),
            (
                "share_chat",
                json!({"chat_id": "oc_shared"}),
                Some("[shared chat: oc_shared]"),
                None,
            ),
            (
                "share_user",
                json!({"user_id": "ou_shared"}),
                Some("[shared contact: ou_shared]"),
                None,
            ),
        ];
        for (message_type, content, expected_text, expected_attachment) in cases {
            let payload = group_message(message_type, content);
            let event = event_from_payload("g", "feishu", &payload)
                .unwrap_or_else(|| panic!("{message_type} must not be dropped"));
            assert_eq!(event.text.as_deref(), expected_text, "{message_type} text");
            match expected_attachment {
                Some(spec) => {
                    let (kind, path) = spec.split_once('|').unwrap();
                    assert_eq!(event.attachments.len(), 1, "{message_type}");
                    assert_eq!(event.attachments[0].kind, kind, "{message_type}");
                    assert_eq!(event.attachments[0].path, path, "{message_type}");
                }
                None => assert!(
                    event.attachments.is_empty(),
                    "{message_type} carries no attachment"
                ),
            }
            assert_eq!(event.conversation, "oc_group");
        }
    }

    #[test]
    fn a_file_attachment_keeps_the_platform_file_name() {
        let payload = group_message("file", json!({"file_key": "k", "file_name": "plan.pdf"}));
        let event = event_from_payload("g", "feishu", &payload).unwrap();
        assert_eq!(event.attachments[0].alt.as_deref(), Some("plan.pdf"));
    }

    #[test]
    fn the_bot_added_event_names_the_chat_so_a_trigger_can_bind_it() {
        let payload = json!({
            "header": {"event_id": "evt-add", "event_type": "im.chat.member.bot.added_v1"},
            "event": {
                "chat_id": "oc_new",
                "name": "Release",
                "external": false,
                "operator_id": {"open_id": "ou_adder"},
                "operator_tenant_key": "tenant_1"
            }
        });
        let event = event_from_payload("g", "feishu", &payload).unwrap();
        assert_eq!(event.kind, ImEventKind::Message);
        assert_eq!(event.conversation, "oc_new");
        assert_eq!(event.id, "evt-add");
        assert_eq!(event.from.as_ref().unwrap().id, "ou_adder");
        let text = event.text.unwrap();
        assert!(text.contains("oc_new"), "{text}");
        assert!(text.contains("Release"), "{text}");
        assert!(!text.contains("external group"), "{text}");
        assert!(text.contains("tenant_1"), "{text}");
    }

    #[test]
    fn an_external_group_addition_says_so() {
        let payload = json!({
            "header": {"event_id": "evt-ext", "event_type": "im.chat.member.bot.added_v1"},
            "event": {"chat_id": "oc_ext", "name": "Partners", "external": true}
        });
        let event = event_from_payload("g", "feishu", &payload).unwrap();
        let text = event.text.unwrap();
        assert!(text.contains("external group"), "{text}");
        assert_eq!(event.conversation, "oc_ext");
    }

    #[test]
    fn a_card_button_press_is_an_action_event_anchored_on_the_card_message() {
        let payload = json!({
            "header": {"event_id": "evt-card", "event_type": "card.action.trigger"},
            "event": {
                "operator": {"open_id": "ou_presser"},
                "action": {"tag": "button", "value": {"action": "approve"}},
                "context": {"open_chat_id": "oc_group", "open_message_id": "om_card"}
            }
        });
        let event = event_from_payload("g", "feishu", &payload).unwrap();
        assert_eq!(event.kind, ImEventKind::Action);
        assert_eq!(event.action.as_deref(), Some("approve"));
        assert_eq!(event.conversation, "oc_group");
        assert_eq!(event.id, "evt-card");
        assert_eq!(event.from.as_ref().unwrap().id, "ou_presser");
        assert!(event.text.is_none());
        let reply = event.reply.expect("the card message is the reply target");
        assert_eq!(reply.parent.as_deref(), Some("om_card"));
        assert_eq!(reply.answers(), Some("om_card"));
    }

    #[test]
    fn a_card_action_may_carry_its_id_as_a_plain_string() {
        let payload = json!({
            "header": {"event_id": "evt-card-2", "event_type": "card.action.trigger"},
            "event": {
                "action": {"value": "retry"},
                "context": {"open_chat_id": "oc_group", "open_message_id": "om_card"}
            }
        });
        let event = event_from_payload("g", "feishu", &payload).unwrap();
        assert_eq!(event.action.as_deref(), Some("retry"));
    }

    #[test]
    fn a_card_action_with_no_button_id_is_skipped() {
        let payload = json!({
            "header": {"event_id": "evt-card-3", "event_type": "card.action.trigger"},
            "event": {
                "action": {"value": {"other": "x"}},
                "context": {"open_chat_id": "oc_group"}
            }
        });
        assert!(event_from_payload("g", "feishu", &payload).is_none());
    }

    #[test]
    fn anything_not_a_translated_event_is_skipped() {
        let payload = json!({
            "header": {"event_id": "evt-5", "event_type": "im.chat.member.updated"},
            "event": {}
        });
        assert!(event_from_payload("g", "feishu", &payload).is_none());
    }

    #[test]
    fn a_message_type_with_no_content_is_skipped() {
        let payload = group_message("image", json!({}));
        assert!(event_from_payload("g", "feishu", &payload).is_none());
    }

    #[test]
    fn a_message_with_no_text_is_skipped() {
        let mut payload = text_event();
        payload["event"]["message"]["content"] = json!("{}");
        assert!(event_from_payload("g", "feishu", &payload).is_none());
    }

    #[test]
    fn malformed_content_json_is_skipped() {
        let mut payload = text_event();
        payload["event"]["message"]["content"] = json!("not json");
        assert!(event_from_payload("g", "feishu", &payload).is_none());
    }
}
