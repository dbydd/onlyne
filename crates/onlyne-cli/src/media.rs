//! Inline image loading: magic-byte mime detection with an extension fallback.

use base64::Engine;
use onlyne_proto::{ImagePart, IMAGE_DATA_MAX_BYTES, IMAGE_MIMES};
use std::path::Path;

/// A local failure to attach an image.
#[derive(Debug)]
pub enum MediaError {
    /// The file could not be read.
    Read(std::io::Error),
    /// The file crossed the 2 MiB decoded ceiling.
    TooLarge,
    /// Neither the magic bytes nor the extension named an accepted mime type.
    Unsupported(String),
}

impl MediaError {
    /// The byte-exact stderr message for this failure.
    pub fn message(&self) -> String {
        match self {
            MediaError::Read(error) => {
                format!("onlyne: cannot read image: {error}")
            }
            MediaError::TooLarge => {
                format!("onlyne: image exceeds {IMAGE_DATA_MAX_BYTES} bytes")
            }
            MediaError::Unsupported(guess) => {
                format!(
                    "onlyne: unsupported image mime {guess}; allowed: {}",
                    IMAGE_MIMES.join(", ")
                )
            }
        }
    }
}

/// Mime type of a file's leading bytes, when they match a known image signature.
pub fn mime_from_magic(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(&[0x89, b'P', b'I', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]) {
        return Some("image/png");
    }
    if bytes.len() >= 3 && bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        return Some("image/jpeg");
    }
    if bytes.starts_with(b"GIF8") {
        return Some("image/gif");
    }
    if bytes.len() >= 12 && bytes.starts_with(b"RIFF") && &bytes[8..12] == b"WEBP" {
        return Some("image/webp");
    }
    None
}

/// Mime type guessed from a file extension.
pub fn mime_from_extension(path: &Path) -> String {
    match path
        .extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| ext.to_ascii_lowercase())
        .as_deref()
    {
        Some("png") => "image/png".to_string(),
        Some("jpg") | Some("jpeg") => "image/jpeg".to_string(),
        Some("gif") => "image/gif".to_string(),
        Some("webp") => "image/webp".to_string(),
        Some(other) => format!("image/{other}"),
        None => "image/unknown".to_string(),
    }
}

/// Read an image file, detect its mime, and validate it against the protocol.
pub fn load_image_part(path: &Path) -> Result<ImagePart, MediaError> {
    let bytes = std::fs::read(path).map_err(MediaError::Read)?;
    if bytes.len() > IMAGE_DATA_MAX_BYTES {
        return Err(MediaError::TooLarge);
    }
    let guess = mime_from_extension(path);
    let mime = mime_from_magic(&bytes).unwrap_or(&guess);
    if !IMAGE_MIMES.contains(&mime) {
        return Err(MediaError::Unsupported(guess));
    }
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .map(|name| name.to_string());
    Ok(ImagePart {
        data_base64: base64::engine::general_purpose::STANDARD.encode(&bytes),
        mime: mime.to_string(),
        name,
    })
}
