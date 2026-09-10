//! Credential resolution and local onboarding surface for the Weixin gateway.
//!
//! The gateway deliberately keeps credentials out of the protocol envelope.  A
//! configured value may be a literal token or a `$NAME` environment reference;
//! when no value is supplied the stable `WEIXIN_ILINK_TOKEN` name is used.

use onlyne_adapter::{AdapterError, OnboardingKind, OnboardingPrompt};
use onlyne_proto::ErrorCode;
use std::env;

/// The default iLink API endpoint used by the Weixin SDK.
pub const DEFAULT_BASE_URL: &str = "https://ilinkai.weixin.qq.com";
/// Stable environment variable name used by generated Onlyne config.
pub const TOKEN_ENV: &str = "WEIXIN_ILINK_TOKEN";

fn credential_error(env_name: &str) -> AdapterError {
    AdapterError::new(
        ErrorCode::Unauthorized,
        format!("weixin credential missing: set {env_name}"),
    )
}

/// Resolve a token with an injectable environment lookup.
///
/// This small pure seam keeps unit tests deterministic and makes the exact
/// missing-environment wording part of the plugin contract.
pub fn resolve_token_from<F>(
    configured: Option<&str>,
    explicit_env: Option<&str>,
    lookup: F,
) -> Result<String, AdapterError>
where
    F: Fn(&str) -> Option<String>,
{
    let configured = configured.map(str::trim).filter(|value| !value.is_empty());
    let env_name = explicit_env
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .or_else(|| {
            configured
                .and_then(|value| value.strip_prefix('$'))
                .filter(|value| !value.trim().is_empty())
        })
        .unwrap_or(TOKEN_ENV);

    if let Some(value) = configured {
        if let Some(name) = value.strip_prefix('$') {
            return lookup(name.trim())
                .map(|token| token.trim().to_string())
                .filter(|token| !token.is_empty())
                .ok_or_else(|| credential_error(name.trim()));
        }
        if explicit_env.is_none() {
            return Ok(value.to_string());
        }
    }

    lookup(env_name)
        .map(|token| token.trim().to_string())
        .filter(|token| !token.is_empty())
        .ok_or_else(|| credential_error(env_name))
}

/// Resolve a token against the process environment.
pub fn resolve_token(
    configured: Option<&str>,
    explicit_env: Option<&str>,
) -> Result<String, AdapterError> {
    resolve_token_from(configured, explicit_env, |name| env::var(name).ok())
}

/// Construct the non-network onboarding prompt for a tokenless installation.
///
/// The host can use this prompt to start the SDK's QR flow.  Calling this
/// function never contacts Weixin and never prints or persists credentials.
pub fn qr_onboarding_prompt() -> OnboardingPrompt {
    OnboardingPrompt {
        kind: OnboardingKind::Qr,
        payload: format!(
            "Scan a Weixin iLink QR code, or set {TOKEN_ENV} before starting the gateway"
        ),
        expires_in: Some(300),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn literal_token_wins_without_network_or_environment() {
        let token = resolve_token_from(Some("literal"), None, |_| None).expect("literal token");
        assert_eq!(token, "literal");
    }

    #[test]
    fn dollar_token_reports_the_exact_missing_environment_name() {
        let err = resolve_token_from(Some("$WEIXIN_TEST_TOKEN"), None, |_| None)
            .expect_err("missing token must fail");
        assert!(err.to_string().contains("WEIXIN_TEST_TOKEN"));
    }

    #[test]
    fn explicit_environment_name_is_honoured() {
        let mut vars = HashMap::new();
        vars.insert("CUSTOM_WEIXIN_TOKEN".to_string(), "secret".to_string());
        let token = resolve_token_from(None, Some("CUSTOM_WEIXIN_TOKEN"), |name| {
            vars.get(name).cloned()
        })
        .expect("custom environment token");
        assert_eq!(token, "secret");
    }

    #[test]
    fn onboarding_is_a_local_qr_prompt() {
        let prompt = qr_onboarding_prompt();
        assert_eq!(prompt.kind, OnboardingKind::Qr);
        assert!(prompt.payload.contains(TOKEN_ENV));
        assert_eq!(prompt.expires_in, Some(300));
    }
}
