use base64::{Engine as _, engine::general_purpose::STANDARD};
use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};
use rand::rngs::OsRng;
use std::fs;
use std::path::Path;

use crate::NetError;

pub const KEY_PREFIX: &str = "ed25519/";

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
        fs::write(path, self.0.to_bytes())?;
        set_private_mode(path)
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

fn set_private_mode(path: &Path) -> Result<(), NetError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = fs::metadata(path)?.permissions();
        permissions.set_mode(0o600);
        fs::set_permissions(path, permissions)?;
    }
    Ok(())
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
