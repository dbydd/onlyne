use crate::kit::error::KitError;
use aes_gcm::{
    Aes256Gcm, Nonce,
    aead::{Aead, KeyInit},
};
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use chrono::{DateTime, Utc};
use qrcode::{QrCode, render::unicode};
use serde::{Deserialize, Serialize};
use std::{
    env, fmt,
    fs::{self, OpenOptions},
    io::Write,
    path::Path,
    time::Duration,
};
use tokio::time::{Instant, sleep};

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Credential {
    pub token: String,
    pub expiry: Option<DateTime<Utc>>,
}

impl fmt::Debug for Credential {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Credential")
            .field("token", &"***")
            .field("expiry", &self.expiry)
            .finish()
    }
}

impl Credential {
    pub fn new(token: impl Into<String>, expiry: Option<DateTime<Utc>>) -> Self {
        Self {
            token: token.into(),
            expiry,
        }
    }

    pub fn bearer(token: impl Into<String>) -> Self {
        Self::new(token, None)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScanState {
    Pending,
    Scanned,
    Confirmed {
        token: Secret,
        expiry: DateTime<Utc>,
    },
    Expired,
    Denied(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProbeError {
    detail: String,
}

impl ProbeError {
    pub fn new(detail: impl Into<String>) -> Self {
        Self {
            detail: detail.into(),
        }
    }
}

impl fmt::Display for ProbeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.detail)
    }
}

impl std::error::Error for ProbeError {}

#[allow(async_fn_in_trait)]
pub trait Probe {
    async fn poll(&self, token: &str) -> Result<ScanState, ProbeError>;
}

pub fn render_login_qr_ascii(uri: &str) -> Result<String, KitError> {
    let code = QrCode::new(uri.as_bytes())
        .map_err(|err| KitError::render_failure("qr", err.to_string()))?;
    Ok(code.render::<unicode::Dense1x2>().quiet_zone(false).build())
}

pub async fn poll_for_scan<P: Probe>(
    probe: &P,
    token: &str,
    timeout: Duration,
    interval: Duration,
) -> Result<Credential, KitError> {
    let deadline = Instant::now() + timeout;
    loop {
        match probe
            .poll(token)
            .await
            .map_err(|err| KitError::Unsupported(format!("QR probe failed: {err}")))?
        {
            ScanState::Pending | ScanState::Scanned => {}
            ScanState::Confirmed { token, expiry } => {
                return Ok(Credential::new(token, Some(expiry)));
            }
            ScanState::Expired => return Err(KitError::Unsupported("QR login expired".into())),
            ScanState::Denied(reason) => {
                return Err(KitError::Unsupported(format!("QR login denied: {reason}")));
            }
        }
        if Instant::now() >= deadline {
            return Err(KitError::Unsupported(
                "timed out waiting for QR scan".into(),
            ));
        }
        sleep(interval).await;
    }
}

pub async fn complete_onboarding<P: Probe>(
    probe: &P,
    token: &str,
    credential_path: &Path,
    timeout: Duration,
    interval: Duration,
) -> Result<Credential, KitError> {
    let credential = poll_for_scan(probe, token, timeout, interval).await?;
    write_credential_file(credential_path, &credential)?;
    Ok(credential)
}

pub fn write_credential_file(path: &Path, credential: &Credential) -> Result<(), KitError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let bytes = serde_json::to_vec_pretty(credential)
        .map_err(|err| KitError::Unsupported(format!("serialize credential: {err}")))?;
    write_secret_file(path, &bytes)
}

pub fn load_credential(source: &str) -> Result<Credential, KitError> {
    let (resolved, came_from_env) = resolve_source(source)?;
    if resolved.trim_start().starts_with('{') || came_from_env && !Path::new(&resolved).exists() {
        return parse_credential_text(&resolved);
    }
    let text = fs::read_to_string(Path::new(&resolved))?;
    parse_credential_text(&text)
}

pub fn resolve_env_value(raw: &str) -> Result<String, KitError> {
    resolve_source(raw).map(|(value, _)| value)
}

/// Gateway v1 reads key bytes from `<server-root>/.onlyne/keys/` and passes them into this helper.
pub fn decrypt_aes256_gcm_base64(encrypted_secret: &str, key: &[u8]) -> Result<Secret, KitError> {
    if key.len() != 32 {
        return Err(KitError::Unsupported(
            "AES-256-GCM key must be 32 bytes".into(),
        ));
    }
    let raw = BASE64.decode(encrypted_secret).map_err(|err| {
        KitError::Unsupported(format!("credential blob base64 decode failed: {err}"))
    })?;
    if raw.len() <= 28 {
        return Err(KitError::Unsupported(
            "credential blob AES-GCM payload is too short".into(),
        ));
    }
    let nonce_bytes: [u8; 12] = raw[..12]
        .try_into()
        .map_err(|_| KitError::Unsupported("credential blob nonce is invalid".into()))?;
    let nonce = Nonce::from(nonce_bytes);
    let ciphertext_and_tag = &raw[12..];
    let cipher = Aes256Gcm::new_from_slice(key)
        .map_err(|_| KitError::Unsupported("credential blob AES-GCM key is invalid".into()))?;
    let plain = cipher
        .decrypt(&nonce, ciphertext_and_tag)
        .map_err(|_| KitError::Unsupported("credential blob decrypt failed".into()))?;
    let token = String::from_utf8(plain)
        .map_err(|err| KitError::Unsupported(format!("credential blob is not utf-8: {err}")))?;
    Ok(Secret::new(token))
}

fn resolve_source(raw: &str) -> Result<(String, bool), KitError> {
    let value = raw.trim();
    if let Some(name) = value.strip_prefix('$') {
        if name.trim().is_empty() {
            return Err(KitError::Unsupported(
                "empty environment variable reference".into(),
            ));
        }
        let resolved = env::var(name).map_err(|_| {
            KitError::Unsupported(format!("environment variable {name} is not set"))
        })?;
        Ok((resolved, true))
    } else {
        Ok((value.to_string(), false))
    }
}

fn parse_credential_text(text: &str) -> Result<Credential, KitError> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Err(KitError::Unsupported("credential source is empty".into()));
    }
    if trimmed.starts_with('{') {
        serde_json::from_str::<Credential>(trimmed)
            .map_err(|err| KitError::Unsupported(format!("parse credential JSON: {err}")))
    } else {
        Ok(Credential::bearer(trimmed))
    }
}

fn write_secret_file(path: &Path, bytes: &[u8]) -> Result<(), KitError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
        let mut file = OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .mode(0o600)
            .open(path)?;
        file.write_all(bytes)?;
        file.write_all(b"\n")?;
        file.sync_all()?;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
        Ok(())
    }
    #[cfg(not(unix))]
    {
        let mut file = OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(path)?;
        file.write_all(bytes)?;
        file.write_all(b"\n")?;
        file.sync_all()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{collections::VecDeque, sync::Mutex};

    struct ScriptedProbe {
        states: Mutex<VecDeque<ScanState>>,
    }

    impl ScriptedProbe {
        fn new(states: impl IntoIterator<Item = ScanState>) -> Self {
            Self {
                states: Mutex::new(states.into_iter().collect()),
            }
        }
    }

    impl Probe for ScriptedProbe {
        async fn poll(&self, _token: &str) -> Result<ScanState, ProbeError> {
            self.states
                .lock()
                .unwrap()
                .pop_front()
                .ok_or_else(|| ProbeError::new("script exhausted"))
        }
    }

    #[tokio::test]
    async fn scripted_probe_writes_credential_file_with_private_mode() {
        let expiry = DateTime::from_timestamp(1_893_456_000, 0).unwrap();
        let probe = ScriptedProbe::new([
            ScanState::Pending,
            ScanState::Scanned,
            ScanState::Confirmed {
                token: Secret::new("confirmed-token"),
                expiry,
            },
        ]);
        let dir = tempfile::tempdir().unwrap();
        let credential_path = dir.path().join("credential.json");
        let credential = complete_onboarding(
            &probe,
            "login-token",
            &credential_path,
            Duration::from_secs(1),
            Duration::ZERO,
        )
        .await
        .unwrap();
        assert_eq!(credential.token.as_str(), "confirmed-token");
        let loaded = load_credential(credential_path.to_str().unwrap()).unwrap();
        assert_eq!(loaded.token.as_str(), "confirmed-token");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&credential_path)
                .unwrap()
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(mode, 0o600);
        }
    }

    #[tokio::test]
    async fn denied_scan_surfaces_reason() {
        let probe = ScriptedProbe::new([ScanState::Denied("operator rejected login".into())]);
        let err = poll_for_scan(
            &probe,
            "login-token",
            Duration::from_secs(1),
            Duration::ZERO,
        )
        .await
        .unwrap_err();
        assert!(err.to_string().contains("operator rejected login"));
    }

    #[test]
    fn env_indirection_loads_token_without_debug_leak() {
        let name = format!("ONLYNE_GATEWAY_KIT_TOKEN_{}", std::process::id());
        let secret = "env-supplied-secret-token";
        unsafe {
            env::set_var(&name, secret);
        }
        let credential = load_credential(&format!("${name}")).unwrap();
        unsafe {
            env::remove_var(&name);
        }
        assert_eq!(credential.token.as_str(), secret);
        let debug = format!("{credential:?}");
        assert!(!debug.contains(secret));
        assert!(debug.contains("***"));
    }

    #[test]
    fn qr_renderer_returns_terminal_block() {
        let block = render_login_qr_ascii("https://example.com/login").unwrap();
        assert!(!block.trim().is_empty());
        assert!(block.lines().count() > 1);
    }
}
