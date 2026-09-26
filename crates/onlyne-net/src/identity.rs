use base64::{Engine as _, engine::general_purpose::STANDARD};
use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};
use rand::rngs::OsRng;
use std::ffi::OsString;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::Path;

use crate::NetError;

pub const KEY_PREFIX: &str = "ed25519/";

#[derive(Clone)]
pub struct KeyPair(SigningKey);

impl KeyPair {
    pub fn generate() -> Self {
        Self(SigningKey::generate(&mut OsRng))
    }

    pub fn from_seed(seed: [u8; 32]) -> Self {
        Self(SigningKey::from_bytes(&seed))
    }

    pub fn load(path: &Path) -> Result<Self, NetError> {
        let bytes = fs::read(path)?;
        if bytes.len() != 32 {
            return Err(NetError::MalformedKey(format!(
                "seed must be exactly 32 bytes, got {}",
                bytes.len()
            )));
        }
        let seed: [u8; 32] = bytes
            .try_into()
            .map_err(|_| NetError::MalformedKey("seed".to_string()))?;
        Ok(Self::from_seed(seed))
    }

    pub fn save(&self, path: &Path) -> Result<(), NetError> {
        write_private_atomic(path, &self.0.to_bytes())
    }

    pub fn public_str(&self) -> String {
        format!(
            "{KEY_PREFIX}{}",
            STANDARD.encode(self.0.verifying_key().to_bytes())
        )
    }

    pub fn sign(&self, message: &[u8]) -> String {
        STANDARD.encode(self.0.sign(message).to_bytes())
    }

    pub fn verifying_key(&self) -> VerifyingKey {
        self.0.verifying_key()
    }
}

pub fn parse_public(text: &str) -> Result<VerifyingKey, NetError> {
    let encoded = text.strip_prefix(KEY_PREFIX).ok_or_else(|| {
        NetError::MalformedKey("field key must use the ed25519/ prefix".to_string())
    })?;
    let bytes = STANDARD.decode(encoded).map_err(|_| {
        NetError::MalformedKey("field key must contain standard base64".to_string())
    })?;
    let bytes: [u8; 32] = bytes.try_into().map_err(|_| {
        NetError::MalformedKey("field key must decode to exactly 32 bytes".to_string())
    })?;
    VerifyingKey::from_bytes(&bytes).map_err(|error| {
        NetError::MalformedKey(format!(
            "field key is not a valid ed25519 public key: {error}"
        ))
    })
}

pub fn challenge_message(challenge: &[u8; 32], role: &str, protocol: u16) -> Vec<u8> {
    let mut message = Vec::with_capacity(9 + 32 + 1 + role.len() + 1 + 5);
    message.extend_from_slice(b"onlyne-v1\0");
    message.extend_from_slice(challenge);
    message.push(b'\n');
    message.extend_from_slice(role.as_bytes());
    message.push(b'\n');
    message.extend_from_slice(protocol.to_string().as_bytes());
    message
}

/// Replace `path` with `data` so that no reader ever sees a torn file or a
/// window at umask permissions.
///
/// The temp file lives in the target directory so the rename stays on one
/// filesystem, and is created with mode 0600 rather than chmodded afterward.
/// The directory fsync makes the rename itself survive a crash.
pub(crate) fn write_private_atomic(path: &Path, data: &[u8]) -> Result<(), NetError> {
    let io_error = |error: io::Error| NetError::Io(format!("{}: {error}", path.display()));
    let file_name = path
        .file_name()
        .ok_or_else(|| NetError::Io(format!("{}: not a file path", path.display())))?;
    let dir = match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent,
        _ => Path::new("."),
    };
    let mut temp_name = OsString::from(".");
    temp_name.push(file_name);
    temp_name.push(format!(".tmp.{}", std::process::id()));
    let temp_path = dir.join(temp_name);
    // A crash of an earlier process with the same pid can leave this name behind.
    match fs::remove_file(&temp_path) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(io_error(error)),
    }
    let written = write_new_private(&temp_path, data).and_then(|()| fs::rename(&temp_path, path));
    if let Err(error) = written {
        let _ = fs::remove_file(&temp_path);
        return Err(io_error(error));
    }
    sync_dir(dir).map_err(io_error)
}

fn write_new_private(path: &Path, data: &[u8]) -> io::Result<()> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    // Windows: key files live in the user profile directory and inherit its
    // ACL; there is no mode to set at creation.
    let mut file = options.open(path)?;
    file.write_all(data)?;
    file.sync_all()
}

fn sync_dir(dir: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        fs::File::open(dir)?.sync_all()
    }
    #[cfg(not(unix))]
    {
        // Directory handles cannot be fsynced through std on Windows; NTFS
        // journals the rename metadata.
        let _ = dir;
        Ok(())
    }
}

pub(crate) fn decode_signature(text: &str) -> Result<Signature, NetError> {
    let bytes = STANDARD
        .decode(text)
        .map_err(|_| NetError::Unauthorized("invalid signature encoding".to_string()))?;
    let bytes: [u8; 64] = bytes
        .try_into()
        .map_err(|_| NetError::Unauthorized("invalid signature length".to_string()))?;
    Ok(Signature::from_bytes(&bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn save_creates_owner_only_file_and_replaces_existing() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("identity.key");
        fs::write(&path, b"stale world-readable bytes").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        let key = KeyPair::from_seed([9; 32]);
        key.save(&path).unwrap();
        let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        assert_eq!(fs::read(&path).unwrap(), [9; 32]);
        let names: Vec<_> = fs::read_dir(dir.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(names, [OsString::from("identity.key")]);
    }
}
