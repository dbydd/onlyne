//! The presentation layer: turn a standard payload into Telegram API calls.
//!
//! This is the only place Telegram's own shapes appear. The core never sees
//! `parse_mode`, `reply_markup`, or an inline keyboard — it hands over `text`,
//! `format`, `attachments`, and `actions`, and this module decides what the Bot
//! API receives (`docs/v2.2-IM-GATEWAY-PLAN.md` §5).

use onlyne_im_backend::payload::{ImAction, ImAttachment, ImFormat, ImPayload};
use serde_json::{Value, json};

/// One Bot API call a payload becomes.
#[derive(Debug, Clone, PartialEq)]
pub enum Call {
    /// `sendMessage`.
    Message {
        chat_id: String,
        text: String,
        parse_mode: Option<&'static str>,
        reply_markup: Option<Value>,
    },
    /// `sendPhoto` with local file bytes.
    Photo {
        chat_id: String,
        path: String,
        caption: Option<String>,
    },
    /// `sendDocument` with local file bytes.
    Document {
        chat_id: String,
        path: String,
        caption: Option<String>,
    },
}

/// Render one payload into the ordered Bot API calls that show it.
///
/// A payload with text and no attachment is one `sendMessage`. A payload with
/// images is one `sendPhoto` per image, the first carrying the text as its
/// caption. Buttons ride the first call that carries text, as an inline
/// keyboard of one button per row.
pub fn render(chat_id: &str, payload: &ImPayload) -> Vec<Call> {
    let parse_mode = match payload.format {
        ImFormat::Plain => None,
        // Telegram's MarkdownV2 needs every reserved character escaped; the
        // backend sends what it is given and lets the sender's formatting
        // stand, so a payload that is already Telegram-shaped passes through.
        ImFormat::Markdown => Some("MarkdownV2"),
    };
    let keyboard = keyboard(&payload.actions);
    let mut calls = Vec::new();
    let images: Vec<&ImAttachment> = payload
        .attachments
        .iter()
        .filter(|a| a.kind == "image")
        .collect();
    let files: Vec<&ImAttachment> = payload
        .attachments
        .iter()
        .filter(|a| a.kind != "image")
        .collect();

    // The text rides alone when there is no image, else as the first image's
    // caption. A payload with neither is not renderable and yields nothing.
    if images.is_empty() && !payload.text.trim().is_empty() {
        calls.push(Call::Message {
            chat_id: chat_id.to_string(),
            text: payload.text.clone(),
            parse_mode,
            reply_markup: keyboard.clone(),
        });
    }
    for (index, image) in images.iter().enumerate() {
        calls.push(Call::Photo {
            chat_id: chat_id.to_string(),
            path: image.path.clone(),
            caption: (index == 0 && !payload.text.trim().is_empty()).then(|| payload.text.clone()),
        });
    }
    for file in &files {
        calls.push(Call::Document {
            chat_id: chat_id.to_string(),
            path: file.path.clone(),
            caption: file.alt.clone(),
        });
    }
    calls
}

/// An inline keyboard: one button per row, `callback_data` the action's id.
fn keyboard(actions: &[ImAction]) -> Option<Value> {
    if actions.is_empty() {
        return None;
    }
    let rows: Vec<Value> = actions
        .iter()
        .map(|action| json!([{ "text": action.label, "callback_data": action.id }]))
        .collect();
    Some(json!({ "inline_keyboard": rows }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn payload(text: &str) -> ImPayload {
        ImPayload {
            text: text.to_string(),
            ..ImPayload::default()
        }
    }

    #[test]
    fn text_alone_is_one_message() {
        let calls = render("123", &payload("hello"));
        assert_eq!(
            calls,
            vec![Call::Message {
                chat_id: "123".into(),
                text: "hello".into(),
                parse_mode: None,
                reply_markup: None,
            }]
        );
    }

    #[test]
    fn markdown_sets_the_parse_mode() {
        let mut p = payload("a *word*");
        p.format = ImFormat::Markdown;
        match &render("1", &p)[0] {
            Call::Message { parse_mode, .. } => assert_eq!(*parse_mode, Some("MarkdownV2")),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn actions_become_an_inline_keyboard_of_one_button_per_row() {
        let mut p = payload("approve?");
        p.actions = vec![
            ImAction {
                id: "yes".into(),
                label: "Allow".into(),
                style: Some("primary".into()),
            },
            ImAction {
                id: "no".into(),
                label: "Deny".into(),
                style: None,
            },
        ];
        let Call::Message { reply_markup, .. } = &render("1", &p)[0] else {
            panic!("expected a message");
        };
        let markup = reply_markup.as_ref().unwrap();
        assert_eq!(markup["inline_keyboard"][0][0]["text"], "Allow");
        assert_eq!(markup["inline_keyboard"][0][0]["callback_data"], "yes");
        assert_eq!(markup["inline_keyboard"][1][0]["callback_data"], "no");
    }

    #[test]
    fn an_image_carries_the_text_as_its_caption_and_a_file_does_not() {
        let mut p = payload("look");
        p.attachments = vec![ImAttachment {
            kind: "image".into(),
            path: "/tmp/a.png".into(),
            alt: Some("a picture".into()),
        }];
        let calls = render("9", &p);
        assert_eq!(calls.len(), 1);
        match &calls[0] {
            Call::Photo { caption, path, .. } => {
                assert_eq!(caption.as_deref(), Some("look"));
                assert_eq!(path, "/tmp/a.png");
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_file_rides_its_own_send_after_the_text() {
        let mut p = payload("here");
        p.attachments = vec![ImAttachment {
            kind: "file".into(),
            path: "/tmp/report.pdf".into(),
            alt: Some("report".into()),
        }];
        let calls = render("9", &p);
        assert_eq!(calls.len(), 2);
        assert!(matches!(&calls[0], Call::Message { .. }));
        match &calls[1] {
            Call::Document { caption, .. } => assert_eq!(caption.as_deref(), Some("report")),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn an_empty_payload_renders_nothing() {
        assert!(render("1", &payload("   ")).is_empty());
    }
}
