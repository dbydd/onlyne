use crate::kit::error::KitError;
use pulldown_cmark::{Event, Options, Parser, Tag, TagEnd};
use resvg::{tiny_skia, usvg};
use unicode_display_width::width as display_width;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RenderTable {
    pub rows: usize,
    pub columns: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MarkdownSegment {
    Text(String),
    Table(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RenderedMarkdownTable {
    Text { text: String, table: RenderTable },
    Png { png: Vec<u8>, table: RenderTable },
}

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

pub fn plain_text_fallback(input: &str) -> String {
    let mut out = Vec::new();
    for segment in split_tables(input) {
        match segment {
            MarkdownSegment::Text(text) => out.push(markdown_text(&text)),
            MarkdownSegment::Table(table) => out.push(table_plain_text(&table)),
        }
    }
    tidy(&out.join("\n\n"))
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

pub fn parse_table_rows(table: &str) -> Vec<Vec<String>> {
    let mut rows = Vec::new();
    let mut current_row: Option<Vec<String>> = None;
    let mut current_cell: Option<String> = None;
    let parser = Parser::new_ext(table, options());
    for ev in parser {
        match ev {
            Event::Start(Tag::TableHead | Tag::TableRow) => current_row = Some(Vec::new()),
            Event::Start(Tag::TableCell) => current_cell = Some(String::new()),
            Event::End(TagEnd::TableCell) => {
                if let Some(cell) = current_cell.take() {
                    if let Some(row) = current_row.as_mut() {
                        row.push(tidy(&cell));
                    }
                }
            }
            Event::End(TagEnd::TableHead | TagEnd::TableRow) => {
                if let Some(row) = current_row.take() {
                    rows.push(row);
                }
            }
            Event::Text(t)
            | Event::Code(t)
            | Event::Html(t)
            | Event::InlineHtml(t)
            | Event::FootnoteReference(t)
            | Event::InlineMath(t)
            | Event::DisplayMath(t) => {
                if let Some(cell) = current_cell.as_mut() {
                    if !cell.is_empty() {
                        cell.push(' ');
                    }
                    cell.push_str(&t);
                }
            }
            Event::SoftBreak | Event::HardBreak => {
                if let Some(cell) = current_cell.as_mut() {
                    cell.push(' ');
                }
            }
            _ => {}
        }
    }
    if rows.is_empty() {
        parse_table_rows_line_based(table)
    } else {
        rows
    }
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

pub fn render_markdown_table(table: &str) -> Result<RenderedMarkdownTable, KitError> {
    let rows = normalized_table_rows(table)?;
    let table = RenderTable {
        rows: rows.len(),
        columns: rows.first().map_or(0, Vec::len),
    };
    if table.columns <= 1 {
        Ok(RenderedMarkdownTable::Text {
            text: table_plain_text_from_rows(&rows),
            table,
        })
    } else {
        let png = render_table_png_bytes(&rows)?;
        Ok(RenderedMarkdownTable::Png { png, table })
    }
}

pub fn render_markdown_table_png(table: &str) -> Result<(Vec<u8>, RenderTable), KitError> {
    let rows = normalized_table_rows(table)?;
    let table = RenderTable {
        rows: rows.len(),
        columns: rows.first().map_or(0, Vec::len),
    };
    Ok((render_table_png_bytes(&rows)?, table))
}

fn normalized_table_rows(table: &str) -> Result<Vec<Vec<String>>, KitError> {
    let rows = parse_table_rows(table);
    if rows.is_empty() {
        return Err(KitError::render_failure(
            "markdown_table",
            "markdown table has no rows",
        ));
    }
    let cols = rows.iter().map(Vec::len).max().unwrap_or(0);
    if cols == 0 {
        return Err(KitError::render_failure(
            "markdown_table",
            "markdown table has no columns",
        ));
    }
    Ok(rows
        .into_iter()
        .map(|mut row| {
            row.resize(cols, String::new());
            row
        })
        .collect())
}

fn render_table_png_bytes(rows: &[Vec<String>]) -> Result<Vec<u8>, KitError> {
    let col_widths = column_widths(rows);
    let row_h = 56u32;
    let pad = 24u32;
    let width = col_widths.iter().sum::<u32>() + pad * 2;
    let height = row_h * rows.len() as u32 + pad * 2;
    let svg = table_svg(rows, &col_widths, width, height, row_h, pad);

    let mut opt = usvg::Options::default();
    opt.fontdb_mut().load_system_fonts();
    opt.font_family = "sans-serif".into();
    let tree = usvg::Tree::from_str(&svg, &opt)
        .map_err(|err| KitError::render_failure("markdown_table", err.to_string()))?;
    let size = tree.size();
    let mut pixmap = tiny_skia::Pixmap::new(size.width() as u32, size.height() as u32)
        .ok_or_else(|| KitError::render_failure("markdown_table", "create table pixmap"))?;
    pixmap.fill(tiny_skia::Color::WHITE);
    resvg::render(&tree, tiny_skia::Transform::default(), &mut pixmap.as_mut());
    pixmap
        .encode_png()
        .map_err(|err| KitError::render_failure("markdown_table", err.to_string()))
}

fn column_widths(rows: &[Vec<String>]) -> Vec<u32> {
    let cols = rows.first().map_or(0, Vec::len);
    (0..cols)
        .map(|i| {
            rows.iter()
                .map(|row| display_width(row[i].as_str()) as usize)
                .max()
                .unwrap_or(0)
                .clamp(3, 40) as u32
                * 14
                + 36
        })
        .collect()
}

fn table_svg(
    rows: &[Vec<String>],
    col_widths: &[u32],
    width: u32,
    height: u32,
    row_h: u32,
    pad: u32,
) -> String {
    let mut out = format!(
        r##"<svg xmlns="http://www.w3.org/2000/svg" width="{width}" height="{height}" viewBox="0 0 {width} {height}"><rect width="100%" height="100%" fill="#ffffff"/>"##
    );
    let font = "-apple-system, BlinkMacSystemFont, Segoe UI, Noto Sans CJK SC, Noto Sans CJK, Arial Unicode MS, sans-serif";
    for (ri, row) in rows.iter().enumerate() {
        let y = pad + ri as u32 * row_h;
        let mut x = pad;
        for (ci, cell) in row.iter().enumerate() {
            let w = col_widths[ci];
            let fill = if ri == 0 { "#f6f8fa" } else { "#ffffff" };
            let weight = if ri == 0 { 700 } else { 400 };
            out.push_str(&format!(
                r##"<rect x="{x}" y="{y}" width="{w}" height="{row_h}" fill="{fill}" stroke="#d0d7de" stroke-width="2"/><text x="{}" y="{}" fill="#111111" font-family="{}" font-size="22" font-weight="{}">{}</text>"##,
                x + 14,
                y + 36,
                escape_xml(font),
                weight,
                escape_xml(cell)
            ));
            x += w;
        }
    }
    out.push_str("</svg>");
    out
}

fn markdown_text(input: &str) -> String {
    let mut out = String::new();
    let parser = Parser::new_ext(input, options());
    for ev in parser {
        match ev {
            Event::Start(tag) => match tag {
                Tag::Item => out.push_str("- "),
                Tag::BlockQuote(_) => out.push_str("> "),
                _ => {}
            },
            Event::End(tag) => match tag {
                TagEnd::Paragraph | TagEnd::Heading(_) | TagEnd::Item | TagEnd::CodeBlock
                    if !out.ends_with('\n') =>
                {
                    out.push('\n');
                }
                _ => {}
            },
            Event::Text(t)
            | Event::Code(t)
            | Event::Html(t)
            | Event::InlineHtml(t)
            | Event::FootnoteReference(t)
            | Event::InlineMath(t)
            | Event::DisplayMath(t) => out.push_str(&t),
            Event::SoftBreak | Event::HardBreak => out.push('\n'),
            Event::Rule => out.push_str("\n---\n"),
            Event::TaskListMarker(done) => out.push_str(if done { "[x] " } else { "[ ] " }),
        }
    }
    tidy(&out)
}

fn table_plain_text(table: &str) -> String {
    table_plain_text_from_rows(&parse_table_rows(table))
}

fn table_plain_text_from_rows(rows: &[Vec<String>]) -> String {
    if rows.is_empty() {
        return String::new();
    }
    let cols = rows.iter().map(Vec::len).max().unwrap_or(0);
    let mut normalized: Vec<Vec<String>> = rows
        .iter()
        .cloned()
        .map(|mut row| {
            row.resize(cols, String::new());
            row
        })
        .collect();
    let widths: Vec<usize> = (0..cols)
        .map(|i| {
            normalized
                .iter()
                .map(|row| display_width(row[i].as_str()) as usize)
                .max()
                .unwrap_or(0)
        })
        .collect();
    normalized
        .iter_mut()
        .map(|row| {
            row.iter()
                .enumerate()
                .map(|(i, cell)| pad_cell(cell, widths[i]))
                .collect::<Vec<_>>()
                .join(" | ")
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn pad_cell(cell: &str, width: usize) -> String {
    let cell_width = display_width(cell) as usize;
    let mut out = cell.to_string();
    if width > cell_width {
        out.push_str(&" ".repeat(width - cell_width));
    }
    out
}

fn parse_table_rows_line_based(table: &str) -> Vec<Vec<String>> {
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

fn escape_xml(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    escape_html(s, &mut out);
    out
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

    const PNG_SIGNATURE: &[u8; 8] = b"\x89PNG\r\n\x1a\n";

    #[test]
    fn gfm_table_renders_to_png_bytes() {
        let (png, table) =
            render_markdown_table_png("| A | B |\n| --- | --- |\n| 1 | 2 |").unwrap();
        assert_eq!(table.rows, 2);
        assert_eq!(table.columns, 2);
        assert!(png.starts_with(PNG_SIGNATURE));
    }

    #[test]
    fn narrow_single_column_table_takes_text_path() {
        let rendered = render_markdown_table("| A |\n| --- |\n| 1 |").unwrap();
        match rendered {
            RenderedMarkdownTable::Text { text, table } => {
                assert_eq!(table.rows, 2);
                assert_eq!(table.columns, 1);
                assert!(text.contains('A'));
                assert!(text.contains('1'));
            }
            RenderedMarkdownTable::Png { .. } => panic!("single column table rendered as image"),
        }
    }

    #[test]
    fn cjk_headers_produce_wider_png_than_ascii_headers() {
        let (ascii_png, _) =
            render_markdown_table_png("| Name | State |\n| --- | --- |\n| Bot | Ready |").unwrap();
        let (cjk_png, _) =
            render_markdown_table_png("| 渠道名称 | 当前状态 |\n| --- | --- |\n| Bot | Ready |")
                .unwrap();
        assert!(ascii_png.starts_with(PNG_SIGNATURE));
        assert!(cjk_png.starts_with(PNG_SIGNATURE));
        assert_ne!(ascii_png.len(), cjk_png.len());
        assert!(ihdr_width(&cjk_png) > ihdr_width(&ascii_png));
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
    fn parses_table_rows_without_separator() {
        let rows = parse_table_rows("| A | B |\n| --- | :---: |\n| 1 | 2 |");
        assert_eq!(rows, vec![vec!["A", "B"], vec!["1", "2"]]);
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

    fn ihdr_width(png: &[u8]) -> u32 {
        assert!(png.starts_with(PNG_SIGNATURE));
        assert_eq!(&png[12..16], b"IHDR");
        u32::from_be_bytes([png[16], png[17], png[18], png[19]])
    }
}
