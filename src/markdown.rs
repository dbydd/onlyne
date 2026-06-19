use pulldown_cmark::{Event, Options, Parser, Tag, TagEnd};

#[derive(Debug, Clone)]
pub struct MarkdownCheck {
    pub plain_text: String,
    pub unsupported_reason: Option<String>,
}

pub fn check(input: &str) -> MarkdownCheck {
    let mut plain = String::new();
    let mut unsupported = None;
    let parser = Parser::new_ext(input, options());
    for ev in parser {
        match ev {
            Event::Text(t) | Event::Code(t) => plain.push_str(&t),
            Event::SoftBreak | Event::HardBreak => plain.push('\n'),
            Event::Rule => plain.push_str("\n---\n"),
            Event::Html(_) | Event::InlineHtml(_) => {
                unsupported.get_or_insert_with(|| "html".into());
            }
            Event::Start(tag) => match tag {
                Tag::Paragraph | Tag::BlockQuote(_) | Tag::List(_) | Tag::Item => {}
                Tag::Heading { .. } => {
                    if !plain.ends_with('\n') && !plain.is_empty() {
                        plain.push('\n');
                    }
                }
                Tag::CodeBlock(_) => {
                    if !plain.ends_with('\n') && !plain.is_empty() {
                        plain.push('\n');
                    }
                }
                Tag::Link { dest_url, .. } => {
                    if !dest_url.is_empty() {
                        plain.push('(');
                        plain.push_str(&dest_url);
                        plain.push(')');
                    }
                }
                Tag::Image { .. } => {
                    unsupported.get_or_insert_with(|| "image node".into());
                }
                Tag::Table(_) | Tag::TableHead | Tag::TableRow | Tag::TableCell => {}
                _ => {}
            },
            Event::End(tag) => match tag {
                TagEnd::Paragraph | TagEnd::Heading(_) | TagEnd::Item | TagEnd::CodeBlock => {
                    if !plain.ends_with('\n') {
                        plain.push('\n');
                    }
                }
                _ => {}
            },
            Event::FootnoteReference(_) => {
                unsupported.get_or_insert_with(|| "footnote".into());
            }
            Event::TaskListMarker(done) => plain.push_str(if done { "[x] " } else { "[ ] " }),
            Event::InlineMath(t) | Event::DisplayMath(t) => {
                unsupported.get_or_insert_with(|| "math".into());
                plain.push_str(&t);
            }
        }
    }
    MarkdownCheck {
        plain_text: tidy(&plain),
        unsupported_reason: unsupported,
    }
}

pub fn plain_text(input: &str) -> String {
    check(input).plain_text
}

fn options() -> Options {
    let mut options = Options::empty();
    options.insert(Options::ENABLE_TABLES);
    options.insert(Options::ENABLE_FOOTNOTES);
    options.insert(Options::ENABLE_STRIKETHROUGH);
    options.insert(Options::ENABLE_TASKLISTS);
    options
}

fn tidy(s: &str) -> String {
    s.lines()
        .map(str::trim_end)
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_common_markdown_to_readable_text() {
        let out = plain_text("# Hi\n\n- **bold** `code`\n- [link](https://example.com)");
        assert!(out.contains("Hi"));
        assert!(out.contains("bold code"));
        assert!(out.contains("https://example.com"));
    }

    #[test]
    fn flags_html_for_whole_message_fallback() {
        let check = check("hello <span>raw</span>");
        assert_eq!(check.unsupported_reason.as_deref(), Some("html"));
    }
}
