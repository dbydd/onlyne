//! Feishu credential resolution for the gateway plugin (no network).
//!
//! Mirrors the legacy daemon: `FEISHU_APP_ID` / `FEISHU_APP_SECRET` plus an
//! optional `FEISHU_DOMAIN` override. Missing values surface as
//! `AdapterError::Code { code: ErrorCode::Unauthorized, .. }` whose message
//! names the absent env var, so the host can report
//! `fault{kind:"gateway_unconfigured"}` instead of panicking (S10.4).

use onlyne_adapter::AdapterError;
use onlyne_proto::ErrorCode;

pub const APP_ID_ENV: &str = "FEISHU_APP_ID";
pub const APP_SECRET_ENV: &str = "FEISHU_APP_SECRET";
pub const DOMAIN_ENV: &str = "FEISHU_DOMAIN";
pub const DEFAULT_DOMAIN: &str = "https://open.feishu.cn";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FeishuCredentials {
    pub app_id: String,
    pub app_secret: String,
    pub domain: String,
}

impl FeishuCredentials {
    pub fn new(
        app_id: impl Into<String>,
        app_secret: impl Into<String>,
        domain: impl Into<String>,
    ) -> Self {
        Self {
            app_id: app_id.into(),
            app_secret: app_secret.into(),
            domain: domain.into(),
        }
    }
    pub fn base_url(&self) -> String {
        self.domain.trim_end_matches('/').to_string()
    }

    pub fn from_env() -> Result<Self, AdapterError> {
        credentials_from_env()
    }
}

/// Missing-credential error naming the exact env var.
pub fn missing_credential_error(env: &str) -> AdapterError {
    AdapterError::new(
        ErrorCode::Unauthorized,
        format!("feishu missing credential: set {env}"),
    )
}

fn read_env(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

/// Resolve credentials from explicit values, falling back to the process env.
///
/// Explicit non-empty values win; otherwise `FEISHU_APP_ID` /
/// `FEISHU_APP_SECRET` are read, with `FEISHU_DOMAIN` (defaulting to
/// `https://open.feishu.cn`) as the API base.
pub fn resolve_credentials(
    app_id: Option<&str>,
    app_secret: Option<&str>,
    domain: Option<&str>,
) -> Result<FeishuCredentials, AdapterError> {
    let app_id = app_id
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(str::to_string)
        .or_else(|| read_env(APP_ID_ENV))
        .ok_or_else(|| missing_credential_error(APP_ID_ENV))?;
    let app_secret = app_secret
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(str::to_string)
        .or_else(|| read_env(APP_SECRET_ENV))
        .ok_or_else(|| missing_credential_error(APP_SECRET_ENV))?;
    let domain = domain
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(str::to_string)
        .or_else(|| read_env(DOMAIN_ENV))
        .unwrap_or_else(|| DEFAULT_DOMAIN.to_string());
    Ok(FeishuCredentials::new(app_id, app_secret, domain))
}

/// Convenience for `FeishuPlugin::from_env()`.
pub fn credentials_from_env() -> Result<FeishuCredentials, AdapterError> {
    resolve_credentials(None, None, None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};

    /// Serialize the env-touching tests without `std::sync::Mutex` (which
    /// would need an `unwrap()` on the lock result).
    static ENV_SERIAL: AtomicBool = AtomicBool::new(false);

    struct EnvGuard;

    fn lock_env() -> EnvGuard {
        while ENV_SERIAL
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            std::thread::yield_now();
        }
        EnvGuard
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            ENV_SERIAL.store(false, Ordering::SeqCst);
        }
    }

    fn clear_env() {
        for name in [APP_ID_ENV, APP_SECRET_ENV, DOMAIN_ENV] {
            unsafe { std::env::remove_var(name) };
        }
    }

    #[test]
    fn explicit_values_win_over_env() {
        let _guard = lock_env();
        clear_env();
        let creds = resolve_credentials(Some("id"), Some("secret"), Some("https://x/"))
            .expect("explicit creds");
        assert_eq!(creds.app_id, "id");
        assert_eq!(creds.app_secret, "secret");
        assert_eq!(creds.base_url(), "https://x");
    }

    #[test]
    fn missing_app_id_names_env() {
        let _guard = lock_env();
        clear_env();
        let err = resolve_credentials(None, Some("secret"), None).unwrap_err();
        assert_eq!(err.code(), Some(ErrorCode::Unauthorized));
        assert!(err.to_string().contains(APP_ID_ENV), "{err}");
    }

    #[test]
    fn missing_app_secret_names_env() {
        let _guard = lock_env();
        clear_env();
        unsafe { std::env::set_var(APP_ID_ENV, "id") };
        let err = resolve_credentials(None, None, None).unwrap_err();
        assert_eq!(err.code(), Some(ErrorCode::Unauthorized));
        assert!(err.to_string().contains(APP_SECRET_ENV), "{err}");
        clear_env();
    }
}
