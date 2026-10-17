//! Feishu credential resolution for the IM backend (no network).
//!
//! Ported from `frozen/gateway-v1`'s `plugins/onlyne-gateway-feishu/src/auth.rs`:
//! `FEISHU_APP_ID` / `FEISHU_APP_SECRET` plus an optional `FEISHU_DOMAIN`
//! override (default `https://open.feishu.cn`, Lark International
//! `https://open.larksuite.com`).
//!
//! The v2.3 backend protocol carries one credential string, so the pair the v1
//! code read as two variables is encoded as `app_id:app_secret`. A Feishu app
//! id is a constant-length `cli_`-prefixed string that cannot contain `:`, and
//! a secret is hex, so the first `:` is always the separator. A bare secret
//! with no `:` is not a valid credential and is refused with a sentence naming
//! the encoding, rather than being split silently.
//!
//! The domain is never part of the credential: an operator who needs Lark
//! International sets `FEISHU_DOMAIN`, exactly as in v1.

/// The app-id variable, named in the onboarding sentence.
pub const APP_ID_ENV: &str = "FEISHU_APP_ID";
/// The app-secret variable, named in the onboarding sentence.
pub const APP_SECRET_ENV: &str = "FEISHU_APP_SECRET";
/// The API domain override.
pub const DOMAIN_ENV: &str = "FEISHU_DOMAIN";
/// The API domain used when `FEISHU_DOMAIN` is unset.
pub const DEFAULT_DOMAIN: &str = "https://open.feishu.cn";

/// One Feishu custom app's identity: id, secret, and the API domain to call.
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

    /// The API root, without a trailing slash.
    pub fn base_url(&self) -> String {
        self.domain.trim_end_matches('/').to_string()
    }

    /// Encode the pair into the one credential string the backend protocol
    /// carries: `app_id:app_secret`.
    pub fn encode(&self) -> String {
        format!("{}:{}", self.app_id, self.app_secret)
    }
}

/// Split one `app_id:app_secret` credential string. The first `:` separates;
/// anything else is a sentence the operator reads, not a panic.
pub fn parse_credential(raw: &str) -> Result<FeishuCredentials, String> {
    let raw = raw.trim();
    let (app_id, app_secret) = raw
        .split_once(':')
        .ok_or_else(|| {
            format!(
                "the credential must be app_id:app_secret (create a Feishu custom app at {DEFAULT_DOMAIN}/app)"
            )
        })?;
    let app_id = app_id.trim();
    let app_secret = app_secret.trim();
    if app_id.is_empty() || app_secret.is_empty() {
        return Err(
            "the credential must be app_id:app_secret with both halves non-empty".to_string(),
        );
    }
    Ok(FeishuCredentials::new(app_id, app_secret, DEFAULT_DOMAIN))
}

/// The onboarding sentence an unauthenticated backend reports, carrying the
/// loader's own problem string when there is one.
pub fn onboarding_payload(problem: Option<&str>) -> String {
    let base = format!(
        "paste app_id:app_secret from a Feishu custom app created at {DEFAULT_DOMAIN}/app (grant im:message, im:chat:readonly and im:resource), or export {APP_ID_ENV} and {APP_SECRET_ENV}; set {DOMAIN_ENV}=https://open.larksuite.com for Lark International"
    );
    match problem {
        Some(problem) => format!("{base} ({problem})"),
        None => base,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_colon_separated_pair_decodes_to_both_halves() {
        let creds = parse_credential("cli_a1b2c3:secrethex").expect("parse");
        assert_eq!(creds.app_id, "cli_a1b2c3");
        assert_eq!(creds.app_secret, "secrethex");
        assert_eq!(creds.domain, DEFAULT_DOMAIN);
        assert_eq!(creds.base_url(), "https://open.feishu.cn");
    }

    #[test]
    fn whitespace_around_the_halves_is_trimmed() {
        let creds = parse_credential(" cli_a1b2c3 : secrethex ").expect("parse");
        assert_eq!(creds.app_id, "cli_a1b2c3");
        assert_eq!(creds.app_secret, "secrethex");
    }

    #[test]
    fn a_credential_with_no_colon_is_refused_with_the_encoding_named() {
        let err = parse_credential("secrethex").unwrap_err();
        assert!(err.contains("app_id:app_secret"), "{err}");
        assert!(err.contains(DEFAULT_DOMAIN), "{err}");
    }

    #[test]
    fn an_empty_half_is_refused() {
        assert!(parse_credential("cli_a1b2c3:").is_err());
        assert!(parse_credential(":secrethex").is_err());
        assert!(parse_credential("  :  ").is_err());
    }

    #[test]
    fn encode_round_trips_through_parse() {
        let creds = FeishuCredentials::new("cli_x", "s3cret", DEFAULT_DOMAIN);
        let back = parse_credential(&creds.encode()).expect("round trip");
        assert_eq!(back, creds);
    }

    #[test]
    fn the_onboarding_sentence_names_both_env_vars() {
        let payload = onboarding_payload(None);
        assert!(payload.contains(APP_ID_ENV), "{payload}");
        assert!(payload.contains(APP_SECRET_ENV), "{payload}");
        assert!(payload.contains(DOMAIN_ENV), "{payload}");
        assert!(payload.contains("app_id:app_secret"), "{payload}");
        assert!(payload.contains("im:message"), "{payload}");
        assert!(payload.contains("im:chat:readonly"), "{payload}");
        let with_problem = onboarding_payload(Some("FEISHU_APP_SECRET is not set"));
        assert!(with_problem.ends_with("(FEISHU_APP_SECRET is not set)"));
    }
}
