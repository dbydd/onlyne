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

pub fn table_code_block(table: &str) -> String {
    let mut rows: Vec<Vec<String>> = Vec::new();
    for line in table.lines().map(str::trim).filter(|l| l.contains('|')) {
        let cells: Vec<String> = line
            .trim_matches('|')
            .split('|')
            .map(|c| c.trim().to_string())
            .collect();
        if cells.iter().all(|c| {
            let c = c.replace(':', "");
            c.chars().all(|ch| ch == '-') && c.contains('-')
        }) {
            continue;
        }
        rows.push(cells);
    }
    if rows.is_empty() {
        return format!("```\n{}\n```", table.trim());
    }
    let cols = rows.iter().map(Vec::len).max().unwrap_or(0);
    for row in &mut rows {
        row.resize(cols, String::new());
    }
    let widths: Vec<usize> = (0..cols)
        .map(|i| rows.iter().map(|r| display_width(&r[i])).max().unwrap_or(0))
        .collect();
    let mut out = String::from("```\n");
    for (ri, row) in rows.iter().enumerate() {
        out.push('│');
        for (cell, width) in row.iter().zip(&widths) {
            out.push(' ');
            out.push_str(cell);
            for _ in 0..(width.saturating_sub(display_width(cell)) + 1) {
                out.push(' ');
            }
            out.push('│');
        }
        out.push('\n');
        if ri == 0 && rows.len() > 1 {
            out.push('├');
            for (i, width) in widths.iter().enumerate() {
                out.push_str(&"─".repeat(width + 2));
                out.push(if i + 1 == widths.len() { '┤' } else { '┼' });
            }
            out.push('\n');
        }
    }
    out.push_str("```");
    out
}

fn display_width(s: &str) -> usize {
    s.chars().map(|c| if c.is_ascii() { 1 } else { 2 }).sum()
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
    fn strips_common_markdown_to_readable_text() {
        let out = plain_text("# Hi\n\n- **bold** `code`\n- [link](https://example.com)");
        assert!(out.contains("Hi"));
        assert!(out.contains("bold code"));
        assert!(out.contains("https://example.com"));
    }

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
    fn table_code_block_formats_cells() {
        let out = table_code_block("| 渠道 | 状态 |\n| --- | --- |\n| Telegram | HTML |\n");
        assert!(out.starts_with("```\n"));
        assert!(out.contains("│ 渠道"));
        assert!(out.contains("Telegram"));
        assert!(out.ends_with("```"));
    }

    #[test]
    fn splits_first_h1_for_card_header() {
        let (title, body) = split_first_heading("# Title\n\nbody");
        assert_eq!(title.as_deref(), Some("Title"));
        assert_eq!(body, "body");
    }

    #[test]
    fn flags_html_for_whole_message_fallback() {
        let check = check("hello <span>raw</span>");
        assert_eq!(check.unsupported_reason.as_deref(), Some("html"));
    }
}
