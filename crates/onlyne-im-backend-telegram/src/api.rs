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

    /// Perform one rendered call, uploading a file when the call carries one.
    pub async fn perform(&self, call: &Call) -> Result<()> {
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
                self.call("sendMessage", form).await?;
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
        Ok(())
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
            .with_context(|| format!("POST {method}"))?;
        let status = response.status();
        let value: Value = response
            .json()
            .await
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
            .with_context(|| format!("POST {method}"))?;
        if !response.status().is_success() {
            bail!("{method} refused with HTTP {}", response.status());
        }
        Ok(())
    }
}
