use pulldown_cmark::{Event, Options, Parser, Tag, TagEnd};

pub fn unsupported_reason(input: &str) -> Option<String> {
    let parser = Parser::new_ext(input, options());
    for ev in parser {
        match ev {
            Event::Html(_) | Event::InlineHtml(_) => return Some("html".into()),
            Event::Start(Tag::Image { .. }) => return Some("image node".into()),
            Event::FootnoteReference(_) => return Some("footnote".into()),
            Event::InlineMath(_) | Event::DisplayMath(_) => return Some("math".into()),
            _ => {}
        }
    }
    None
}

pub fn telegram_html(input: &str) -> String {
    let mut out = String::new();
    let parser = Parser::new_ext(input, options());
    for ev in parser {
        match ev {
            Event::Start(tag) => match tag {
                Tag::Paragraph => {}
                Tag::Heading { .. } => out.push_str("<b>"),
                Tag::Strong => out.push_str("<b>"),
                Tag::Emphasis => out.push_str("<i>"),
                Tag::Strikethrough => out.push_str("<s>"),
                Tag::CodeBlock(_) => out.push_str("<pre><code>"),
                Tag::Link { dest_url, .. } => {
                    out.push_str("<a href=\"");
                    escape_html(&dest_url, &mut out);
                    out.push_str("\">");
                }
                Tag::List(_) => {}
                Tag::Item => out.push_str("• "),
                Tag::BlockQuote(_) => out.push_str("<blockquote>"),
                Tag::Table(_) | Tag::TableHead | Tag::TableRow | Tag::TableCell => {}
                _ => {}
            },
            Event::End(tag) => match tag {
                TagEnd::Paragraph | TagEnd::Item | TagEnd::TableRow => {
                    if !out.ends_with('\n') {
                        out.push('\n');
                    }
                }
                TagEnd::Heading(_) => out.push_str("</b>\n"),
                TagEnd::Strong => out.push_str("</b>"),
                TagEnd::Emphasis => out.push_str("</i>"),
                TagEnd::Strikethrough => out.push_str("</s>"),
                TagEnd::CodeBlock => out.push_str("</code></pre>\n"),
                TagEnd::Link => out.push_str("</a>"),
                TagEnd::BlockQuote(_) => out.push_str("</blockquote>\n"),
                TagEnd::TableCell => out.push_str(" | "),
                _ => {}
            },
            Event::Text(t) => escape_html(&t, &mut out),
            Event::Code(t) => {
                out.push_str("<code>");
                escape_html(&t, &mut out);
                out.push_str("</code>");
            }
            Event::SoftBreak | Event::HardBreak => out.push('\n'),
            Event::Rule => out.push_str("\n---\n"),
            Event::Html(t) | Event::InlineHtml(t) => escape_html(&t, &mut out),
            Event::FootnoteReference(t) => escape_html(&t, &mut out),
            Event::TaskListMarker(done) => out.push_str(if done { "☑ " } else { "☐ " }),
            Event::InlineMath(t) | Event::DisplayMath(t) => escape_html(&t, &mut out),
        }
    }
    tidy(&out)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MarkdownSegment {
    Text(String),
    Table(String),
}

pub fn split_tables(input: &str) -> Vec<MarkdownSegment> {
    let lines: Vec<&str> = input.lines().collect();
    let mut out = Vec::new();
    let mut buf = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        if i + 1 < lines.len() && looks_like_table_header(lines[i], lines[i + 1]) {
            flush_text(&mut out, &mut buf);
            let mut table = vec![lines[i], lines[i + 1]];
            i += 2;
            while i < lines.len() && lines[i].contains('|') && !lines[i].trim().is_empty() {
                table.push(lines[i]);
                i += 1;
            }
            out.push(MarkdownSegment::Table(table.join("\n")));
            continue;
        }
        buf.push(lines[i]);
        i += 1;
    }
    flush_text(&mut out, &mut buf);
    out
}

fn looks_like_table_header(header: &str, sep: &str) -> bool {
    header.contains('|')
        && sep.contains('|')
        && sep
            .chars()
            .all(|c| matches!(c, '|' | '-' | ':' | ' ' | '\t'))
        && sep.contains("---")
}

fn flush_text(out: &mut Vec<MarkdownSegment>, buf: &mut Vec<&str>) {
    let text = buf.join("\n").trim().to_string();
    if !text.is_empty() {
        out.push(MarkdownSegment::Text(text));
    }
    buf.clear();
}

pub fn split_first_heading(input: &str) -> (Option<String>, String) {
    let mut lines = input.lines();
    if let Some(first) = lines.next() {
        if let Some(title) = first.strip_prefix("# ").filter(|s| !s.trim().is_empty()) {
            return (
                Some(title.trim().to_string()),
                lines.collect::<Vec<_>>().join("\n").trim().to_string(),
            );
        }
    }
    (None, input.to_string())
}

fn escape_html(input: &str, out: &mut String) {
    for c in input.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            _ => out.push(c),
        }
    }
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
    fn renders_common_markdown_to_telegram_html() {
        let out = telegram_html("# Hi\n\n- **bold** `code`\n- [link](https://example.com)");
        assert!(out.contains("<b>Hi</b>"));
        assert!(out.contains("<b>bold</b>"));
        assert!(out.contains("<code>code</code>"));
        assert!(out.contains("<a href=\"https://example.com\">link</a>"));
    }

    #[test]
    fn splits_table_segments() {
        let parts = split_tables("before\n\n| A | B |\n| --- | --- |\n| 1 | 2 |\n\nafter");
        assert_eq!(parts.len(), 3);
        assert!(matches!(&parts[0], MarkdownSegment::Text(s) if s == "before"));
        assert!(matches!(&parts[1], MarkdownSegment::Table(s) if s.contains("| 1 | 2 |")));
        assert!(matches!(&parts[2], MarkdownSegment::Text(s) if s == "after"));
    }

    #[test]
    fn splits_first_h1_for_card_header() {
        let (title, body) = split_first_heading("# Title\n\nbody");
        assert_eq!(title.as_deref(), Some("Title"));
        assert_eq!(body, "body");
    }

    #[test]
    fn flags_html_as_unsupported() {
        assert_eq!(
            unsupported_reason("hello <span>raw</span>").as_deref(),
            Some("html")
        );
    }
}
