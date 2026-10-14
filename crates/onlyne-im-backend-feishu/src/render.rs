//! The presentation layer: turn a standard payload into Feishu request JSON.
//!
//! Ported from `frozen/gateway-v1`'s outbound translation (`text_request`,
//! `card_request`, `markdown_card` and the table/code-span renderers). This is
//! the only place Feishu's own shapes appear: the core hands over `text`,
//! `format`, and `reply_to`, and this module decides what `POST
//! /open-apis/im/v1/messages` receives.
//!
//! v1 routed between text and card with a `looks_like_markdown` heuristic
//! because its `Outbound` carried no format field; the standard payload does,
//! so the heuristic is gone: `ImFormat::Plain` renders a `text` message,
//! `ImFormat::Markdown` renders an `interactive` card. v1 attached no buttons
//! and uploaded no files on this path, so neither does this render.

use onlyne_im_backend::payload::{ImFormat, ImPayload};
use serde_json::{Value, json};

/// One send-message request body (`POST /open-apis/im/v1/messages`), fully
/// rendered. `receive_id_type` is decided by the conversation's shape at the
/// API layer, not here.
#[derive(Debug, Clone, PartialEq)]
pub struct Request {
    pub body: Value,
}

/// Render one payload into the ordered requests that show it.
///
/// A payload with text is one request — plain text as a `text` message,
/// markdown as an interactive card with the first `# ` heading as its header.
/// A payload with no text (v1's outbound had no attachments) renders nothing.
pub fn render(conversation: &str, payload: &ImPayload) -> Vec<Request> {
    let text = payload.text.trim();
    if text.is_empty() {
        return Vec::new();
    }
    let request = match payload.format {
        ImFormat::Plain => text_request(conversation, text, payload.reply_to.as_deref()),
        ImFormat::Markdown => card_request(conversation, text, payload.reply_to.as_deref()),
    };
    vec![Request { body: request }]
}

/// The wire request body for a plain-text message.
pub fn text_request(conversation: &str, text: &str, reply_to: Option<&str>) -> Value {
    wire_request(conversation, "text", json!({"text": text}), reply_to)
}

/// The wire request body for an interactive card built from markdown.
pub fn card_request(conversation: &str, markdown: &str, reply_to: Option<&str>) -> Value {
    wire_request(
        conversation,
        "interactive",
        markdown_card(markdown),
        reply_to,
    )
}

fn wire_request(
    conversation: &str,
    msg_type: &str,
    content: Value,
    reply_to: Option<&str>,
) -> Value {
    let mut body = json!({
        "receive_id": conversation,
        "msg_type": msg_type,
        "content": content.to_string(),
    });
    if let Some(reply_to) = reply_to.filter(|r| !r.is_empty()) {
        body["reply_id"] = Value::String(reply_to.to_string());
    }
    body
}

/// Render markdown to a Feishu interactive card: first `# ` heading becomes
/// the header; tables become card tables; the rest stays a markdown element.
pub fn markdown_card(markdown: &str) -> Value {
    let (title, body) = split_first_heading(markdown);
    let mut card = json!({
        "config": {"wide_screen_mode": true},
        "elements": markdown_elements(&body),
    });
    if let Some(title) = title {
        card["header"] =
            json!({"template": "blue", "title": {"tag": "plain_text", "content": title}});
    }
    card
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Segment {
    Text(String),
    Table(String),
}

fn split_tables(input: &str) -> Vec<Segment> {
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
            out.push(Segment::Table(table.join("\n")));
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

fn flush_text(out: &mut Vec<Segment>, buf: &mut Vec<&str>) {
    let text = buf.join("\n").trim().to_string();
    if !text.is_empty() {
        out.push(Segment::Text(text));
    }
    buf.clear();
}

fn split_first_heading(input: &str) -> (Option<String>, String) {
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

fn markdown_elements(markdown: &str) -> Vec<Value> {
    let mut elements = Vec::new();
    for segment in split_tables(markdown) {
        match segment {
            Segment::Text(text) => {
                let content = markdown_body(&text);
                if !content.is_empty() {
                    elements.push(json!({"tag": "markdown", "content": content}));
                }
            }
            Segment::Table(table) => {
                if let Some(table) = table_element(&table) {
                    elements.push(table);
                }
            }
        }
    }
    if elements.is_empty() {
        elements.push(json!({"tag": "markdown", "content": markdown_body(markdown)}));
    }
    elements
}

/// Turn one backtick-inline code span into a Feishu-markdown code block
/// (ported from the legacy adapter).
fn markdown_body(markdown: &str) -> String {
    let chars: Vec<char> = markdown.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] != '`' {
            out.push(chars[i]);
            i += 1;
            continue;
        }
        let mut run = 1;
        while i + run < chars.len() && chars[i + run] == '`' {
            run += 1;
        }
        if run != 1 {
            out.extend(std::iter::repeat_n('`', run));
            i += run;
            continue;
        }
        i += 1;
        let mut code = String::new();
        while i < chars.len() && chars[i] != '`' {
            code.push(chars[i]);
            i += 1;
        }
        if i < chars.len() {
            i += 1;
            out.push_str("\n```\n");
            out.push_str(&code);
            out.push_str("\n```\n");
        } else {
            out.push('`');
            out.push_str(&code);
        }
    }
    out.trim().to_string()
}

fn table_element(table: &str) -> Option<Value> {
    let rows = parse_table_rows(table);
    let header = rows.first()?;
    let width = header.len();
    if width == 0 {
        return None;
    }
    let columns: Vec<Value> = header
        .iter()
        .enumerate()
        .map(|(i, name)| {
            json!({
                "name": format!("col_{i}"),
                "display_name": if name.is_empty() { format!("Column {}", i + 1) } else { name.clone() },
                "data_type": "text",
                "width": "auto",
            })
        })
        .collect();
    let data_rows: Vec<Value> = rows
        .iter()
        .skip(1)
        .map(|row| {
            let mut obj = serde_json::Map::new();
            for i in 0..width {
                obj.insert(
                    format!("col_{i}"),
                    Value::String(row.get(i).cloned().unwrap_or_default()),
                );
            }
            Value::Object(obj)
        })
        .collect();
    Some(json!({
        "tag": "table",
        "page_size": data_rows.len().clamp(1, 10),
        "row_height": "low",
        "header_style": {"background_style": "grey", "bold": true},
        "columns": columns,
        "rows": data_rows,
    }))
}

fn parse_table_rows(table: &str) -> Vec<Vec<String>> {
    table
        .lines()
        .map(str::trim)
        .filter(|line| line.contains('|'))
        .filter_map(|line| {
            let cells: Vec<String> = line
                .trim_matches('|')
                .split('|')
                .map(|cell| cell.trim().to_string())
                .collect();
            if cells.iter().all(|cell| {
                let c = cell.replace(':', "");
                c.contains('-') && c.chars().all(|ch| ch == '-')
            }) {
                None
            } else {
                Some(cells)
            }
        })
        .collect()
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
    fn plain_text_is_one_text_message() {
        let requests = render("oc_group", &payload("hello"));
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].body["msg_type"], "text");
        assert_eq!(requests[0].body["receive_id"], "oc_group");
        let content: Value =
            serde_json::from_str(requests[0].body["content"].as_str().unwrap()).unwrap();
        assert_eq!(content["text"], "hello");
        assert!(requests[0].body.get("reply_id").is_none());
    }

    #[test]
    fn markdown_is_one_interactive_card_with_the_heading_as_header() {
        let mut p = payload("# hi\n\n**bold**");
        p.format = ImFormat::Markdown;
        let requests = render("oc_group", &p);
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].body["msg_type"], "interactive");
        let card: Value =
            serde_json::from_str(requests[0].body["content"].as_str().unwrap()).unwrap();
        assert_eq!(
            card.pointer("/header/title/content")
                .and_then(Value::as_str),
            Some("hi")
        );
        assert_eq!(
            card.pointer("/elements/0/tag").and_then(Value::as_str),
            Some("markdown")
        );
    }

    #[test]
    fn a_reply_to_ride_as_the_reply_id() {
        let mut p = payload("hello");
        p.reply_to = Some("om_9".into());
        let requests = render("oc_group", &p);
        assert_eq!(requests[0].body["reply_id"], "om_9");
    }

    #[test]
    fn an_empty_payload_renders_nothing() {
        assert!(render("oc_group", &payload("   ")).is_empty());
    }

    #[test]
    fn a_markdown_table_becomes_a_card_table() {
        let mut p = payload("# t\n\n| a | b |\n|---|---|\n| 1 | 2 |");
        p.format = ImFormat::Markdown;
        let requests = render("oc_group", &p);
        let card: Value =
            serde_json::from_str(requests[0].body["content"].as_str().unwrap()).unwrap();
        assert_eq!(
            card.pointer("/elements/0/tag").and_then(Value::as_str),
            Some("table")
        );
        assert_eq!(
            card.pointer("/elements/0/columns/0/display_name"),
            Some(&Value::String("a".into()))
        );
        assert_eq!(
            card.pointer("/elements/0/rows/0/col_0"),
            Some(&Value::String("1".into()))
        );
    }

    #[test]
    fn an_inline_code_span_becomes_a_code_block() {
        let mut p = payload("run `cargo test` now");
        p.format = ImFormat::Markdown;
        let requests = render("oc_group", &p);
        let card: Value =
            serde_json::from_str(requests[0].body["content"].as_str().unwrap()).unwrap();
        let content = card
            .pointer("/elements/0/content")
            .and_then(Value::as_str)
            .unwrap();
        assert!(content.contains("```\ncargo test\n```"), "{content}");
    }
}
