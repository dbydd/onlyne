//! The Telegram Bot API client.
//!
//! One thin wrapper over the four calls this backend makes. Telegram shapes stop
//! at this module's public methods: everything above speaks the standard payload
//! or the standard event, and only `call` knows a URL, a token, or the Bot API's
//! JSON envelopes.

use crate::render::Call;
use anyhow::{Context, Result, bail};
use serde_json::{Value, json};

/// One Bot API token, from `ONLYNE_TELEGRAM_TOKEN` or `--token`.
pub struct Bot {
    token: String,
    http: reqwest::Client,
    /// The API root, overridable for a fake in tests.
    base: String,
}

impl Bot {
    pub fn new(token: impl Into<String>) -> Self {
        Self {
            token: token.into(),
            http: reqwest::Client::new(),
            base: "https://api.telegram.org".to_string(),
        }
    }

    /// Point the client at another API root (a fake server in tests).
    pub fn with_base(mut self, base: impl Into<String>) -> Self {
        self.base = base.into();
        self
    }

    /// The token this client sends. A backend reports whether it holds one in
    /// its `hello` without contacting Telegram.
    pub fn token(&self) -> &str {
        &self.token
    }

    /// The same client pointed at another token, reusing the connection pool.
    /// A login check swaps the token in before asking `getMe`, so what it
    /// verifies is exactly what it would send with.
    pub fn with_token(&self, token: impl Into<String>) -> Self {
        Self {
            token: token.into(),
            http: self.http.clone(),
            base: self.base.clone(),
        }
    }

    /// Long-poll `getUpdates`. `offset` is the last seen update id plus one;
    /// `timeout_s` is how long Telegram may hold the request.
    pub async fn get_updates(&self, offset: i64, timeout_s: u64) -> Result<Vec<Value>> {
        let body = self
            .call(
                "getUpdates",
                json!({
                    "offset": offset,
                    "timeout": timeout_s,
                    "allowed_updates": ["message", "edited_message", "callback_query"],
                }),
            )
            .await?;
        Ok(body.as_array().cloned().unwrap_or_default())
    }

    /// The bot this token belongs to: `getMe`.
    ///
    /// This is the whole of authentication. Telegram has no separate login —
    /// the token *is* the credential — so verifying it is one call, and the
    /// username it answers is what an operator reads back to know they pasted
    /// the right bot's token.
    pub async fn get_me(&self) -> Result<Value> {
        self.call("getMe", json!({})).await
    }

    /// One chat's own description: `getChat`. An id the bot cannot see is an
    /// error, which is how a bad chat id is told apart from a quiet chat.
    pub async fn get_chat(&self, chat_id: &str) -> Result<Value> {
        self.call("getChat", json!({ "chat_id": chat_id })).await
    }

    /// Perform one rendered call, uploading a file when the call carries one.
    /// Render and send one call, answering the platform's id for the message
    /// it created — `None` for a call the platform answers without one (an
    /// upload whose response carries no `message_id`).
    pub async fn perform(&self, call: &Call) -> Result<Option<String>> {
        match call {
            Call::Message {
                chat_id,
                text,
                parse_mode,
                reply_markup,
            } => {
                let mut form = json!({ "chat_id": chat_id, "text": text });
                if let Some(mode) = parse_mode {
                    form["parse_mode"] = json!(mode);
                }
                if let Some(markup) = reply_markup {
                    form["reply_markup"] = markup.clone();
                }
                let result = self.call("sendMessage", form).await?;
                return Ok(result
                    .get("message_id")
                    .map(|v| v.to_string())
                    .filter(|id| !id.trim().is_empty() && id != "null"));
            }
            Call::Photo {
                chat_id,
                path,
                caption,
            } => {
                self.upload("sendPhoto", "photo", chat_id, path, caption.as_deref())
                    .await?;
            }
            Call::Document {
                chat_id,
                path,
                caption,
            } => {
                self.upload(
                    "sendDocument",
                    "document",
                    chat_id,
                    path,
                    caption.as_deref(),
                )
                .await?;
            }
        }
        // An upload answers with the message too, and Telegram names it the
        // same way; reading it costs one line and keeps a photo reply in the
        // chain like any other.
        Ok(None)
    }

    /// One JSON POST, unwrapping Telegram's `{ok, result}` envelope.
    async fn call(&self, method: &str, body: Value) -> Result<Value> {
        let url = format!("{}/bot{}/{method}", self.base, self.token);
        let response = self
            .http
            .post(&url)
            .json(&body)
            .send()
            .await
            // Telegram puts the bot token in the request path, and reqwest's
            // error Display carries the URL, so an un-stripped error writes the
            // token into the operator's log. `POST <method>` says everything the
            // documenter needs; the transport cause rides along as the source.
            .map_err(reqwest::Error::without_url)
            .with_context(|| format!("POST {method}"))?;
        let status = response.status();
        let value: Value = response
            .json()
            .await
            .map_err(reqwest::Error::without_url)
            .with_context(|| format!("{method} answered no JSON"))?;
        if !value.get("ok").and_then(Value::as_bool).unwrap_or(false) {
            bail!(
                "{method} refused (HTTP {status}): {}",
                value
                    .get("description")
                    .and_then(Value::as_str)
                    .unwrap_or("no description")
            );
        }
        Ok(value.get("result").cloned().unwrap_or(Value::Null))
    }

    /// A multipart upload of one local file.
    async fn upload(
        &self,
        method: &str,
        field: &str,
        chat_id: &str,
        path: &str,
        caption: Option<&str>,
    ) -> Result<()> {
        let bytes = tokio::fs::read(path)
            .await
            .with_context(|| format!("reading {path} for {method}"))?;
        let name = std::path::Path::new(path)
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| "file".to_string());
        let part = reqwest::multipart::Part::bytes(bytes).file_name(name);
        let mut form = reqwest::multipart::Form::new()
            .text("chat_id", chat_id.to_string())
            .part(field.to_string(), part);
        if let Some(caption) = caption {
            form = form.text("caption", caption.to_string());
        }
        let url = format!("{}/bot{}/{method}", self.base, self.token);
        let response = self
            .http
            .post(&url)
            .multipart(form)
            .send()
            .await
            .map_err(reqwest::Error::without_url)
            .with_context(|| format!("POST {method}"))?;
        if !response.status().is_success() {
            bail!("{method} refused with HTTP {}", response.status());
        }
        Ok(())
    }
}
