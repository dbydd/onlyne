//! The inbound half: turn one Slack event into a standard [`ImEvent`].
//!
//! The listener hands over the SDK's typed event, and this module reads the
//! contract's fields off it: the message id (the dedupe key), the channel (the
//! conversation), the sender, the text, the files, and a thread reply's
//! `thread_ts`. Events the SDK models as [`SlackEventCallbackBody::Unknown`]
//! are read the same way from their raw JSON — the catch-all is the SDK's, so
//! a Slack type it has not modelled yet still arrives.
//!
//! The message id is Slack's `ts`, not the Events API `event_id` and not
//! `client_msg_id`: outbound replies carry `thread_ts`, which is a message ts,
//! so a record keyed on anything else would answer with an id the platform
//! cannot thread under.

use onlyne_im_backend::payload::{ImAttachment, ImEvent, ImEventKind, ImReply, ImSender};
use slack_morphism::prelude::*;
use std::collections::HashSet;

/// The message subtypes that still describe a message a person typed.
///
/// Everything else Slack lists as a subtype is either an edit or delete notice
/// (`message_changed`, `message_deleted`), a channel event (`channel_join`,
/// `channel_topic`, …), or a bot integration's post (`bot_message`), and none
/// of those is something a trigger script should answer.
const ACCEPTED_SUBTYPES: [&str; 3] = ["thread_broadcast", "file_share", "me_message"];

/// Map one push event into the event a trigger script receives.
///
/// `self_id` is this bot's own user id, when `auth.test` named it: Slack
/// echoes this bot's posts back as events, and answering one would let a bot
/// talk to itself. `gateway` and `platform` are the backend's own
/// coordinates, filled from its configuration.
pub fn event_from_push(
    gateway: &str,
    platform: &str,
    self_id: Option<&str>,
    push: &SlackPushEventCallback,
) -> Option<ImEvent> {
    match &push.event {
        SlackEventCallbackBody::Message(message) => {
            message_event(gateway, platform, self_id, message)
        }
        SlackEventCallbackBody::AppMention(mention) => {
            mention_event(gateway, platform, self_id, mention)
        }
        SlackEventCallbackBody::Unknown(raw) => raw_event(gateway, platform, self_id, raw),
        _ => None,
    }
}

/// A `message` event: a person's message in a channel, group, or DM.
fn message_event(
    gateway: &str,
    platform: &str,
    self_id: Option<&str>,
    message: &SlackMessageEvent,
) -> Option<ImEvent> {
    // Any post carrying `bot_id` comes from an app, including this bot's own
    // messages echoed back.
    if message.sender.bot_id.is_some() || same_user(self_id, message.sender.user.as_ref()) {
        return None;
    }
    if let Some(subtype) = &message.subtype {
        if !accepted(subtype) {
            return None;
        }
    }
    let conversation = message.origin.channel.as_ref()?.value().clone();
    let ts = message.origin.ts.value().clone();
    let text = message
        .content
        .as_ref()
        .and_then(|content| content.text.as_deref())
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_string);
    let attachments = files_of(message.content.as_ref());
    if text.is_none() && attachments.is_empty() {
        return None;
    }
    Some(event(
        gateway,
        platform,
        Message {
            conversation,
            id: ts.clone(),
            from: sender_of(message.sender.user.as_ref()),
            text,
            attachments,
            reply: reply_of(message.origin.thread_ts.as_ref(), &ts),
        },
    ))
}

/// An `app_mention` event: the bot named in a channel. The text keeps the
/// mention markup; a trigger script's own prefix matching decides what to do
/// with it.
fn mention_event(
    gateway: &str,
    platform: &str,
    self_id: Option<&str>,
    mention: &SlackAppMentionEvent,
) -> Option<ImEvent> {
    if same_user(self_id, Some(&mention.user)) {
        return None;
    }
    let ts = mention.origin.ts.value().clone();
    let text = mention
        .content
        .text
        .as_deref()
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_string);
    let attachments = files_of(Some(&mention.content));
    if text.is_none() && attachments.is_empty() {
        return None;
    }
    Some(event(
        gateway,
        platform,
        Message {
            conversation: mention.channel.value().clone(),
            id: ts.clone(),
            from: sender_of(Some(&mention.user)),
            text,
            attachments,
            reply: reply_of(mention.origin.thread_ts.as_ref(), &ts),
        },
    ))
}

/// An event type the SDK does not model: read the same fields off its JSON.
fn raw_event(
    gateway: &str,
    platform: &str,
    self_id: Option<&str>,
    raw: &serde_json::Value,
) -> Option<ImEvent> {
    let kind = raw.get("type").and_then(serde_json::Value::as_str)?;
    if kind != "message" && kind != "app_mention" {
        return None;
    }
    // A shape the models refused, which is the same set of facts the model
    // path reads: bot posts, an unanswerable subtype, an empty body.
    if raw.get("bot_id").is_some_and(|value| !value.is_null()) {
        return None;
    }
    if let Some(subtype) = raw.get("subtype").and_then(serde_json::Value::as_str) {
        if !ACCEPTED_SUBTYPES.contains(&subtype) {
            return None;
        }
    }
    let user = raw.get("user").and_then(serde_json::Value::as_str);
    if self_id.is_some_and(|self_id| Some(self_id) == user) {
        return None;
    }
    let conversation = field(raw, "channel")?;
    let ts = field(raw, "ts")?;
    let text = field(raw, "text");
    let attachments = raw_files(raw.get("files"));
    if text.is_none() && attachments.is_empty() {
        return None;
    }
    let thread_ts = field(raw, "thread_ts").filter(|thread_ts| thread_ts != &ts);
    Some(event(
        gateway,
        platform,
        Message {
            conversation,
            id: ts.clone(),
            from: user.map(|user| ImSender {
                id: user.to_string(),
                name: None,
            }),
            text,
            attachments,
            reply: thread_ts.map(reply_coordinate),
        },
    ))
}

/// The parts of an event Slacks sends differently per event type.
struct Message {
    conversation: String,
    id: String,
    from: Option<ImSender>,
    text: Option<String>,
    attachments: Vec<ImAttachment>,
    reply: Option<ImReply>,
}

fn event(gateway: &str, platform: &str, message: Message) -> ImEvent {
    ImEvent {
        gateway: gateway.to_string(),
        platform: platform.to_string(),
        channel: platform.to_string(),
        conversation: message.conversation,
        kind: ImEventKind::Message,
        id: message.id,
        from: message.from,
        text: message.text,
        attachments: message.attachments,
        reply: message.reply,
        at: now(),
        ..ImEvent::default()
    }
}

/// Slack's threads are a flat list anchored at the message that started them,
/// and `thread_ts` names that anchor — both the list root and the nearest
/// parent Slack reports, because Slack reports nothing else.
fn reply_coordinate(thread_ts: String) -> ImReply {
    ImReply {
        parent: Some(thread_ts.clone()),
        root: Some(thread_ts),
        thread: None,
        quoted_text: None,
        self_handle: None,
    }
}

/// What this message answers: a message whose `thread_ts` is its own `ts` is
/// the thread's first message, not a reply.
fn reply_of(thread_ts: Option<&SlackTs>, ts: &str) -> Option<ImReply> {
    let thread_ts = thread_ts
        .map(|thread_ts| thread_ts.value().as_str())
        .filter(|thread_ts| *thread_ts != ts)?;
    Some(reply_coordinate(thread_ts.to_string()))
}

fn accepted(subtype: &SlackMessageEventType) -> bool {
    match subtype {
        SlackMessageEventType::ThreadBroadcast
        | SlackMessageEventType::FileShare
        | SlackMessageEventType::MeMessage => true,
        SlackMessageEventType::Other(other) => ACCEPTED_SUBTYPES.contains(&other.as_str()),
        _ => false,
    }
}

fn same_user(self_id: Option<&str>, user: Option<&SlackUserId>) -> bool {
    match (self_id, user) {
        (Some(self_id), Some(user)) => self_id == user.value(),
        _ => false,
    }
}

/// The files carried by a message, as Slack file references.
///
/// `url_private` needs `Authorization: Bearer <bot token>` and the
/// `files:read` scope (`docs.slack.dev/reference/objects/file-object#access`).
/// Nothing downstream of this event holds that token — the gateway forwards
/// `path` verbatim to a trigger script — so reporting the URL would hand every
/// reader a link that answers 401. The reference names the file the same way
/// the Telegram backend names its `file_id`; fetching it is an operator
/// decision under a `files:read` grant.
fn files_of(content: Option<&SlackMessageContent>) -> Vec<ImAttachment> {
    let Some(files) = content.and_then(|content| content.files.as_ref()) else {
        return Vec::new();
    };
    let mut seen = HashSet::new();
    let mut attachments = Vec::new();
    for file in files {
        let id = file.id.value().clone();
        if !seen.insert(id.clone()) {
            continue;
        }
        let kind = match file.mimetype.as_ref().map(SlackMimeType::value) {
            Some(mimetype) if mimetype.starts_with("image/") => "image",
            _ => "file",
        };
        attachments.push(ImAttachment {
            kind: kind.to_string(),
            path: format!("slack:file:{id}"),
            alt: file.name.clone().or_else(|| file.title.clone()),
        });
    }
    attachments
}

/// The same references from the raw shape of an unmodelled event.
fn raw_files(files: Option<&serde_json::Value>) -> Vec<ImAttachment> {
    let Some(files) = files.and_then(serde_json::Value::as_array) else {
        return Vec::new();
    };
    let mut seen = HashSet::new();
    let mut attachments = Vec::new();
    for file in files {
        let Some(id) = field(file, "id") else {
            continue;
        };
        if !seen.insert(id.clone()) {
            continue;
        }
        let kind = match field(file, "mimetype") {
            Some(mimetype) if mimetype.starts_with("image/") => "image",
            _ => "file",
        };
        attachments.push(ImAttachment {
            kind: kind.to_string(),
            path: format!("slack:file:{id}"),
            alt: field(file, "name").or_else(|| field(file, "title")),
        });
    }
    attachments
}

fn sender_of(user: Option<&SlackUserId>) -> Option<ImSender> {
    user.map(|user| ImSender {
        id: user.value().clone(),
        name: None,
    })
}

/// One non-empty string field.
fn field(value: &serde_json::Value, name: &str) -> Option<String> {
    value
        .get(name)
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn now() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};

    /// One `event_callback` push, the envelope Socket Mode's `events_api`
    /// frame carries.
    fn push(event: Value) -> SlackPushEventCallback {
        let frame = json!({
            "token": "XXYYZZ",
            "team_id": "T123ABC456",
            "api_app_id": "A123ABC456",
            "event": event,
            "type": "event_callback",
            "event_id": "Ev123ABC456",
            "event_time": 123456789,
            "authorizations": []
        });
        serde_json::from_value(frame).expect("a push event")
    }

    fn mapped(event: Value) -> Option<ImEvent> {
        event_from_push("slack-1", "slack", None, &push(event))
    }

    #[test]
    fn a_plain_message_becomes_a_message_event_keyed_by_ts() {
        let event = mapped(json!({
            "type": "message",
            "channel": "C123ABC456",
            "user": "U123ABC456",
            "text": "Hello world",
            "ts": "1355517523.000005",
            "channel_type": "channel"
        }))
        .expect("a message");
        assert_eq!(event.kind, ImEventKind::Message);
        assert_eq!(event.id, "1355517523.000005");
        assert_eq!(event.conversation, "C123ABC456");
        assert_eq!(event.text.as_deref(), Some("Hello world"));
        assert_eq!(event.from.unwrap().id, "U123ABC456");
        assert!(event.reply.is_none(), "a channel message answers nothing");
    }

    #[test]
    fn an_app_mention_is_a_message_event() {
        let event = mapped(json!({
            "type": "app_mention",
            "user": "U061F7AUR",
            "text": "<@U0LAN0Z89> is it everything a river should be?",
            "ts": "1515449522.000016",
            "channel": "C123ABC456",
            "event_ts": "1515449522000016"
        }))
        .expect("a mention");
        assert_eq!(event.id, "1515449522.000016");
        assert!(event.text.unwrap().contains("everything a river"));
    }

    #[test]
    fn a_thread_reply_names_its_thread_as_both_parent_and_root() {
        let event = mapped(json!({
            "type": "message",
            "channel": "C1",
            "user": "U1",
            "text": "and another thing",
            "ts": "1355517536.000001",
            "thread_ts": "1355517523.000005"
        }))
        .expect("a reply");
        let reply = event.reply.expect("a thread reply carries its coordinate");
        assert_eq!(reply.parent.as_deref(), Some("1355517523.000005"));
        assert_eq!(reply.root.as_deref(), Some("1355517523.000005"));
        assert!(reply.thread.is_none(), "Slack has no second thread id");
        assert_eq!(event.id, "1355517536.000001", "the reply's own ts");
    }

    #[test]
    fn a_thread_root_names_no_reply_even_when_it_carries_thread_ts() {
        // A root delivered with replies carries `thread_ts` equal to its own
        // `ts`; reading that as a reply would make every thread reply to
        // itself.
        let event = mapped(json!({
            "type": "message",
            "channel": "C1",
            "user": "U1",
            "text": "the original words",
            "ts": "1355517523.000005",
            "thread_ts": "1355517523.000005"
        }))
        .expect("a root");
        assert!(event.reply.is_none());
    }

    #[test]
    fn a_bot_post_is_ignored() {
        // This bot's own sends come back as events; answering them would make
        // the backend talk to itself.
        assert!(
            mapped(json!({
                "type": "message",
                "subtype": "bot_message",
                "bot_id": "B1",
                "channel": "C1",
                "text": "posted by the bot",
                "ts": "1.1"
            }))
            .is_none()
        );
        assert!(
            mapped(json!({
                "type": "message",
                "bot_id": "B1",
                "channel": "C1",
                "text": "posted by an app",
                "ts": "1.2"
            }))
            .is_none()
        );
    }

    #[test]
    fn this_bots_own_post_is_not_answered_even_without_a_bot_id() {
        let frame = json!({
            "type": "message",
            "channel": "C1",
            "user": "U-bot",
            "text": "my own words",
            "ts": "1.3"
        });
        let push = push(frame);
        assert!(event_from_push("gw", "slack", Some("U-bot"), &push).is_none());
        assert!(event_from_push("gw", "slack", Some("U-other"), &push).is_some());
    }

    #[test]
    fn an_edit_or_delete_notice_is_not_a_message() {
        for subtype in ["message_changed", "message_deleted", "channel_join"] {
            assert!(
                mapped(json!({
                    "type": "message",
                    "subtype": subtype,
                    "hidden": true,
                    "channel": "C1",
                    "ts": "1358878755.000001"
                }))
                .is_none(),
                "subtype {subtype}"
            );
        }
        // The documentation's edited-message example: an edit arrives as a
        // `message` event with an `edited` ledger, not as a new message.
        assert!(
            mapped(json!({
                "type": "message",
                "channel": "C1",
                "user": "U1",
                "text": "Hello, world!",
                "ts": "1355517523.000005",
                "edited": {"user": "U123ABC456", "ts": "1355517536.000001"}
            }))
            .is_some()
        );
    }

    #[test]
    fn a_shared_file_carries_a_reference_and_its_kind() {
        let event = mapped(json!({
            "type": "message",
            "subtype": "file_share",
            "channel": "D0L4B9P0Q",
            "user": "U123ABC456",
            "ts": "1529342088.000086",
            "channel_type": "im",
            "files": [{
                "id": "F0RDC39U1",
                "name": "ghostrap.png",
                "mimetype": "image/png",
                "url_private": "https://files.slack.com/files-pri/T061EG9R6-F0RDC39U1/ghostrap.png"
            }, {
                "id": "F1",
                "name": "notes.txt",
                "mimetype": "text/plain",
                "url_private": "https://files.slack.com/files-pri/T061EG9R6-F1/notes.txt"
            }]
        }))
        .expect("a file share");
        assert_eq!(event.attachments.len(), 2);
        assert_eq!(event.attachments[0].kind, "image");
        assert_eq!(event.attachments[0].alt.as_deref(), Some("ghostrap.png"));
        // A bearer-only URL would answer 401 for every reader downstream.
        assert_eq!(event.attachments[0].path, "slack:file:F0RDC39U1");
        assert_eq!(event.attachments[1].kind, "file");
        assert_eq!(event.attachments[1].path, "slack:file:F1");
        assert_eq!(event.conversation, "D0L4B9P0Q");
    }

    #[test]
    fn a_message_with_nothing_to_read_is_not_an_event() {
        assert!(
            mapped(json!({
                "type": "message",
                "channel": "C1",
                "user": "U1",
                "text": "",
                "ts": "1.1"
            }))
            .is_none()
        );
        // No ts: nothing can key the record or thread a reply under it.
        assert!(
            mapped(json!({
                "type": "message",
                "channel": "C1",
                "user": "U1",
                "text": "hello"
            }))
            .is_none()
        );
    }

    #[test]
    fn an_unmodelled_event_type_is_read_from_its_json() {
        // `SlackEventCallbackBody::Unknown` is the SDK's catch-all, and it is
        // where a message shape the models do not expect lands.
        let push = push(json!({
            "type": "message",
            "channel": "C1",
            "user": "U1",
            "text": "from an unknown shape",
            "ts": "2.2"
        }));
        let SlackEventCallbackBody::Unknown(_) = &push.event else {
            // The SDK models this one; both paths produce the same event.
            assert!(event_from_push("gw", "slack", None, &push).is_some());
            return;
        };
        let event = event_from_push("gw", "slack", None, &push).expect("a raw message");
        assert_eq!(event.id, "2.2");
        assert_eq!(event.conversation, "C1");
        assert_eq!(event.text.as_deref(), Some("from an unknown shape"));
    }
}
