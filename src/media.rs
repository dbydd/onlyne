use crate::markdown;
use anyhow::{Context, anyhow};
use resvg::{tiny_skia, usvg};
use sha2::{Digest, Sha256};
use std::{
    path::{Path, PathBuf},
    process::Stdio,
};
use tokio::{fs, process::Command};
use unicode_display_width::width as display_width;

pub async fn read_bytes(path_or_url: &str) -> anyhow::Result<Vec<u8>> {
    if path_or_url.starts_with("http://") || path_or_url.starts_with("https://") {
        let resp = reqwest::get(path_or_url).await?.error_for_status()?;
        Ok(resp.bytes().await?.to_vec())
    } else {
        Ok(fs::read(path_or_url).await?)
    }
}

pub async fn cache_bytes(
    root: &Path,
    channel: &str,
    name: &str,
    bytes: &[u8],
) -> anyhow::Result<PathBuf> {
    let hash = hex(bytes);
    let safe = sanitize(name);
    let dir = root.join(channel).join(&hash[..16]);
    fs::create_dir_all(&dir).await?;
    let path = dir.join(safe);
    fs::write(&path, bytes).await?;
    Ok(path)
}

pub async fn render_markdown_table_png(root: &Path, table: &str) -> anyhow::Result<PathBuf> {
    fs::create_dir_all(root).await?;
    let path = root.join(format!("{}.png", &hex(table.as_bytes())[..16]));
    let bytes = tokio::task::spawn_blocking({
        let table = table.to_string();
        move || render_table_png_bytes(&table)
    })
    .await??;
    fs::write(&path, bytes).await?;
    Ok(path)
}

fn render_table_png_bytes(table: &str) -> anyhow::Result<Vec<u8>> {
    let rows = markdown::parse_table_rows(table);
    if rows.is_empty() {
        return Err(anyhow!("markdown table has no rows"));
    }
    let cols = rows.iter().map(Vec::len).max().unwrap_or(0);
    let rows: Vec<Vec<String>> = rows
        .into_iter()
        .map(|mut row| {
            row.resize(cols, String::new());
            row
        })
        .collect();
    let col_widths: Vec<u32> = (0..cols)
        .map(|i| {
            rows.iter()
                .map(|row| display_width(row[i].as_str()) as usize)
                .max()
                .unwrap_or(0)
                .clamp(4, 28) as u32
                * 14
                + 36
        })
        .collect();
    let row_h = 56u32;
    let pad = 24u32;
    let width = col_widths.iter().sum::<u32>() + pad * 2;
    let height = row_h * rows.len() as u32 + pad * 2;
    let svg = table_svg(&rows, &col_widths, width, height, row_h, pad);

    let mut opt = usvg::Options::default();
    opt.fontdb_mut().load_system_fonts();
    opt.font_family = "sans-serif".into();
    let tree = usvg::Tree::from_str(&svg, &opt)?;
    let size = tree.size();
    let mut pixmap = tiny_skia::Pixmap::new(size.width() as u32, size.height() as u32)
        .context("create table pixmap")?;
    pixmap.fill(tiny_skia::Color::WHITE);
    resvg::render(&tree, tiny_skia::Transform::default(), &mut pixmap.as_mut());
    Ok(pixmap.encode_png()?)
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

fn escape_xml(s: &str) -> String {
    s.chars()
        .flat_map(|c| match c {
            '&' => "&amp;".chars().collect::<Vec<_>>(),
            '<' => "&lt;".chars().collect(),
            '>' => "&gt;".chars().collect(),
            '"' => "&quot;".chars().collect(),
            _ => vec![c],
        })
        .collect()
}

pub async fn ffmpeg_convert(input: &Path, output: &Path, args: &[&str]) -> anyhow::Result<()> {
    let status = Command::new("ffmpeg")
        .arg("-y")
        .arg("-i")
        .arg(input)
        .args(args)
        .arg(output)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .await
        .context("run ffmpeg")?;
    if status.success() {
        Ok(())
    } else {
        Err(anyhow!("ffmpeg exited with {status}"))
    }
}

fn sanitize(s: &str) -> String {
    let out: String = s
        .chars()
        .map(|c| {
            if matches!(c, '/' | '\\' | ':' | '\0') {
                '_'
            } else {
                c
            }
        })
        .collect();
    if out.trim().is_empty() {
        "media.bin".into()
    } else {
        out
    }
}
fn hex(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn cache_uses_local_path() {
        let dir = tempfile::tempdir().unwrap();
        let p = cache_bytes(dir.path(), "telegram", "a/b.txt", b"x")
            .await
            .unwrap();
        assert!(p.exists());
        assert!(p.to_string_lossy().contains("telegram"));
        assert!(p.ends_with("a_b.txt"));
    }

    #[tokio::test]
    async fn renders_markdown_table_png() {
        let dir = tempfile::tempdir().unwrap();
        let out = render_markdown_table_png(
            dir.path(),
            "| 渠道 | 状态 |\n| --- | --- |\n| Telegram | HTML 富文本 |",
        )
        .await
        .unwrap();
        let bytes = std::fs::read(out).unwrap();
        assert!(bytes.starts_with(b"\x89PNG\r\n\x1a\n"));
        assert!(bytes.len() > 1000);
    }
}
