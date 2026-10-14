//! The conversation's own chat log: `chat.jsonl`, one JSON object per line.
//!
//! An IM conversation is worth reading after the fact — what a human asked, what
//! the cluster answered — and the ledger does not hold the platform's side: it
//! holds deliveries, not the rendered text or the inbound event a script was
//! handed. So the gateway appends what it saw and what it sent into the
//! conversation's workspace, where the operator's scripts can tail it and a
//! person can open it.
//!
//! The shape is deliberately the same one the trigger script's stdin carries
//! (`docs/v2.2-IM-GATEWAY-PLAN.md` §4.3) for an inbound event, and the standard
//! payload's for an outbound one, each with `direction` added. A line is one
//! `write_all` of a serialized object plus a newline; the log is an
//! convenience, not the delivery, so a failed append is logged and never costs
//! a message.

use crate::config::Conversation;
use anyhow::Result;
use onlyne_im_backend::payload::{ImEvent, ImPayload};
use serde_json::json;
use std::fs::OpenOptions;
use std::io::Write;

/// Append one inbound event to the conversation's log.
pub fn append_inbound(entry: &Conversation, event: &ImEvent) -> Result<()> {
    let line = json!({
        "direction": "in",
        "chat": entry.chat_key,
        "gateway": event.gateway,
        "platform": event.platform,
        "conversation": event.conversation,
        "kind": event.kind,
        "id": event.id,
        "from": event.from,
        "text": event.text,
        "attachments": event.attachments,
        "action": event.action,
        "at": event.at,
    });
    append(entry, &line)
}

/// Append one outbound payload to the conversation's log.
pub fn append_outbound(entry: &Conversation, payload: &ImPayload) -> Result<()> {
    let line = json!({
        "direction": "out",
        "chat": entry.chat_key,
        "gateway": entry.gateway,
        "conversation": entry.conversation,
        "payload": payload,
        "at": now(),
    });
    append(entry, &line)
}

/// Append one line. The file is created on first write; the media directory and
/// the log share the conversation workspace, which `generate` created, so no
/// directory is made here beyond the workspace's own.
fn append(entry: &Conversation, line: &serde_json::Value) -> Result<()> {
    let path = entry.chat_log();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut file = OpenOptions::new().create(true).append(true).open(path)?;
    let mut bytes = serde_json::to_vec(line)?;
    bytes.push(b'\n');
    file.write_all(&bytes)?;
    file.flush()?;
    Ok(())
}

pub(crate) fn now() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

/// The id of the last inbound event this conversation logged, if any.
///
/// QQ's passive reply is only passive when the send carries the message it
/// answers (`msg_id`), so a reply must be able to name it; the event id is the
/// one id the platform gave, and it is already in the log. Reading it back is a
/// line scan, which is nothing next to the send it enables.
pub fn last_inbound_id(entry: &Conversation) -> Option<String> {
    let path = entry.chat_log();
    let text = std::fs::read_to_string(path).ok()?;
    // Read backwards: the answer is usually the last line, and `lines` is
    // double-ended, so the common case parses one line rather than the log.
    let id = text
        .lines()
        .rev()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .find(|line| line.get("direction").and_then(|d| d.as_str()) == Some("in"))
        .and_then(|line| line.get("id").and_then(|id| id.as_str()).map(str::to_owned))?;
    (!id.trim().is_empty()).then_some(id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use onlyne_im_backend::payload::{ImAttachment, ImEventKind};

    fn conversation(dir: &std::path::Path) -> Conversation {
        Conversation {
            slug: "tg-group-1".into(),
            workspace: dir.to_path_buf(),
            config_path: dir.join(".onlyne/config.toml"),
            gateway: "tg1".into(),
            conversation: "-100".into(),
            chat_key: "abc123".into(),
        }
    }

    #[test]
    fn an_inbound_and_an_outbound_line_land_in_the_conversation_workspace() {
        let dir = tempfile::tempdir().unwrap();
        let entry = conversation(dir.path());
        let event = ImEvent {
            gateway: "tg1".into(),
            platform: "telegram".into(),
            channel: "telegram".into(),
            conversation: "-100".into(),
            kind: ImEventKind::Message,
            id: "msg-1".into(),
            text: Some("hi".into()),
            at: "2026-10-11T00:00:00.000Z".into(),
            ..ImEvent::default()
        };
        append_inbound(&entry, &event).unwrap();
        let payload = ImPayload {
            text: "hello".into(),
            attachments: vec![ImAttachment {
                kind: "image".into(),
                path: "/abs/media/out-1.png".into(),
                alt: None,
            }],
            ..ImPayload::default()
        };
        append_outbound(&entry, &payload).unwrap();

        let log = entry.chat_log();
        assert!(log.starts_with(&entry.workspace));
        let text = std::fs::read_to_string(&log).unwrap();
        let lines: Vec<serde_json::Value> = text
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0]["direction"], "in");
        assert_eq!(lines[0]["chat"], "abc123");
        assert_eq!(lines[0]["id"], "msg-1");
        assert_eq!(lines[1]["direction"], "out");
        assert_eq!(lines[1]["chat"], "abc123");
        assert_eq!(lines[1]["payload"]["text"], "hello");
    }
}
