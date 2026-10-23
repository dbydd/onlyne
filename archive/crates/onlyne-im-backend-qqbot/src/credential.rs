//! Credential parsing for the QQ Open Platform backend.
//!
//! QQ's Open Platform authenticates a bot with an app id and an app secret,
//! two values, not one token. The backend protocol hands one credential
//! string, so the two values share it as `app_id:app_secret` — the encoding
//! the onboarding prompt documents and [`parse`] reads back. A secret may not
//! contain `:`, which the QQ Open Platform never issues; the first `:` is the
//! separator, and everything after it is the secret.

/// The conventional environment variables, named in the onboarding prompt.
pub const APP_ID_ENV: &str = "QQBOT_APP_ID";
pub const APP_SECRET_ENV: &str = "QQBOT_APP_SECRET";

/// One app id and secret pair, already split.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QqCredentials {
    pub app_id: String,
    pub app_secret: String,
}

impl QqCredentials {
    /// Parse the `app_id:app_secret` credential string.
    ///
    /// A missing separator is the one shape an operator pastes wrong, so the
    /// error names the encoding rather than reporting a bare "bad credential".
    pub fn parse(value: &str) -> anyhow::Result<Self> {
        let (app_id, app_secret) = onlyne_im_backend::credential::parse_colon_pair(
            value,
            &format!(
                "the credential must be `{APP_ID_ENV}:{APP_SECRET_ENV}` \
                 (app id and app secret separated by one colon)"
            ),
            "the app id (before the colon) is empty",
            "the app secret (after the colon) is empty",
        )?;
        Ok(Self { app_id, app_secret })
    }

    /// The encoded form, what a credential file holds.
    pub fn encode(&self) -> String {
        format!("{}:{}", self.app_id, self.app_secret)
    }
}

/// The onboarding sentence an unauthenticated backend reports: what to paste
/// and where the two halves come from.
pub fn onboarding_prompt(problem: Option<&str>) -> onlyne_im_backend::frame::OnboardingPrompt {
    let base = format!(
        "paste the app credentials as `{APP_ID_ENV}:{APP_SECRET_ENV}` \
         (one colon between them), from the QQ Open Platform bot console \
         (q.qq.com), or set both variables in the environment"
    );
    let payload = match problem {
        Some(problem) => format!("{base} ({problem})"),
        None => base,
    };
    onlyne_im_backend::frame::OnboardingPrompt {
        kind: onlyne_im_backend::frame::OnboardingKind::ManualCode,
        payload,
        expires_in: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pair_splits_on_the_first_colon() {
        let parsed = QqCredentials::parse("102045678:secret-value").unwrap();
        assert_eq!(parsed.app_id, "102045678");
        assert_eq!(parsed.app_secret, "secret-value");
        assert_eq!(parsed.encode(), "102045678:secret-value");
    }

    #[test]
    fn whitespace_around_the_halves_is_trimmed() {
        let parsed = QqCredentials::parse(" 102045678 : secret-value ").unwrap();
        assert_eq!(parsed.app_id, "102045678");
        assert_eq!(parsed.app_secret, "secret-value");
    }

    #[test]
    fn a_missing_separator_names_the_encoding() {
        let error = QqCredentials::parse("just-a-token").unwrap_err();
        assert!(
            error.to_string().contains("app id and app secret"),
            "{error}"
        );
    }

    #[test]
    fn an_empty_half_names_which_half() {
        assert!(
            QqCredentials::parse(":secret")
                .unwrap_err()
                .to_string()
                .contains("app id")
        );
        assert!(
            QqCredentials::parse("id:")
                .unwrap_err()
                .to_string()
                .contains("app secret")
        );
        assert!(
            QqCredentials::parse("")
                .unwrap_err()
                .to_string()
                .contains("empty")
        );
    }
}
