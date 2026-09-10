//! Telegram credential resolution kept local to the platform plugin.
//!
//! The gateway host deliberately does not own platform secrets.  This module
//! accepts either a literal bot token or a `$VARIABLE` reference and keeps the
//! missing-variable error explicit for operator diagnostics.

use onlyne_adapter::AdapterError;
use onlyne_proto::ErrorCode;
use std::env;

pub const TELEGRAM_TOKEN_ENV: &str = "TELEGRAM_BOT_TOKEN";

/// Credentials needed by the Telegram Bot API.
#[derive(Clone, PartialEq, Eq)]
pub struct TelegramCredentials {
    token: String,
}

impl std::fmt::Debug for TelegramCredentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TelegramCredentials")
            .field("token", &"***")
            .finish()
    }
}

impl TelegramCredentials {
    pub fn token(&self) -> &str {
        &self.token
    }

    pub fn from_env() -> Result<Self, AdapterError> {
        Ok(Self {
            token: resolve_token(Some(TELEGRAM_TOKEN_ENV))?,
        })
    }

    pub fn from_source(source: Option<&str>) -> Result<Self, AdapterError> {
        Ok(Self {
            token: resolve_token(source)?,
        })
    }
}

/// Resolve a literal token, `$ENV_NAME`, or the conventional Telegram env.
pub fn resolve_token(source: Option<&str>) -> Result<String, AdapterError> {
    let raw = source.unwrap_or(TELEGRAM_TOKEN_ENV).trim();
    let env_name = raw.strip_prefix('$').unwrap_or(raw);
    if raw.is_empty() {
        return Err(missing_token_error(TELEGRAM_TOKEN_ENV));
    }
    if raw.starts_with('$') || raw == TELEGRAM_TOKEN_ENV {
        return env::var(env_name).map_err(|_| missing_token_error(env_name));
    }
    Ok(raw.to_owned())
}

/// Construct the stable error used when no Telegram secret is configured.
pub fn missing_token_error(env_name: &str) -> AdapterError {
    AdapterError::new(
        ErrorCode::Unauthorized,
        format!("telegram credentials missing: environment variable {env_name} is not set"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn literal_token_is_not_treated_as_an_env_name() {
        assert_eq!(resolve_token(Some("123:abc")).unwrap(), "123:abc");
    }

    #[test]
    fn missing_env_error_names_variable() {
        let err = resolve_token(Some("$ONLYNE_TELEGRAM_MISSING_TEST"))
            .expect_err("unset token should fail");
        assert!(err.to_string().contains("ONLYNE_TELEGRAM_MISSING_TEST"));
    }
}
