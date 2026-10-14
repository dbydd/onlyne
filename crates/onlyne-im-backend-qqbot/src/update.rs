//! The inbound half: turn one QQ Open Platform gateway dispatch into a
//! standard [`ImEvent`].
//!
//! The shapes are QQ's websocket event payloads, as v1 read them
//! (`frozen/gateway-v1:plugins/onlyne-gateway-qqbot/src/lib.rs`,
//! `parse_inbound_event`): a dispatch carries `op` (0 is a server dispatch),
//! `t` (the event type) and `d` (the payload). Only what the event needs is
//! read — the message id (the dedupe key), the conversation id per scene, the
//! sender, the text. Everything else rides past unread.
//!
//! The four scenes QQ addressable:
//!
//! | event type | scene | conversation id |
//! |---|---|---|
//! | `GROUP_AT_MESSAGE_CREATE` / `GROUP_MESSAGE_CREATE` | `group` | `group_openid` |
//! | `C2C_MESSAGE_CREATE` | `c2c` | `author.user_openid` |
//! | `AT_MESSAGE_CREATE` | `channel` | `channel_id` |
//! | `DIRECT_MESSAGE_CREATE` | `direct` | `guild_id` |

use onlyne_im_backend::payload::{ImAttachment, ImEvent, ImEventKind, ImReply, ImSender};
use serde_json::Value;

/// Map one QQ gateway dispatch into the event a trigger script receives.
///
/// `gateway` and `platform` are the backend's own coordinates. A dispatch
/// that is not a message (`op != 0`), an unsupported event type, or a message
/// with no readable text yields nothing — the websocket keeps running.
pub fn event_from_dispatch(gateway: &str, platform: &str, dispatch: &Value) -> Option<ImEvent> {
    let op = dispatch.get("op").and_then(Value::as_i64).unwrap_or(0);
    if op != 0 {
        // Heartbeats (`op = 1`), confirms and reconnect notices are the
        // websocket layer's business, not messages.
        return None;
    }
    let event_type = dispatch
        .get("t")
        .or_else(|| dispatch.get("event"))
        .and_then(Value::as_str)?;
    let data = dispatch
        .get("d")
        .or_else(|| dispatch.get("data"))
        .unwrap_or(dispatch);
    if data.is_null() {
        return None;
    }

    let id = data
        .get("id")
        .or_else(|| data.get("msg_id"))
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .unwrap_or("qqbot-inbound")
        .to_string();
    let text = clean_content(data.get("content").and_then(Value::as_str));
    text.as_ref()?;

    let (conversation, from) = match event_type {
        "GROUP_AT_MESSAGE_CREATE" | "GROUP_MESSAGE_CREATE" => (
            format!("group:{}", required_string(data, "group_openid")?),
            user_of(data.get("author")),
        ),
        "C2C_MESSAGE_CREATE" => {
            let user = data
                .get("author")
                .and_then(|a| a.get("user_openid"))
                .and_then(Value::as_str)
                .filter(|value| !value.trim().is_empty())?;
            (format!("c2c:{user}"), user_of(data.get("author")))
        }
        "AT_MESSAGE_CREATE" => (
            format!("channel:{}", required_string(data, "channel_id")?),
            user_of(data.get("author")),
        ),
        "DIRECT_MESSAGE_CREATE" => (
            format!("direct:{}", required_string(data, "guild_id")?),
            user_of(data.get("author")),
        ),
        _ => return None,
    };

    Some(ImEvent {
        gateway: gateway.to_string(),
        platform: platform.to_string(),
        channel: platform.to_string(),
        conversation,
        kind: ImEventKind::Message,
        id,
        from,
        text,
        attachments: attachments_of(data),
        reply: reply_of(data),
        at: now(),
        ..ImEvent::default()
    })
}

/// What this message quotes, as QQ reports it.
///
/// QQ has no threads: a quoted reply names one message and nothing else, so
/// `parent` is the whole coordinate and the root and the thread stay absent —
/// the `Chain` shape, one forward pointer per message.
///
/// The target rides in `message_scene.ext`, whose `msg_idx` is the
/// `REFIDX_…` handle the platform uses for a quoted message
/// (`发送群聊消息`'s `message_reference.message_id` names the same field for a
/// non-bot message). The quoted *body* is not in the event; a reader who needs
/// it either has it in this cluster's record or asks the platform.
///
/// Shape from the documented event, not yet measured: the live QQ traffic this
/// backend has seen was plain text with no quote.
fn reply_of(data: &Value) -> Option<ImReply> {
    let ext = data.pointer("/message_scene/ext")?;
    let entries = ext.as_array()?;
    let msg_idx = entries
        .iter()
        .find_map(|entry| entry.get("msg_idx").and_then(Value::as_str))
        .map(str::trim)
        .filter(|value| !value.is_empty())?;
    Some(ImReply {
        parent: Some(msg_idx.to_string()),
        root: None,
        thread: None,
        quoted_text: None,
    })
}

/// Strip the bot's own `@` mention, which QQ leaves inside `content` as
/// `<@!{bot_openid}>` or `<@{bot_openid}>`, then trim what remains. Content
/// that was only the mention yields `None` — there is nothing to say.
fn clean_content(content: Option<&str>) -> Option<String> {
    let mut text = content?.trim().to_owned();
    while let Some(start) = text.find("<@") {
        let Some(end) = text[start..].find('>').map(|offset| start + offset) else {
            break;
        };
        text.replace_range(start..=end, "");
    }
    let text = text.trim().to_owned();
    (!text.is_empty()).then_some(text)
}

fn required_string(value: &Value, field: &str) -> Option<String> {
    value
        .get(field)
        .and_then(Value::as_str)
        .filter(|text| !text.trim().is_empty())
        .map(str::to_owned)
}

fn user_of(author: Option<&Value>) -> Option<ImSender> {
    let author = author.filter(|a| !a.is_null())?;
    let id = author
        .get("user_openid")
        .or_else(|| author.get("member_openid"))
        .or_else(|| author.get("id"))
        .map(|value| match value {
            Value::String(text) => text.clone(),
            other => other.to_string(),
        })
        .filter(|value| !value.trim().is_empty())?;
    let name = author
        .get("username")
        .and_then(Value::as_str)
        .map(str::to_string);
    Some(ImSender { id, name })
}

/// A dispatch may carry an attachment descriptor even when the bot cannot
/// download it (QQ's file fetch needs a second authenticated call); the path
/// names the descriptor so the field is nonempty and an operator can see
/// what arrived.
fn attachments_of(data: &Value) -> Vec<ImAttachment> {
    let mut out = Vec::new();
    if let Some(attachments) = data.get("attachments").and_then(Value::as_array) {
        for attachment in attachments {
            let Some(kind) = attachment
                .get("content_type")
                .and_then(Value::as_str)
                .map(|value| value.split('/').next().unwrap_or("file").to_string())
            else {
                continue;
            };
            let url = attachment
                .get("url")
                .and_then(Value::as_str)
                .unwrap_or_default();
            out.push(ImAttachment {
                kind,
                path: format!("qqbot:attachment:{url}"),
                alt: None,
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

    fn group_event() -> Value {
        json!({
            "op": 0,
            "t": "GROUP_MESSAGE_CREATE",
            "d": {
                "id": "m1",
                "content": "<@!bot> hello",
                "group_openid": "group-1",
                "author": {"member_openid": "member-1", "username": "alice"}
            }
        })
    }

    #[test]
    fn a_group_message_names_the_group_openid() {
        let event = event_from_dispatch("qq1", "qqbot", &group_event()).unwrap();
        assert_eq!(event.kind, ImEventKind::Message);
        assert_eq!(event.id, "m1");
        // The scene rides in the conversation so a reply picks the right
        // endpoint: a bare `group_openid` sends to `/channels/…`, which QQ
        // answers 400 for. The backend saw that live.
        assert_eq!(event.conversation, "group:group-1");
        assert_eq!(event.text.as_deref(), Some("hello"));
        assert_eq!(event.from.as_ref().unwrap().id, "member-1");
        assert_eq!(event.from.as_ref().unwrap().name.as_deref(), Some("alice"));
        assert_eq!(event.gateway, "qq1");
        assert_eq!(event.platform, "qqbot");
        assert_eq!(event.channel, "qqbot");
    }

    #[test]
    fn the_bot_mention_is_stripped_from_content() {
        let event = json!({
            "op": 0,
            "t": "GROUP_AT_MESSAGE_CREATE",
            "d": {
                "id": "m2",
                "content": "<@!1234abcd> do this",
                "group_openid": "group-1",
                "author": {"member_openid": "member-1"}
            }
        });
        let event = event_from_dispatch("g", "qqbot", &event).unwrap();
        assert_eq!(event.text.as_deref(), Some("do this"));
    }

    #[test]
    fn a_c2c_message_takes_the_user_openid() {
        let event = json!({
            "op": 0,
            "t": "C2C_MESSAGE_CREATE",
            "d": {
                "id": "m3",
                "content": "hi",
                "author": {"user_openid": "user-7"}
            }
        });
        let event = event_from_dispatch("g", "qqbot", &event).unwrap();
        assert_eq!(event.conversation, "c2c:user-7");
        assert_eq!(event.from.unwrap().id, "user-7");
    }

    #[test]
    fn channel_and_direct_events_take_their_own_ids() {
        let channel = json!({
            "op": 0,
            "t": "AT_MESSAGE_CREATE",
            "d": {"id": "m4", "content": "ch", "channel_id": "ch-1", "author": {"id": "u1"}}
        });
        let direct = json!({
            "op": 0,
            "t": "DIRECT_MESSAGE_CREATE",
            "d": {"id": "m5", "content": "dm", "guild_id": "guild-1", "author": {"id": "u1"}}
        });
        assert_eq!(
            event_from_dispatch("g", "qqbot", &channel)
                .unwrap()
                .conversation,
            "channel:ch-1"
        );
        assert_eq!(
            event_from_dispatch("g", "qqbot", &direct)
                .unwrap()
                .conversation,
            "direct:guild-1"
        );
    }

    #[test]
    fn non_dispatch_ops_and_unknown_types_yield_nothing() {
        // op 1 is QQ's heartbeat, op 13 a reconnect notice; neither is mail.
        for op in [1_i64, 13] {
            let dispatch = json!({"op": op, "t": "RESUMED", "d": {"dummy": 1}});
            assert!(
                event_from_dispatch("g", "qqbot", &dispatch).is_none(),
                "op {op}"
            );
        }
        let unknown = json!({"op": 0, "t": "MESSAGE_DELETE", "d": {"id": "m1"}});
        assert!(event_from_dispatch("g", "qqbot", &unknown).is_none());
        assert!(event_from_dispatch("g", "qqbot", &json!({"op": 0})).is_none());
    }

    #[test]
    fn a_message_with_no_text_yields_no_event() {
        let empty_mention = json!({
            "op": 0,
            "t": "GROUP_AT_MESSAGE_CREATE",
            "d": {
                "id": "m6",
                "content": "<@!bot>",
                "group_openid": "group-1"
            }
        });
        assert!(event_from_dispatch("g", "qqbot", &empty_mention).is_none());
        let no_content = json!({
            "op": 0,
            "t": "GROUP_AT_MESSAGE_CREATE",
            "d": {"id": "m7", "group_openid": "group-1"}
        });
        assert!(event_from_dispatch("g", "qqbot", &no_content).is_none());
    }

    #[test]
    fn a_message_missing_its_conversation_id_yields_no_event() {
        let orphan = json!({
            "op": 0,
            "t": "GROUP_MESSAGE_CREATE",
            "d": {"id": "m8", "content": "hello"}
        });
        assert!(event_from_dispatch("g", "qqbot", &orphan).is_none());
    }

    #[test]
    fn attachment_descriptors_are_named_but_not_downloaded() {
        let event = json!({
            "op": 0,
            "t": "GROUP_MESSAGE_CREATE",
            "d": {
                "id": "m9",
                "content": "see this",
                "group_openid": "group-1",
                "attachments": [{"content_type": "image/png", "url": "https://example.invalid/a.png"}]
            }
        });
        let event = event_from_dispatch("g", "qqbot", &event).unwrap();
        assert_eq!(event.attachments.len(), 1);
        assert_eq!(event.attachments[0].kind, "image");
        assert_eq!(
            event.attachments[0].path,
            "qqbot:attachment:https://example.invalid/a.png"
        );
    }

    #[test]
    fn a_quoted_group_message_names_the_ref_it_answers() {
        // QQ carries one forward pointer and no list: an element of
        // `message_scene.ext` names the `REFIDX_…` handle of the message
        // quoted (the shape `发送群聊消息` documents for a quoted reply).
        let dispatch = json!({
            "op": 0,
            "t": "GROUP_AT_MESSAGE_CREATE",
            "d": {
                "id": "m9",
                "content": "<@!bot> about that thing",
                "group_openid": "group-1",
                "author": {"member_openid": "member-1"},
                "message_scene": {"ext": [{"msg_idx": "REFIDX_abc123=="}]}
            }
        });
        let event = event_from_dispatch("qq1", "qqbot", &dispatch).unwrap();
        let reply = event.reply.expect("a quoted reply carries its ref");
        assert_eq!(reply.parent.as_deref(), Some("REFIDX_abc123=="));
        assert!(reply.root.is_none(), "QQ has no list to name");
        assert!(reply.thread.is_none());
    }

    #[test]
    fn a_plain_message_carries_no_ref() {
        let event = event_from_dispatch("qq1", "qqbot", &group_event()).unwrap();
        assert!(event.reply.is_none(), "{:?}", event.reply);
    }
}
