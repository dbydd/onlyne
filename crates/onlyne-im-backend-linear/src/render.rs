//! The presentation layer: turn a standard payload into one Linear comment.
//!
//! Linear comment bodies are markdown, and a comment carries no interactive
//! elements. Files reach a comment as markdown links: a URL the payload already
//! names is linked directly, and a local path is uploaded first
//! ([`crate::api::Client::upload`]) so the comment links bytes a reader can
//! actually open. A payload Linear cannot render is refused here rather than
//! sent half-visible.

use anyhow::{Result, bail};
use onlyne_im_backend::payload::{ImAttachment, ImPayload};

/// One payload, ready for `commentCreate` apart from attachment URLs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rendered {
    /// The comment body before attachment links are appended.
    pub body: String,
    /// Attachments in payload order, each still needing a URL.
    pub attachments: Vec<Attachment>,
}

/// One attachment, and where its URL comes from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Attachment {
    /// A URL the payload names; the comment links it as-is.
    Url { label: String, url: String },
    /// A local file whose bytes must be uploaded before a link exists.
    Local { label: String, path: String },
}

/// Render one payload into a comment body plus the attachments it names.
///
/// A payload with neither text nor attachments is not renderable: Linear
/// refuses an empty comment, so saying so here gives the sender's caller a
/// sentence instead of a platform error.
pub fn render(payload: &ImPayload) -> Result<Rendered> {
    // Linear comments have no interactive elements and no callback path, so an
    // action cannot be honored; `hello` declares `action: false`, which keeps
    // the core from aiming one at this backend.
    let attachments = payload
        .attachments
        .iter()
        .enumerate()
        .map(|(index, attachment)| attachment_of(attachment, index))
        .collect::<Result<Vec<_>>>()?;
    if payload.text.trim().is_empty() && attachments.is_empty() {
        bail!("the payload has no text and no attachments");
    }
    Ok(Rendered {
        body: payload.text.trim().to_string(),
        attachments,
    })
}

/// One markdown list item linking an attachment.
pub fn link_line(label: &str, url: &str) -> String {
    format!("- [{}]({url})", escape_label(label))
}

fn attachment_of(attachment: &ImAttachment, index: usize) -> Result<Attachment> {
    let path = attachment.path.trim();
    if path.is_empty() {
        bail!("attachment {index} names no path");
    }
    let label = label_of(attachment, path);
    if path.starts_with("http://") || path.starts_with("https://") {
        Ok(Attachment::Url {
            label,
            url: path.to_string(),
        })
    } else {
        Ok(Attachment::Local {
            label,
            path: path.to_string(),
        })
    }
}

/// What a reader sees for the link: the payload's `alt`, else the file name.
fn label_of(attachment: &ImAttachment, path: &str) -> String {
    attachment
        .alt
        .as_deref()
        .map(str::trim)
        .filter(|alt| !alt.is_empty())
        .map(str::to_string)
        .or_else(|| {
            std::path::Path::new(path)
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
        })
        .unwrap_or_else(|| "attachment".to_string())
}

/// Keep a label from closing the link early.
fn escape_label(label: &str) -> String {
    label.replace('\\', "\\\\").replace(']', "\\]")
}

#[cfg(test)]
mod tests {
    use super::*;
    use onlyne_im_backend::payload::ImFormat;

    fn payload(text: &str, attachments: Vec<ImAttachment>) -> ImPayload {
        ImPayload {
            text: text.to_string(),
            format: ImFormat::Markdown,
            attachments,
            ..ImPayload::default()
        }
    }

    /// The render decisions in one table: which attachment a path becomes,
    /// which label it gets, and which payloads are refused.
    #[test]
    fn payloads_render_as_linear_comments() {
        struct Case {
            name: &'static str,
            payload: ImPayload,
            renders: bool,
            attachments: usize,
            first_is_upload: bool,
            first_label: Option<&'static str>,
        }
        let local = ImAttachment {
            kind: "file".into(),
            path: "/tmp/report.txt".into(),
            alt: None,
        };
        let remote = ImAttachment {
            kind: "image".into(),
            path: "https://example.com/a.png".into(),
            alt: Some("the graph".into()),
        };
        let named = ImAttachment {
            kind: "file".into(),
            path: "/tmp/x.log".into(),
            alt: Some("build log".into()),
        };
        let cases = [
            Case {
                name: "text only",
                payload: payload("deploy please", vec![]),
                renders: true,
                attachments: 0,
                first_is_upload: false,
                first_label: None,
            },
            Case {
                name: "attachments only",
                payload: payload("   ", vec![local.clone()]),
                renders: true,
                attachments: 1,
                first_is_upload: true,
                first_label: Some("report.txt"),
            },
            Case {
                name: "remote url",
                payload: payload("see", vec![remote]),
                renders: true,
                attachments: 1,
                first_is_upload: false,
                first_label: Some("the graph"),
            },
            Case {
                name: "alt wins over file name",
                payload: payload("", vec![named]),
                renders: true,
                attachments: 1,
                first_is_upload: true,
                first_label: Some("build log"),
            },
            Case {
                name: "nothing",
                payload: payload("  \n ", vec![]),
                renders: false,
                attachments: 0,
                first_is_upload: false,
                first_label: None,
            },
        ];
        for case in cases {
            let rendered = render(&case.payload);
            assert_eq!(rendered.is_ok(), case.renders, "{}", case.name);
            let Ok(rendered) = rendered else { continue };
            assert_eq!(
                rendered.attachments.len(),
                case.attachments,
                "{}",
                case.name
            );
            if let Some(first) = rendered.attachments.first() {
                assert_eq!(
                    matches!(first, Attachment::Local { .. }),
                    case.first_is_upload,
                    "{}",
                    case.name
                );
                let label = match first {
                    Attachment::Local { label, .. } | Attachment::Url { label, .. } => label,
                };
                assert_eq!(Some(label.as_str()), case.first_label, "{}", case.name);
            }
        }
    }

    #[test]
    fn an_attachment_without_a_path_is_refused_and_named_by_position() {
        let bad = ImAttachment {
            kind: "file".into(),
            path: "  ".into(),
            alt: None,
        };
        let error = render(&payload("text", vec![bad])).unwrap_err();
        assert!(format!("{error:#}").contains("attachment 0"), "{error:#}");
    }

    #[test]
    fn a_link_label_keeps_brackets_from_closing_the_link() {
        assert_eq!(
            link_line("a]b.txt", "https://x"),
            "- [a\\]b.txt](https://x)"
        );
    }
}
