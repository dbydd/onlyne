use crate::kit::error::KitError;
use sha2::{Digest, Sha256};
use std::{
    ffi::OsString,
    io,
    path::{Path, PathBuf},
    process::Stdio,
};
use tokio::{fs, process::Command};

pub const MAX_IMAGE_BYTES: usize = 2 * 1024 * 1024;
/// The spec-level ffmpeg executable name used by the default helper.
pub const FFMPEG_PROGRAM: &str = "ffmpeg";

pub fn fits_image_budget(bytes: &[u8]) -> bool {
    bytes.len() <= MAX_IMAGE_BYTES
}

pub fn ensure_image_budget(bytes: &[u8]) -> Result<(), KitError> {
    if fits_image_budget(bytes) {
        Ok(())
    } else {
        Err(KitError::OversizedPayload {
            max: MAX_IMAGE_BYTES,
            actual: bytes.len(),
        })
    }
}

pub async fn cache_bytes(
    root: &Path,
    channel: &str,
    name: &str,
    bytes: &[u8],
) -> Result<PathBuf, KitError> {
    let hash = hex(bytes);
    let safe_channel = sanitize(channel);
    let safe_name = sanitize(name);
    let dir = root.join(safe_channel).join(&hash[..16]);
    fs::create_dir_all(&dir).await?;
    let path = dir.join(safe_name);
    match fs::try_exists(&path).await {
        Ok(true) => return Ok(path),
        Ok(false) => {}
        Err(err) => return Err(KitError::Io(err)),
    }
    fs::write(&path, bytes).await?;
    Ok(path)
}

/// Converts media with the caller-owned program name.
pub async fn ffmpeg_convert(
    program: &str,
    input: &Path,
    output: &Path,
    args: &[&str],
) -> Result<(), KitError> {
    ffmpeg_convert_with_program(program, input, output, args).await
}

pub async fn ffmpeg_convert_default(
    input: &Path,
    output: &Path,
    args: &[&str],
) -> Result<(), KitError> {
    ffmpeg_convert_with_program(FFMPEG_PROGRAM, input, output, args).await
}

/// The exact argument vector handed to the conversion program.
///
/// Order is part of the contract: overwrite, input, caller filters, output.
pub fn ffmpeg_args(input: &Path, args: &[&str], output: &Path) -> Vec<OsString> {
    let mut argv = vec![
        OsString::from("-y"),
        OsString::from("-i"),
        input.as_os_str().to_os_string(),
    ];
    argv.extend(args.iter().map(OsString::from));
    argv.push(output.as_os_str().to_os_string());
    argv
}

async fn ffmpeg_convert_with_program(
    program: &str,
    input: &Path,
    output: &Path,
    args: &[&str],
) -> Result<(), KitError> {
    let status = Command::new(program)
        .args(ffmpeg_args(input, args, output))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .await
        .map_err(|err| match err.kind() {
            io::ErrorKind::NotFound => KitError::MissingProgram {
                program: program.to_string(),
            },
            _ => KitError::Io(err),
        })?;
    if status.success() {
        Ok(())
    } else {
        Err(KitError::Unsupported(format!(
            "{program} exited with {status}"
        )))
    }
}

pub fn sanitize(s: &str) -> String {
    let out: String = s
        .chars()
        .map(|c| {
            if matches!(c, '/' | '\\' | ':') || c.is_control() {
                '_'
            } else {
                c
            }
        })
        .collect();
    let trimmed = out.trim();
    if trimmed.is_empty() || trimmed == "." || trimmed == ".." {
        "media.bin".into()
    } else {
        trimmed.to_string()
    }
}

fn hex(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn cache_bytes_writes_once_and_reuses_path() {
        let dir = tempfile::tempdir().unwrap();
        let p1 = cache_bytes(dir.path(), "telegram", "a/b.txt", b"x")
            .await
            .unwrap();
        let first_modified = std::fs::metadata(&p1).unwrap().modified().unwrap();
        let p2 = cache_bytes(dir.path(), "telegram", "a/b.txt", b"x")
            .await
            .unwrap();
        let second_modified = std::fs::metadata(&p2).unwrap().modified().unwrap();
        assert_eq!(p1, p2);
        assert_eq!(first_modified, second_modified);
        assert_eq!(std::fs::read(&p2).unwrap(), b"x");
    }

    #[test]
    fn sanitize_strips_separators_and_control_bytes() {
        let out = sanitize("../a\\b:c\0\n.png");
        assert!(!out.contains('/'));
        assert!(!out.contains('\\'));
        assert!(!out.contains(':'));
        assert!(!out.contains('\0'));
        assert!(!out.contains('\n'));
        assert!(out.ends_with(".png"));
    }

    #[test]
    fn image_budget_boundary_accepts_exact_ceiling() {
        let exact = vec![0u8; MAX_IMAGE_BYTES];
        let above = vec![0u8; MAX_IMAGE_BYTES + 1];
        assert!(fits_image_budget(&exact));
        assert!(!fits_image_budget(&above));
        assert!(ensure_image_budget(&exact).is_ok());
        assert!(matches!(
            ensure_image_budget(&above),
            Err(KitError::OversizedPayload {
                max: MAX_IMAGE_BYTES,
                actual
            }) if actual == MAX_IMAGE_BYTES + 1
        ));
    }
    #[test]
    fn ffmpeg_argv_pins_program_argument_order() {
        let argv = ffmpeg_args(
            Path::new("in.raw"),
            &["-frames:v", "1"],
            Path::new("out.png"),
        );
        assert_eq!(
            argv,
            vec![
                OsString::from("-y"),
                OsString::from("-i"),
                OsString::from("in.raw"),
                OsString::from("-frames:v"),
                OsString::from("1"),
                OsString::from("out.png"),
            ]
        );
    }

    #[tokio::test]
    async fn ffmpeg_convert_reports_missing_program() {
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("input.raw");
        let output = dir.path().join("output.raw");
        std::fs::write(&input, b"data").unwrap();
        let err = ffmpeg_convert("onlyne-ffmpeg-missing-for-test", &input, &output, &[])
            .await
            .unwrap_err();
        assert!(matches!(
            err,
            KitError::MissingProgram { ref program }
                if program == "onlyne-ffmpeg-missing-for-test"
        ));
    }
}
