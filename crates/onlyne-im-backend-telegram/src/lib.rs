//! Telegram platform backend for the Onlyne IM gateway.
//!
//! Implements the standard backend protocol (`onlyne-im-backend`) and calls
//! Telegram's Bot API. The two translation halves live in [`update`] (a platform
//! update becomes a standard event) and [`render`] (a standard payload becomes
//! Bot API calls); this module is the `Backend` that joins them.

mod api;
mod render;
mod update;

use anyhow::{Context, Result};
use api::Bot;
use async_trait::async_trait;
use onlyne_im_backend::backend::Backend;
use onlyne_im_backend::frame::{HealthArgs, HelloArgs, SendArgs};
use onlyne_im_backend::payload::ImEvent;

/// How long Telegram holds a `getUpdates` when nothing arrives.
const POLL_TIMEOUT_S: u64 = 30;

/// One Telegram backend: a bot token and the poll cursor.
pub struct TelegramBackend {
    bot: Bot,
    gateway: String,
    platform: String,
    /// The next `update_id` to ask for.
    offset: i64,
    started: std::time::Instant,
}

impl TelegramBackend {
    pub fn new(bot: Bot, gateway: impl Into<String>) -> Self {
        Self {
            bot,
            gateway: gateway.into(),
            platform: "telegram".to_string(),
            offset: 0,
            started: std::time::Instant::now(),
        }
    }
}

#[async_trait]
impl Backend for TelegramBackend {
    fn hello(&self) -> HelloArgs {
        HelloArgs {
            platform: self.platform.clone(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            capabilities: vec!["message".into(), "action".into()],
        }
    }

    async fn next_event(&mut self) -> Result<Option<ImEvent>> {
        loop {
            let updates = self
                .bot
                .get_updates(self.offset, POLL_TIMEOUT_S)
                .await
                .context("getUpdates")?;
            for update in &updates {
                if let Some(id) = update.get("update_id").and_then(serde_json::Value::as_i64) {
                    // Advance past every update seen, so a skipped one is not
                    // served twice.
                    self.offset = self.offset.max(id + 1);
                }
                if let Some(event) =
                    update::event_from_update(&self.gateway, &self.platform, update)
                {
                    return Ok(Some(event));
                }
            }
            // An empty poll is the ordinary case; keep waiting.
        }
    }

    async fn send(&mut self, args: SendArgs) -> Result<()> {
        for call in render::render(&args.conversation, &args.payload) {
            self.bot.perform(&call).await?;
        }
        Ok(())
    }

    fn health(&self) -> HealthArgs {
        HealthArgs {
            state: "online".to_string(),
            detail: None,
            uptime_s: self.started.elapsed().as_secs(),
        }
    }
}

/// Build the backend from the environment and run the standard protocol loop.
pub async fn run(gateway: String, token: String, base: Option<String>) -> Result<()> {
    let mut bot = Bot::new(token);
    if let Some(base) = base {
        bot = bot.with_base(base);
    }
    onlyne_im_backend::serve(TelegramBackend::new(bot, gateway)).await
}
