//! The inbound half: turn a Telegram `Update` into a standard [`ImEvent`].
//!
//! Only what the contract needs is read — the message id (the dedupe key), the
//! chat id (the conversation), the sender, the text, and a button press's
//! `callback_data`. Everything else Telegram sends rides past unread.

use onlyne_im_backend::payload::{ImEvent, ImEventKind, ImReply, ImSender};
use serde_json::Value;

/// Map one Telegram `Update` into the event a trigger script receives.
///
/// `gateway` and `platform` are the backend's own coordinates, filled from its
/// configuration; the rest comes out of the update. A message with no text and
/// no photo, or an update that is neither a message nor a callback, yields
/// nothing.
pub fn event_from_update(gateway: &str, platform: &str, update: &Value) -> Option<ImEvent> {
    if let Some(query) = update.get("callback_query").filter(|v| !v.is_null()) {
        return callback_event(gateway, platform, query);
    }
    let message = update
        .get("message")
        .or_else(|| update.get("edited_message"))?;
    if message.is_null() {
        return None;
    }
    message_event(gateway, platform, message)
}

fn callback_event(gateway: &str, platform: &str, query: &Value) -> Option<ImEvent> {
    let data = query.get("data").and_then(Value::as_str)?;
    // The callback's own id is the dedupe key; the chat it belongs to is on the
    // message it was pressed under.
    let conversation = query
        .get("message")
        .and_then(|m| m.get("chat"))
        .and_then(chat_id)
        .or_else(|| query.get("from").and_then(user_id))?;
    Some(ImEvent {
        gateway: gateway.to_string(),
        platform: platform.to_string(),
        channel: platform.to_string(),
        conversation,
        kind: ImEventKind::Action,
        id: query
            .get("id")
            .and_then(Value::as_str)
            .map(str::to_string)
            .unwrap_or_else(|| format!("cb-{data}")),
        from: user_of(query.get("from")),
        action: Some(data.to_string()),
        at: now(),
        ..ImEvent::default()
    })
}

fn message_event(gateway: &str, platform: &str, message: &Value) -> Option<ImEvent> {
    let conversation = message.get("chat").and_then(chat_id)?;
    let id = message
        .get("message_id")
        .map(|v| v.to_string())
        .unwrap_or_default();
    // A media message carries no `text`; its words are in `caption`.
    let text = message
        .get("text")
        .or_else(|| message.get("caption"))
        .and_then(Value::as_str)
        .map(str::to_string);
    let attachments = attachments_of(message);
    if text.as_ref().is_none_or(|t| t.trim().is_empty()) && attachments.is_empty() {
        return None;
    }
    Some(ImEvent {
        gateway: gateway.to_string(),
        platform: platform.to_string(),
        channel: platform.to_string(),
        conversation,
        kind: ImEventKind::Message,
        id,
        from: user_of(message.get("from")),
        text,
        attachments,
        reply: reply_of(message),
        at: now(),
        ..ImEvent::default()
    })
}

/// What this message answers, and what it quotes.
///
/// Telegram answers a *message*, not a thread, so there is no list coordinate
/// to report: `parent` is the replied-to `message_id` and the root and the
/// thread stay absent. The gateway then anchors on the parent, which is the
/// reading `thread::assign_list` gives a platform with no list of its own.
///
/// The quoted message rides back as a **reference**, not as the text: the
/// message's own words are `text` above, and the thing it answers is material
/// the reader may want (and Telegram is kind enough to deliver in full here,
/// where a history call would be needed elsewhere). It is not the parent in
/// the structural sense — a reply *inside* a thread and a reply that *quotes*
/// one are different things, and only the first forms a chain.
fn reply_of(message: &Value) -> Option<ImReply> {
    let quoted = message.get("reply_to_message");
    let quoted_id = quoted
        .and_then(|q| q.get("message_id"))
        .map(|v| v.to_string())
        .filter(|value| !value.trim().is_empty());
    quoted_id.map(|parent| ImReply {
        parent: Some(parent),
        root: None,
        thread: None,
        quoted_text: quoted
            .and_then(|q| {
                q.get("text")
                    .or_else(|| q.get("caption"))
                    .and_then(Value::as_str)
            })
            .filter(|t| !t.trim().is_empty())
            .map(str::to_string),
        // Telegram hands out one id per message; there is no second handle to
        // keep, and a displayed quote is `reply_to_message_id`, which the send
        // path already sets.
        self_handle: None,
    })
}

fn chat_id(chat: &Value) -> Option<String> {
    chat.get("id").map(id_string)
}

fn user_id(user: &Value) -> Option<String> {
    user.get("id").map(id_string)
}

/// Telegram ids are integers; the wire carries them as strings.
fn id_string(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

fn user_of(user: Option<&Value>) -> Option<ImSender> {
    let user = user.filter(|u| !u.is_null())?;
    let id = user.get("id").map(id_string)?;
    let name = user
        .get("username")
        .and_then(Value::as_str)
        .map(str::to_string)
        .or_else(|| {
            user.get("first_name")
                .and_then(Value::as_str)
                .map(str::to_string)
        });
    Some(ImSender { id, name })
}

/// Telegram reports uploaded media by file id; the gateway writes nothing here.
/// A placeholder path names the file id so the field is nonempty and an
/// operator can see what arrived.
fn attachments_of(message: &Value) -> Vec<onlyne_im_backend::payload::ImAttachment> {
    let mut out = Vec::new();
    if let Some(photos) = message.get("photo").and_then(Value::as_array) {
        if let Some(best) = photos
            .last()
            .and_then(|p| p.get("file_id"))
            .and_then(Value::as_str)
        {
            out.push(onlyne_im_backend::payload::ImAttachment {
                kind: "image".into(),
                path: format!("telegram:file:{best}"),
                alt: None,
            });
        }
    }
    if let Some(doc) = message.get("document") {
        if let Some(file_id) = doc.get("file_id").and_then(Value::as_str) {
            let name = doc.get("file_name").and_then(Value::as_str);
            out.push(onlyne_im_backend::payload::ImAttachment {
                kind: "file".into(),
                path: format!("telegram:file:{file_id}"),
                alt: name.map(str::to_string),
            });
        }
    }
    out
}

fn now() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_text_message_becomes_a_message_event() {
        let update = json!({
            "update_id": 100,
            "message": {
                "message_id": 7,
                "chat": {"id": -1007788, "type": "group"},
                "from": {"id": 42, "first_name": "Ada"},
                "text": "deploy please"
            }
        });
        let event = event_from_update("tg1", "telegram", &update).unwrap();
        assert_eq!(event.kind, ImEventKind::Message);
        assert_eq!(event.id, "7");
        assert_eq!(event.conversation, "-1007788");
        assert_eq!(event.text.as_deref(), Some("deploy please"));
        assert_eq!(event.from.unwrap().id, "42");
    }

    #[test]
    fn a_button_press_becomes_an_action_event() {
        let update = json!({
            "update_id": 101,
            "callback_query": {
                "id": "cb-9",
                "data": "approve",
                "from": {"id": 42, "username": "ada"},
                "message": {"message_id": 8, "chat": {"id": -1007788}}
            }
        });
        let event = event_from_update("tg1", "telegram", &update).unwrap();
        assert_eq!(event.kind, ImEventKind::Action);
        assert_eq!(event.action.as_deref(), Some("approve"));
        assert_eq!(event.id, "cb-9");
        assert_eq!(event.conversation, "-1007788");
    }

    #[test]
    fn an_update_with_nothing_to_read_yields_no_event() {
        assert!(event_from_update("g", "telegram", &json!({"update_id": 5})).is_none());
        assert!(
            event_from_update(
                "g",
                "telegram",
                &json!({"message": {"chat": {"id": 1}, "from": {"id": 2}}})
            )
            .is_none(),
            "a message with no text and no attachment is not an event"
        );
    }

    #[test]
    fn a_photo_message_carries_an_attachment() {
        let update = json!({
            "message": {
                "message_id": 9,
                "chat": {"id": 5},
                "from": {"id": 1},
                "caption": "see this",
                "photo": [{"file_id": "small"}, {"file_id": "big"}]
            }
        });
        let event = event_from_update("g", "telegram", &update).unwrap();
        assert_eq!(event.attachments.len(), 1);
        assert_eq!(event.attachments[0].path, "telegram:file:big");
    }

    #[test]
    fn a_reply_carries_the_message_it_answers_and_what_it_quotes() {
        // Telegram answers a message, not a thread, and is kind enough to
        // deliver the quoted message in full — the context a Feishu reply has
        // to fetch and a QQ quote never has.
        let update = json!({
            "update_id": 1,
            "message": {
                "message_id": 42,
                "chat": {"id": -100},
                "from": {"id": 7, "first_name": "operator"},
                "text": "and another thing",
                "reply_to_message": {
                    "message_id": 41,
                    "chat": {"id": -100},
                    "from": {"id": 9, "first_name": "someone"},
                    "text": "the original words"
                }
            }
        });
        let event = event_from_update("tg1", "telegram", &update).unwrap();
        let reply = event.reply.expect("a reply carries its coordinate");
        assert_eq!(reply.parent.as_deref(), Some("41"));
        assert!(reply.root.is_none(), "Telegram has no list to name");
        assert_eq!(reply.quoted_text.as_deref(), Some("the original words"));
        assert_eq!(event.text.as_deref(), Some("and another thing"));
    }

    #[test]
    fn a_message_that_replies_to_nothing_carries_no_coordinate() {
        let update = json!({
            "update_id": 2,
            "message": {"message_id": 43, "chat": {"id": -100}, "text": "plain"}
        });
        let event = event_from_update("tg1", "telegram", &update).unwrap();
        assert!(event.reply.is_none());
    }
}
