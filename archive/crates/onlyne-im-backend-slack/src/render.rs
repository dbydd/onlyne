//! The presentation layer: turn a standard payload into Slack Web API calls.
//!
//! This is the only place Slack's own request shapes appear. The core hands
//! over `text`, `format`, `attachments`, `actions`, and `reply_to`, and this
//! module decides what `chat.postMessage` and the file upload calls receive
//! (`docs/v2.2-IM-GATEWAY-PLAN.md` §5).
//!
//! Three rules decide the shape:
//!
//! * `reply_to` is a message ts, which is exactly what `thread_ts` takes, so a
//!   reply is one field rather than a chain this backend rebuilds.
//! * A URL attachment becomes a link in the text; Slack cannot fetch a URL as
//!   an upload, and dropping it silently is worse than a visible link.
//! * A local file becomes a real upload. Slack's upload endpoints are the only
//!   way bytes on this host become a file a channel can see.

use onlyne_im_backend::payload::{ImAttachment, ImFormat, ImPayload};

/// One Web API call a payload becomes.
#[derive(Debug, Clone, PartialEq)]
pub enum Call {
    /// `chat.postMessage`.
    Post {
        channel: String,
        text: String,
        thread_ts: Option<String>,
        /// Whether Slack may parse its mrkdwn markup in `text`.
        mrkdwn: bool,
    },
    /// The external upload flow: `files.getUploadURLExternal`, the byte POST,
    /// and `files.completeUploadExternal`.
    Upload {
        channel: String,
        path: String,
        thread_ts: Option<String>,
    },
}

/// The anchor a payload with files but no words posts.
///
/// `files.completeUploadExternal` answers file objects, not a message, so an
/// upload-only payload would send with no `ts` and a later reply would have
/// nothing to thread under. A one-line text post gives the file share a real
/// message id; it is Slack's own documented shape for "a file with a comment".
pub const ATTACHMENT_ANCHOR: &str = "Shared an attachment.";

/// Render one payload into the ordered calls that show it.
///
/// The text posts first, so the answer's `ts` — the id a later reply threads
/// under — always names a message. Files follow as uploads threaded under that
/// post. A payload with nothing at all yields no calls; the caller refuses it.
pub fn render(channel: &str, payload: &ImPayload) -> Vec<Call> {
    let thread_ts = payload
        .reply_to
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);
    let mrkdwn = matches!(payload.format, ImFormat::Markdown);
    let mut text = payload.text.clone();
    let mut uploads: Vec<&ImAttachment> = Vec::new();
    for attachment in &payload.attachments {
        if is_url(&attachment.path) {
            // A URL cannot be uploaded; a link in the message keeps it, and
            // the alt text keeps what it was called.
            if text.trim().is_empty() {
                // Whitespace-only text is nothing to say; leaving it would put
                // a blank first line above the link.
                text.clear();
            } else {
                text.push('\n');
            }
            match &attachment.alt {
                Some(alt) if !alt.trim().is_empty() => {
                    text.push_str(&format!("{alt}: {}", attachment.path));
                }
                _ => text.push_str(&attachment.path),
            }
        } else {
            uploads.push(attachment);
        }
    }

    let mut calls = Vec::new();
    if text.trim().is_empty() && !uploads.is_empty() {
        text = ATTACHMENT_ANCHOR.to_string();
    }
    if !text.trim().is_empty() {
        calls.push(Call::Post {
            channel: channel.to_string(),
            text,
            thread_ts: thread_ts.clone(),
            mrkdwn,
        });
    }
    // A file inherits the payload's reply target. When the payload is not a
    // reply, the send path threads it under the post above instead — the
    // anchor's own `ts` is only known after that post returns.
    let upload_thread = thread_ts.clone();
    for attachment in uploads {
        calls.push(Call::Upload {
            channel: channel.to_string(),
            path: attachment.path.clone(),
            thread_ts: upload_thread.clone(),
        });
    }
    calls
}

fn is_url(path: &str) -> bool {
    let path = path.trim().to_ascii_lowercase();
    path.starts_with("http://") || path.starts_with("https://")
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

    fn attachment(kind: &str, path: &str, alt: Option<&str>) -> ImAttachment {
        ImAttachment {
            kind: kind.to_string(),
            path: path.to_string(),
            alt: alt.map(str::to_string),
        }
    }

    #[test]
    fn text_alone_is_one_plain_post() {
        assert_eq!(
            render("C1", &payload("hello")),
            vec![Call::Post {
                channel: "C1".into(),
                text: "hello".into(),
                thread_ts: None,
                mrkdwn: false,
            }]
        );
    }

    #[test]
    fn markdown_leaves_markup_parsing_on_and_plain_turns_it_off() {
        let mut p = payload("a *word*");
        p.format = ImFormat::Markdown;
        let Call::Post { mrkdwn, .. } = &render("C1", &p)[0] else {
            panic!("expected a post");
        };
        assert!(mrkdwn, "markdown is Slack's own default parsing");

        p.format = ImFormat::Plain;
        let Call::Post { mrkdwn, .. } = &render("C1", &p)[0] else {
            panic!("expected a post");
        };
        assert!(!mrkdwn);
    }

    #[test]
    fn a_reply_threads_under_the_message_it_answers() {
        let mut p = payload("and another thing");
        p.reply_to = Some("1355517523.000005".into());
        assert_eq!(
            render("C1", &p),
            vec![Call::Post {
                channel: "C1".into(),
                text: "and another thing".into(),
                thread_ts: Some("1355517523.000005".into()),
                mrkdwn: false,
            }]
        );
    }

    #[test]
    fn a_url_attachment_becomes_a_link_in_the_text() {
        let mut p = payload("look at this");
        p.attachments = vec![attachment(
            "image",
            "https://example.com/a.png",
            Some("a picture"),
        )];
        let calls = render("C1", &p);
        assert_eq!(calls.len(), 1);
        let Call::Post { text, .. } = &calls[0] else {
            panic!("expected one post");
        };
        assert_eq!(text, "look at this\na picture: https://example.com/a.png");
    }

    #[test]
    fn a_url_attachment_alone_is_the_whole_text() {
        let mut p = payload("   ");
        p.attachments = vec![attachment("file", "https://example.com/report.pdf", None)];
        let calls = render("C1", &p);
        let Call::Post { text, .. } = &calls[0] else {
            panic!("expected one post");
        };
        assert_eq!(text, "https://example.com/report.pdf");
        assert_eq!(calls.len(), 1);
    }

    #[test]
    fn a_local_file_becomes_an_upload_after_the_text() {
        let mut p = payload("here");
        p.attachments = vec![attachment("file", "/tmp/report.pdf", Some("report"))];
        let calls = render("C9", &p);
        assert_eq!(
            calls,
            vec![
                Call::Post {
                    channel: "C9".into(),
                    text: "here".into(),
                    thread_ts: None,
                    mrkdwn: false,
                },
                Call::Upload {
                    channel: "C9".into(),
                    path: "/tmp/report.pdf".into(),
                    thread_ts: None,
                },
            ]
        );
    }

    #[test]
    fn a_file_only_payload_posts_an_anchor_it_can_be_answered_under() {
        // An upload answers file objects, not a message ts; without the anchor
        // a later reply would have nothing to thread under.
        let mut p = payload("");
        p.attachments = vec![attachment("image", "/tmp/a.png", None)];
        assert_eq!(
            render("C1", &p),
            vec![
                Call::Post {
                    channel: "C1".into(),
                    text: ATTACHMENT_ANCHOR.into(),
                    thread_ts: None,
                    mrkdwn: false,
                },
                Call::Upload {
                    channel: "C1".into(),
                    path: "/tmp/a.png".into(),
                    thread_ts: None,
                },
            ]
        );
    }

    #[test]
    fn an_empty_payload_renders_nothing() {
        assert!(render("C1", &payload("   ")).is_empty());
    }
}
