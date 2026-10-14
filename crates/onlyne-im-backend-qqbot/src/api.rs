//! The QQ Open Platform API client.
//!
//! One thin wrapper over the three calls this backend makes: the app access
//! token, the message send per scene, and the websocket gateway the events
//! arrive on. QQ's shapes stop here; everything above speaks the standard
//! payload or the standard event.

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use std::time::{Duration, Instant};

/// The token endpoint, from the QQ Open Platform's documented API.
pub const TOKEN_URL: &str = "https://bots.qq.com/app/getAppAccessToken";
/// The production API root.
pub const API_URL: &str = "https://api.sgroup.qq.com";
/// The sandbox API root.
pub const SANDBOX_API_URL: &str = "https://sandbox.api.sgroup.qq.com";
/// The production event-gateway websocket.
pub const WS_URL: &str = "wss://api.sgroup.qq.com/websocket";
/// The sandbox event-gateway websocket.
pub const SANDBOX_WS_URL: &str = "wss://sandbox.api.sgroup.qq.com/websocket";

/// Which environment the client talks to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Endpoint {
    Production,
    Sandbox,
}

impl Endpoint {
    pub fn api_base(self) -> &'static str {
        match self {
            Self::Production => API_URL,
            Self::Sandbox => SANDBOX_API_URL,
        }
    }

    pub fn ws_url(self) -> &'static str {
        match self {
            Self::Production => WS_URL,
            Self::Sandbox => SANDBOX_WS_URL,
        }
    }
}

/// The four scenes QQ addressable, encoded in a conversation's prefix.
///
/// A conversation is `<scene>:<id>` — `group:g1`, `c2c:u1` (also `user:u1`
/// or `private:u1`), `channel:c1`, `direct:d1`. A bare id is a channel id,
/// matching QQ's most common gateway address, the same default v1 chose.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scene {
    Group,
    C2c,
    Channel,
    Direct,
}

impl Scene {
    /// Split a conversation address into its scene and platform id.
    pub fn parse(conversation: &str) -> Result<(&str, Self)> {
        for (prefix, scene) in [
            ("group:", Self::Group),
            ("c2c:", Self::C2c),
            ("user:", Self::C2c),
            ("private:", Self::C2c),
            ("channel:", Self::Channel),
            ("direct:", Self::Direct),
        ] {
            if let Some(value) = conversation.strip_prefix(prefix) {
                if value.trim().is_empty() {
                    bail!("the qqbot conversation id after `{prefix}` is empty");
                }
                return Ok((value, scene));
            }
        }
        if conversation.trim().is_empty() {
            bail!("the qqbot conversation is empty");
        }
        Ok((conversation, Self::Channel))
    }

    /// The message-send path for one conversation id.
    pub fn endpoint(self, conversation: &str) -> String {
        match self {
            Self::Group => format!("/v2/groups/{conversation}/messages"),
            Self::C2c => format!("/v2/users/{conversation}/messages"),
            Self::Channel => format!("/channels/{conversation}/messages"),
            Self::Direct => format!("/dms/{conversation}/messages"),
        }
    }
}

/// One cached app access token.
#[derive(Debug, Clone)]
struct AccessToken {
    value: String,
    expires_at: Instant,
}

/// The QQ Open Platform client: the app credentials and the token cache.
pub struct QqApi {
    app_id: String,
    app_secret: String,
    endpoint: Endpoint,
    http: reqwest::Client,
    token: Option<AccessToken>,
}

impl QqApi {
    pub fn new(credentials: crate::credential::QqCredentials, endpoint: Endpoint) -> Self {
        Self {
            app_id: credentials.app_id,
            app_secret: credentials.app_secret,
            endpoint,
            http: reqwest::Client::new(),
            token: None,
        }
    }

    pub fn endpoint(&self) -> Endpoint {
        self.endpoint
    }

    /// One app access token, cached until a minute before expiry.
    ///
    /// QQ's Open Platform token endpoint answers `{access_token, expires_in}`
    /// where `expires_in` is seconds (7200 on a fresh grant).
    pub async fn access_token(&mut self) -> Result<String> {
        if let Some(token) = &self.token
            && Instant::now() < token.expires_at
        {
            return Ok(token.value.clone());
        }
        let response = self
            .http
            .post(TOKEN_URL)
            .json(&json!({
                "appId": self.app_id,
                "clientSecret": self.app_secret,
            }))
            .send()
            .await
            .context("POST getAppAccessToken")?;
        let status = response.status();
        let text = response
            .text()
            .await
            .context("reading the token response")?;
        if !status.is_success() {
            bail!("the token request refused ({status}): {text}");
        }
        let value: Value = serde_json::from_str(&text).context("the token response is not JSON")?;
        let token = value
            .get("access_token")
            .and_then(Value::as_str)
            .filter(|token| !token.trim().is_empty())
            .ok_or_else(|| anyhow::anyhow!("the token response omitted access_token: {text}"))?
            .to_owned();
        let expires_in = value
            .get("expires_in")
            .or_else(|| value.get("expiresIn"))
            .and_then(Value::as_u64)
            .unwrap_or(7_200);
        self.token = Some(AccessToken {
            value: token.clone(),
            expires_at: Instant::now() + Duration::from_secs(expires_in.saturating_sub(60)),
        });
        Ok(token)
    }

    /// One authenticated JSON POST against the API root.
    ///
    /// QQ takes its bot token under the `QQBot` auth scheme and nothing else.
    /// A `Bearer` header must never ride along: `bearer_auth` writes the same
    /// `Authorization` key, so it *replaces* the `QQBot` value and the platform
    /// rejects the call — an HTML 400 from the edge gateway, or `401 请求头
    /// Authorization参数格式错误` when only `Bearer` arrives. Measured live:
    /// `QQBot <token>` answers 200 with a message id, `Bearer <token>` answers
    /// 401 with code 11241.
    pub async fn post_api(&self, token: &str, path: &str, body: Value) -> Result<Value> {
        let url = format!("{}{path}", self.endpoint.api_base());
        let response = self
            .http
            .post(&url)
            .header("Authorization", format!("QQBot {token}"))
            .json(&body)
            .send()
            .await
            .with_context(|| format!("POST {path}"))?;
        let status = response.status();
        let text = response
            .text()
            .await
            .with_context(|| format!("reading the {path} response"))?;
        if !status.is_success() {
            bail!("{path} refused ({status}): {text}");
        }
        if text.trim().is_empty() {
            return Ok(Value::Object(Default::default()));
        }
        serde_json::from_str(&text).with_context(|| format!("{path} answered no JSON"))
    }

    /// Send one message body into one conversation, returning the platform
    /// message id the response names (QQ's answer shape varies by scene).
    pub async fn send_message(
        &mut self,
        conversation: &str,
        scene: Scene,
        body: Value,
    ) -> Result<String> {
        let token = self.access_token().await?;
        let response = self
            .post_api(&token, &scene.endpoint(conversation), body)
            .await?;
        Ok(response
            .get("id")
            .or_else(|| response.get("msg_id"))
            .or_else(|| response.get("message_id"))
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .unwrap_or("qqbot-out")
            .to_owned())
    }

    /// Connect to the event gateway and announce the app: the `IDENTIFY`
    /// payload the QQ websocket protocol expects after the `HELLO` frame.
    /// The intent is always `EVENTS` — this backend listens, it never
    /// consumes intents it cannot map.
    pub fn identify_payload(
        &self,
        token: &str,
        session_id: Option<&str>,
        seq: Option<u64>,
    ) -> Value {
        let _ = (session_id, seq);
        json!({
            "op": 2,
            "d": {
                "token": format!("QQBot {token}"),
                "intents": (1 << 0) | (1 << 12) | (1 << 25),
                "shard": [0, 1],
            }
        })
    }
}
