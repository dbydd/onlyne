//! Credential resolution for the QQ Open Platform gateway.
//!
//! This module deliberately does not perform QR or network authentication.  The
//! host can surface [`onboarding_prompt`] and let an operator provision the two
//! documented environment variables before starting the gateway.

use onlyne_adapter::{AdapterError, OnboardingKind, OnboardingPrompt};

pub const APP_ID_ENV: &str = "QQBOT_APP_ID";
pub const APP_SECRET_ENV: &str = "QQBOT_APP_SECRET";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QqBotCredentials {
    pub app_id: String,
    pub app_secret: String,
}

impl QqBotCredentials {
    /// Resolve credentials from the process environment.
    pub fn from_env() -> Result<Self, AdapterError> {
        Self::from_values(
            std::env::var(APP_ID_ENV).ok(),
            std::env::var(APP_SECRET_ENV).ok(),
        )
    }

    /// Resolve optional configured values, using the environment when a value
    /// is absent. Empty values are treated as absent so the resulting error
    /// names the variable an operator must set.
    pub fn from_values(
        app_id: Option<String>,
        app_secret: Option<String>,
    ) -> Result<Self, AdapterError> {
        let app_id = app_id
            .or_else(|| std::env::var(APP_ID_ENV).ok())
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| missing_credential(APP_ID_ENV))?;
        let app_secret = app_secret
            .or_else(|| std::env::var(APP_SECRET_ENV).ok())
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| missing_credential(APP_SECRET_ENV))?;
        Ok(Self { app_id, app_secret })
    }
}

fn missing_credential(name: &'static str) -> AdapterError {
    AdapterError::Unexpected(format!(
        "qqbot credential missing: set {name} in the gateway environment"
    ))
}

/// Operator-facing onboarding information. The host may render this without
/// contacting QQ, which keeps startup deterministic in an unconfigured setup.
pub fn onboarding_prompt() -> OnboardingPrompt {
    OnboardingPrompt {
        kind: OnboardingKind::ManualCode,
        payload: format!(
            "Set {APP_ID_ENV} and {APP_SECRET_ENV} before starting onlyne-gateway-qqbot"
        ),
        expires_in: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_app_id_names_its_environment_variable() {
        let error = QqBotCredentials::from_values(None, Some("secret".into())).unwrap_err();
        assert!(error.to_string().contains(APP_ID_ENV));
    }

    #[test]
    fn missing_secret_names_its_environment_variable() {
        let error = QqBotCredentials::from_values(Some("app".into()), None).unwrap_err();
        assert!(error.to_string().contains(APP_SECRET_ENV));
    }

    #[test]
    fn prompt_is_network_free_and_names_both_variables() {
        let prompt = onboarding_prompt();
        assert_eq!(prompt.kind, OnboardingKind::ManualCode);
        assert!(prompt.payload.contains(APP_ID_ENV));
        assert!(prompt.payload.contains(APP_SECRET_ENV));
    }
}
