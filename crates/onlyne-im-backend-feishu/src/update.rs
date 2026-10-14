//! The inbound half: turn a Feishu event into a standard [`ImEvent`].
//!
//! Ported from `frozen/gateway-v1`'s translation (`inbound_event`,
//! `resolve_conversation`, `message_text`, `post_text`). Only
//! `im.message.receive_v1` events of message type `text` or `post` become
//! events; anything else yields `None` and rides past, the way the v1 loop
//! skipped what it could not read. v1 rejected unsupported types with an
//! error; the v2.3 backend loop cannot answer an error per event, so the
//! unsupported case is a skip (traced, in the websocket pump).
//!
//! The conversation is Feishu's chat id: for a group chat the event's
//! `chat_id` (`oc_…`), for a p2p chat the sender's `open_id` (`ou_…`), the
//! address `send()` accepts back. The message id (`om_…`) is the dedupe key.

use onlyne_im_backend::payload::{ImEvent, ImEventKind, ImReply, ImSender};
use serde_json::Value;

/// The message types translated into events; anything else is skipped.
pub const SUPPORTED_MESSAGE_TYPES: [&str; 2] = ["text", "post"];

/// Map one decoded Feishu event payload into the event a trigger script
/// receives.
///
/// `gateway` and `platform` are the backend's own coordinates, filled from its
/// configuration. An event that is not `im.message.receive_v1`, a message of
/// an unsupported type, or a message with no readable text and no conversation
/// yields nothing.
pub fn event_from_payload(gateway: &str, platform: &str, payload: &Value) -> Option<ImEvent> {
    if payload.get("header")?.get("event_type")?.as_str()? != "im.message.receive_v1" {
        return None;
    }
    let event = payload.get("event")?;
    let message = event.get("message")?;
    let message_type = message.get("message_type").and_then(Value::as_str)?;
    if !SUPPORTED_MESSAGE_TYPES.contains(&message_type) {
        return None;
    }
    let conversation = resolve_conversation(event)?;
    let text = message_text(message)?;
    if text.trim().is_empty() {
        return None;
    }
    let id = message
        .get("message_id")
        .and_then(Value::as_str)
        .or_else(|| payload.pointer("/header/event_id").and_then(Value::as_str))
        .unwrap_or_default()
        .to_string();
    Some(ImEvent {
        gateway: gateway.to_string(),
        platform: platform.to_string(),
        channel: platform.to_string(),
        conversation,
        kind: ImEventKind::Message,
        id,
        from: sender_of(event),
        text: Some(text),
        reply: reply_of(message),
        at: now(),
        ..ImEvent::default()
    })
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

/// Extract readable text from a `text` or `post` message content. Feishu puts
/// the content on the wire as a JSON-encoded *string*, so it is decoded first.
fn message_text(message: &Value) -> Option<String> {
    let content = message.get("content")?.as_str()?;
    let v: Value = serde_json::from_str(content).ok()?;
    match message.get("message_type").and_then(Value::as_str) {
        Some("text") => v.get("text").and_then(Value::as_str).map(str::to_string),
        Some("post") => post_text(&v),
        _ => None,
    }
}

/// A `post` (rich text) message: the title, then one line per content row,
/// each row the concatenation of its segments' `text` fields. A segment
/// without a `text` field (an image, an at-mention) contributes nothing,
/// exactly as in v1.
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
    fn a_post_message_yields_title_and_text_segments() {
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
            Some("Report\nline one see docs\nline two")
        );
    }

    #[test]
    fn anything_not_a_message_receive_event_is_skipped() {
        let payload = json!({
            "header": {"event_id": "evt-5", "event_type": "im.chat.member.updated"},
            "event": {}
        });
        assert!(event_from_payload("g", "feishu", &payload).is_none());
    }

    #[test]
    fn an_unsupported_message_type_is_skipped() {
        let mut payload = text_event();
        payload["event"]["message"]["message_type"] = json!("image");
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
