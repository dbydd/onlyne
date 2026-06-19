use crate::config::RendererConfig;
use anyhow::{Context, anyhow};
use sha2::{Digest, Sha256};
use std::{
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};
use tokio::{fs, io::AsyncWriteExt, process::Command, time::timeout};

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

pub async fn render_markdown_png(
    cfg: &RendererConfig,
    root: &Path,
    markdown: &str,
    max_bytes: u64,
) -> anyhow::Result<PathBuf> {
    if !cfg.enabled {
        return Err(anyhow!("markdown renderer disabled"));
    }
    fs::create_dir_all(root).await?;
    let path = root.join(format!("{}.png", &hex(markdown.as_bytes())[..16]));
    let args: Vec<String> = cfg
        .args
        .iter()
        .map(|a| a.replace("{output}", &path.to_string_lossy()))
        .collect();
    let mut child = Command::new(&cfg.command)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .with_context(|| format!("run markdown renderer {}", cfg.command))?;
    let mut stdin = child.stdin.take().context("open renderer stdin")?;
    stdin.write_all(markdown.as_bytes()).await?;
    drop(stdin);
    let status = timeout(Duration::from_secs(cfg.timeout_seconds), child.wait())
        .await
        .context("markdown renderer timed out")??;
    if !status.success() {
        return Err(anyhow!("markdown renderer exited with {status}"));
    }
    let meta = fs::metadata(&path)
        .await
        .with_context(|| format!("renderer did not write {}", path.display()))?;
    if meta.len() > max_bytes {
        return Err(anyhow!(
            "rendered image too large: {} > {} bytes",
            meta.len(),
            max_bytes
        ));
    }
    Ok(path)
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
    async fn renderer_gets_stdin_and_output_arg() {
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("renderer.sh");
        std::fs::write(
            &script,
            "#!/bin/sh\nwhile [ \"$1\" != \"--out\" ]; do shift; done\nout=$2\ncat >/dev/null\nprintf '\\211PNG\\r\\n\\032\\n' > \"$out\"\n",
        )
        .unwrap();
        let _ = std::process::Command::new("chmod")
            .arg("+x")
            .arg(&script)
            .status();
        let cfg = RendererConfig {
            enabled: true,
            command: script.to_string_lossy().to_string(),
            args: vec!["--out".into(), "{output}".into()],
            timeout_seconds: 5,
        };
        let out = render_markdown_png(&cfg, dir.path(), "# hi", 100)
            .await
            .unwrap();
        assert!(out.exists());
    }
}
