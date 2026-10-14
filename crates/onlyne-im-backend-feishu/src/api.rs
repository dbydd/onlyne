//! The Feishu Open API client.
//!
//! Ported from `frozen/gateway-v1`'s `tenant_token` and `post_message`: the
//! only two HTTP calls this backend makes. Feishu shapes stop here —
//! everything above speaks standard payloads; only this module knows a URL,
//! a tenant token, or Feishu's `{code, data}` envelope.
//!
//! Inbound does not come from here: Feishu has no polling API, events arrive
//! over the SDK's websocket connection (`openlark`'s `LarkWsClient`, exactly
//! what v1 used), and that client holds its own token lifecycle.

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};

/// One Feishu API client: the app identity and the domain it calls.
pub struct Api {
    creds: crate::auth::FeishuCredentials,
    http: reqwest::Client,
}

impl Api {
    pub fn new(creds: crate::auth::FeishuCredentials) -> Self {
        Self {
            creds,
            http: reqwest::Client::new(),
        }
    }

    /// The same client pointed at another credential pair, reusing the
    /// connection pool. A login check swaps the pair in before fetching a
    /// tenant token, so what it verifies is exactly what it would send with.
    pub fn with_credentials(&self, creds: crate::auth::FeishuCredentials) -> Self {
        Self {
            creds,
            http: self.http.clone(),
        }
    }

    /// The credential pair this client sends. A backend reports whether it
    /// holds one in its `hello` without contacting Feishu.
    pub fn credentials(&self) -> &crate::auth::FeishuCredentials {
        &self.creds
    }

    /// Fetch a tenant access token (`POST
    /// /open-apis/auth/v3/tenant_access_token/internal`). This is the whole of
    /// authentication: the pair is verified by whether Feishu issues a token,
    /// so `login` is one call.
    pub async fn tenant_token(&self) -> Result<String> {
        let url = format!(
            "{}/open-apis/auth/v3/tenant_access_token/internal",
            self.creds.base_url()
        );
        let v: Value = self
            .http
            .post(&url)
            .json(&json!({
                "app_id": self.creds.app_id,
                "app_secret": self.creds.app_secret,
            }))
            .send()
            .await
            .context("feishu token request")?
            .error_for_status()
            .context("feishu token request")?
            .json()
            .await
            .context("feishu token decode")?;
        v.get("tenant_access_token")
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| {
                anyhow::anyhow!("feishu refused this app_id/app_secret (token response: {v})")
            })
    }

    /// Post one rendered request body into one conversation.
    ///
    /// The `receive_id_type` follows the conversation's shape, as in v1: an
    /// `ou_…` open id is a p2p peer, anything else (`oc_…`) is a chat id.
    /// Feishu answers `code: 0` on success and an error code otherwise.
    pub async fn post_message(&self, conversation: &str, request: Value) -> Result<String> {
        let receive_id_type = if conversation.starts_with("ou_") {
            "open_id"
        } else {
            "chat_id"
        };
        let url = format!(
            "{}/open-apis/im/v1/messages?receive_id_type={receive_id_type}",
            self.creds.base_url()
        );
        let token = self.tenant_token().await?;
        let v: Value = self
            .http
            .post(&url)
            .bearer_auth(&token)
            .json(&request)
            .send()
            .await
            .context("feishu send")?
            .error_for_status()
            .context("feishu send")?
            .json()
            .await
            .context("feishu send decode")?;
        if v.get("code").and_then(Value::as_i64).unwrap_or(0) != 0 {
            bail!("feishu send failed: {v}");
        }
        v.pointer("/data/message_id")
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| anyhow::anyhow!("feishu send missing message_id: {v}"))
    }
}
