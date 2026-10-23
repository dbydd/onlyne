//! The inbound half: turn one Linear comment node into a standard [`ImEvent`].
//!
//! Only what the contract needs is read — the comment id (the dedupe key), the
//! issue id (the conversation), the author, the body, and a parent comment when
//! the comment is one. Everything else Linear sends rides past unread.
//!
//! Linear is a *chain* platform: a comment may name the comment it answers
//! (`parent`), and there is no list id above the issue. So the event reports
//! `reply.parent` and leaves `root` and `thread` absent, which is what
//! `threadlink::shape_of` reads as `Chain`.

use onlyne_im_backend::payload::{ImEvent, ImEventKind, ImReply, ImSender};
use serde_json::Value;

/// Map one comment node into the event a trigger script receives.
///
/// `gateway` and `platform` are the backend's own coordinates, filled from its
/// configuration; the rest comes out of the node. A comment with an empty body,
/// no issue, or no id yields nothing — there is no conversation to route it to
/// and nothing to dedupe it by.
pub fn event_from_comment(gateway: &str, platform: &str, comment: &Value) -> Option<ImEvent> {
    let id = comment.get("id").and_then(Value::as_str)?;
    let conversation = comment.pointer("/issue/id").and_then(Value::as_str)?;
    let text = comment.get("body").and_then(Value::as_str)?.trim();
    if id.is_empty() || conversation.is_empty() || text.is_empty() {
        return None;
    }
    Some(ImEvent {
        gateway: gateway.to_string(),
        platform: platform.to_string(),
        channel: platform.to_string(),
        conversation: conversation.to_string(),
        kind: ImEventKind::Message,
        id: id.to_string(),
        from: sender_of(comment),
        text: Some(text.to_string()),
        reply: reply_of(comment),
        at: updated_at(comment)
            .map(str::to_string)
            .or_else(|| created_at(comment).map(str::to_string))
            .unwrap_or_else(now),
        ..ImEvent::default()
    })
}

/// The comment's `updatedAt`, the poll cursor's ordering key.
pub fn updated_at(comment: &Value) -> Option<&str> {
    comment.get("updatedAt").and_then(Value::as_str)
}

fn created_at(comment: &Value) -> Option<&str> {
    comment.get("createdAt").and_then(Value::as_str)
}

/// Who wrote the comment.
///
/// A Linear comment names a workspace `user`; a comment posted through an
/// external integration can instead name an `externalUser`. Both carry an id
/// and a name, and reporting the one that is present keeps a sender a sender.
fn sender_of(comment: &Value) -> Option<ImSender> {
    ["user", "externalUser"]
        .iter()
        .filter_map(|field| comment.get(*field))
        .find(|value| !value.is_null())
        .and_then(|value| {
            let id = value.get("id").and_then(Value::as_str)?;
            Some(ImSender {
                id: id.to_string(),
                name: value
                    .get("name")
                    .and_then(Value::as_str)
                    .filter(|name| !name.trim().is_empty())
                    .map(str::to_string),
            })
        })
}

/// What this comment answers.
///
/// A Linear comment nests under another comment through `parent`, so that id
/// is the only coordinate reported; `root` and `thread` stay absent because
/// the platform has no separate list to name. A top-level comment carries no
/// reply at all.
fn reply_of(comment: &Value) -> Option<ImReply> {
    let parent = comment
        .pointer("/parent/id")
        .and_then(Value::as_str)
        .filter(|id| !id.trim().is_empty())?;
    Some(ImReply {
        parent: Some(parent.to_string()),
        ..ImReply::default()
    })
}

fn now() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_comment_becomes_a_message_event() {
        let comment = json!({
            "id": "comment-1",
            "body": "deploy please",
            "createdAt": "2026-10-17T09:00:00.000Z",
            "updatedAt": "2026-10-17T09:30:00.000Z",
            "issue": {"id": "issue-1", "identifier": "ENG-1", "title": "Ship it"},
            "user": {"id": "user-1", "name": "Ada"},
            "parent": null
        });
        let event = event_from_comment("linear1", "linear", &comment).unwrap();
        assert_eq!(event.kind, ImEventKind::Message);
        assert_eq!(event.id, "comment-1");
        assert_eq!(event.conversation, "issue-1");
        assert_eq!(event.text.as_deref(), Some("deploy please"));
        assert_eq!(event.at, "2026-10-17T09:30:00.000Z");
        let sender = event.from.unwrap();
        assert_eq!(sender.id, "user-1");
        assert_eq!(sender.name.as_deref(), Some("Ada"));
        assert!(event.reply.is_none(), "a top-level comment answers nothing");
    }

    /// The mapping's variants, one row each: which author field wins, whether
    /// a parent is reported, and which nodes are not events at all.
    #[test]
    fn comment_variants_map_as_the_contract_reads_them() {
        struct Case {
            name: &'static str,
            comment: Value,
            event: bool,
            sender: Option<&'static str>,
            parent: Option<&'static str>,
        }
        let cases = [
            Case {
                name: "nested reply",
                comment: json!({
                    "id": "c2", "body": "on it",
                    "updatedAt": "2026-10-17T10:00:00.000Z",
                    "issue": {"id": "i1"}, "user": {"id": "u1", "name": "Ada"},
                    "parent": {"id": "c1"}
                }),
                event: true,
                sender: Some("u1"),
                parent: Some("c1"),
            },
            Case {
                name: "external user",
                comment: json!({
                    "id": "c3", "body": "from slack",
                    "updatedAt": "2026-10-17T10:00:00.000Z",
                    "issue": {"id": "i1"},
                    "user": null,
                    "externalUser": {"id": "ext1", "name": "Slack Upsert"}
                }),
                event: true,
                sender: Some("ext1"),
                parent: None,
            },
            Case {
                name: "no author",
                comment: json!({
                    "id": "c4", "body": "anon",
                    "updatedAt": "2026-10-17T10:00:00.000Z", "issue": {"id": "i1"}
                }),
                event: true,
                sender: None,
                parent: None,
            },
            Case {
                name: "empty body",
                comment: json!({
                    "id": "c5", "body": "   ",
                    "updatedAt": "2026-10-17T10:00:00.000Z", "issue": {"id": "i1"}
                }),
                event: false,
                sender: None,
                parent: None,
            },
            Case {
                name: "no issue",
                comment: json!({
                    "id": "c6", "body": "orphan",
                    "updatedAt": "2026-10-17T10:00:00.000Z"
                }),
                event: false,
                sender: None,
                parent: None,
            },
            Case {
                name: "no id",
                comment: json!({
                    "body": "nameless", "updatedAt": "2026-10-17T10:00:00.000Z",
                    "issue": {"id": "i1"}
                }),
                event: false,
                sender: None,
                parent: None,
            },
        ];
        for case in cases {
            let event = event_from_comment("g", "linear", &case.comment);
            assert_eq!(event.is_some(), case.event, "{}", case.name);
            let Some(event) = event else { continue };
            assert_eq!(
                event.from.as_ref().map(|sender| sender.id.as_str()),
                case.sender,
                "{}",
                case.name
            );
            assert_eq!(
                event
                    .reply
                    .as_ref()
                    .and_then(|reply| reply.parent.as_deref()),
                case.parent,
                "{}",
                case.name
            );
            assert!(
                event
                    .reply
                    .as_ref()
                    .is_none_or(|reply| reply.root.is_none() && reply.thread.is_none()),
                "{}: Linear names no list or thread",
                case.name
            );
        }
    }
}
