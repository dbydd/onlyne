use anyhow::{Context, anyhow};
use sha2::{Digest, Sha256};
use std::{
    path::{Path, PathBuf},
    process::Stdio,
};
use tokio::{fs, process::Command};

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
}
