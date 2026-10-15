//! The presentation layer: turn a standard payload into QQ message bodies.
//!
//! This is the only place QQ's own request shapes appear. The core hands over
//! `text`, `format`, `attachments`, and `reply_to`, and this module decides
//! what the QQ Open Platform receives — one text message per scene's send
//! endpoint (`docs/v2.2-IM-GATEWAY-PLAN.md` §5, the same split the Telegram
//! backend keeps).
//!
//! QQ's text message is `{"msg_type": 0, "content": ...}` for every scene.
//! Group and C2C sends also carry `msg_seq`; channel and direct sends do not
//! (v1 removed the field there, `outbound_message_request_json`). `msg_id`
//! carries the message this payload answers — QQ requires it for a proactive
//! or group message to be accepted at all, and it is the dedupe key the
//! platform uses to reject a double send.

use crate::api::Scene;
use onlyne_im_backend::payload::ImPayload;
use serde_json::{Value, json};

/// The largest `msg_seq` QQ accepts before it must wrap.
///
/// QQ documents `msg_seq` as a `uint32` that must grow within one `msg_id`
/// and rejects a repeat; the bound here is well under the platform's own so
/// the wrap never collides with a value QQ has already seen in one session.
const SEQ_MAX: u64 = 10_000;

/// One starting sequence per backend process.
///
/// v1 restarted this at 1 on every process restart, and a process that
/// restarted mid-conversation re-sent `msg_seq = 1` for a `msg_id` QQ had
/// already accepted at that sequence — the platform replied
/// `sequence repeat` and the message was dropped. The sequence therefore
/// starts from the second the process started, so two processes — or two
/// runs of one — never open on the same value: the collision window is one
/// birthday-paradox span of ten thousand sends instead of a guaranteed
/// first-send clash.
fn starting_sequence() -> u64 {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos() as u64)
        .unwrap_or(0);
    nanos % SEQ_MAX + 1
}

/// The running `msg_seq` counter: one per backend, wrapping under [`SEQ_MAX`].
pub struct Sequence {
    next: u64,
}

impl Sequence {
    pub fn new() -> Self {
        Self {
            next: starting_sequence(),
        }
    }

    /// The next sequence value, wrapping back to 1 past the bound.
    pub fn next(&mut self) -> u64 {
        self.next = self.next % SEQ_MAX + 1;
        self.next
    }
}

impl Default for Sequence {
    fn default() -> Self {
        Self::new()
    }
}

/// Render one payload into the QQ message body for one conversation's scene.
///
/// The text rides as `msg_type 0`. Markdown degrades to plain text: QQ's
/// rich-message types (`msg_type 2`, markdown templates) need an approved
/// template id this backend does not carry, so sending the raw markdown text
/// is the honest degradation, the same call the Telegram backend's
/// non-MarkdownV2 path makes.
pub fn render(conversation: &str, payload: &ImPayload, msg_seq: u64) -> Result<Value, String> {
    let (_, scene) = Scene::parse(conversation).map_err(|error| error.to_string())?;
    let text = payload.text.trim();
    if text.is_empty() {
        return Err("the payload has no text to send".to_string());
    }
    let mut body = json!({
        "msg_type": 0,
        "content": text,
    });
    if matches!(scene, Scene::Group | Scene::C2c) {
        // `msg_seq` must be at least 1 for the platform to accept the send.
        body["msg_seq"] = json!(msg_seq.max(1));
    }
    if let Some(reply_to) = payload
        .reply_to
        .as_deref()
        .filter(|value| !value.trim().is_empty())
    {
        // `msg_id` is what makes the send a *passive* reply — accepted without
        // the proactive-message grant.
        body["msg_id"] = json!(reply_to);
    }
    if let Some(handle) = payload
        .reply_reference
        .as_deref()
        .filter(|value| !value.trim().is_empty())
    {
        // `message_reference` is what renders it as a *quote*. Two different
        // fields on purpose: QQ puts the message id in one and the message's
        // quote handle in the other, and setting only the first is why a reply
        // that worked still showed the group no quote.
        body["message_reference"] = json!({"message_id": handle});
    }
    Ok(body)
}

#[cfg(test)]
mod tests {
    use super::*;
    use onlyne_im_backend::payload::ImFormat;
    use onlyne_im_backend::payload::ImPayload;

    fn payload(text: &str) -> ImPayload {
        ImPayload {
            text: text.to_string(),
            ..ImPayload::default()
        }
    }

    #[test]
    fn a_group_payload_carries_seq_and_reply() {
        let mut payload = payload("hello");
        payload.reply_to = Some("m0".to_string());
        let body = render("group:g1", &payload, 3).unwrap();
        assert_eq!(body["msg_type"], 0);
        assert_eq!(body["content"], "hello");
        assert_eq!(body["msg_seq"], 3);
        assert_eq!(body["msg_id"], "m0");
    }

    #[test]
    fn a_c2c_payload_is_the_same_shape() {
        let body = render("c2c:u1", &payload("hello"), 1).unwrap();
        assert_eq!(body["msg_type"], 0);
        assert_eq!(body["content"], "hello");
        assert_eq!(body["msg_seq"], 1);
    }

    #[test]
    fn channel_and_direct_sends_omit_msg_seq() {
        for conversation in ["channel:c1", "direct:d1"] {
            let body = render(conversation, &payload("hello"), 5).unwrap();
            assert_eq!(body["msg_type"], 0);
            assert!(body.get("msg_seq").is_none(), "{conversation}");
        }
    }

    #[test]
    fn a_bare_conversation_defaults_to_the_channel_scene() {
        let body = render("c1", &payload("hello"), 1).unwrap();
        assert!(body.get("msg_seq").is_none());
    }

    #[test]
    fn a_sequence_wraps_and_never_stays_below_one() {
        let mut sequence = Sequence::new();
        let mut last = 0;
        for _ in 0..SEQ_MAX + 3 {
            let value = sequence.next();
            assert!((1..=SEQ_MAX).contains(&value));
            assert_ne!(value, last, "a wrap must not repeat the previous value");
            last = value;
        }
    }

    #[test]
    fn an_empty_payload_is_refused() {
        let error = render("group:g1", &payload("   "), 1).unwrap_err();
        assert!(error.contains("no text"), "{error}");
    }

    #[test]
    fn an_empty_conversation_is_refused() {
        assert!(render("", &payload("hello"), 1).is_err());
        assert!(render("group:", &payload("hello"), 1).is_err());
    }

    #[test]
    fn a_reply_to_that_is_only_whitespace_is_omitted() {
        let mut payload = payload("hello");
        payload.reply_to = Some("   ".to_string());
        let body = render("group:g1", &payload, 1).unwrap();
        assert!(body.get("msg_id").is_none());
    }

    #[test]
    fn markdown_degrades_to_plain_text() {
        let payload = ImPayload {
            text: "**bold**".to_string(),
            format: ImFormat::Markdown,
            ..ImPayload::default()
        };
        let body = render("group:g1", &payload, 1).unwrap();
        assert_eq!(body["content"], "**bold**");
        assert_eq!(body["msg_type"], 0);
    }

    #[test]
    fn a_reply_carries_both_ids_the_platform_splits_the_link_across() {
        // `msg_id` makes the send passive; `message_reference` renders it as a
        // quote. Setting only the first is why a reply that the platform
        // accepted still showed the group no quote — measured 2026-10-15.
        let mut payload = payload("hello");
        payload.reply_to = Some("ROBOT1.0_AdAXNfwe".to_string());
        payload.reply_reference = Some("REFIDX_4y7F23NM".to_string());
        let body = render("group:g1", &payload, 1).unwrap();
        assert_eq!(body["msg_id"], "ROBOT1.0_AdAXNfwe");
        assert_eq!(body["message_reference"]["message_id"], "REFIDX_4y7F23NM");
    }

    #[test]
    fn a_reply_without_a_handle_quotes_nothing_but_stays_passive() {
        let mut payload = payload("hello");
        payload.reply_to = Some("ROBOT1.0_AdAXNfwe".to_string());
        let body = render("group:g1", &payload, 1).unwrap();
        assert_eq!(body["msg_id"], "ROBOT1.0_AdAXNfwe");
        assert!(body.get("message_reference").is_none(), "{body}");
    }
}
