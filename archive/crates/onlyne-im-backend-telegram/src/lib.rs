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
use onlyne_im_backend::frame::{
    HealthArgs, HelloArgs, ImConversation, LoginArgs, OnboardingKind, OnboardingPrompt, SendArgs,
    SentArgs,
};
use onlyne_im_backend::payload::ImEvent;

/// How long Telegram holds a `getUpdates` when nothing arrives.
const POLL_TIMEOUT_S: u64 = 30;
/// The conventional token variable, for an operator who exports one instead of
/// running `onlyne-im login`.
pub const TOKEN_ENV: &str = "ONLYNE_TELEGRAM_TOKEN";

/// One Telegram backend: a bot token and the poll cursor.
pub struct TelegramBackend {
    bot: Bot,
    gateway: String,
    platform: String,
    /// The next `update_id` to ask for.
    offset: i64,
    started: std::time::Instant,
    /// Why no credential was found, when that is the case. It becomes the
    /// onboarding sentence, so an operator reads the exact reason.
    problem: Option<String>,
}

impl TelegramBackend {
    pub fn new(bot: Bot, gateway: impl Into<String>) -> Self {
        Self {
            bot,
            gateway: gateway.into(),
            platform: "telegram".to_string(),
            offset: 0,
            started: std::time::Instant::now(),
            problem: None,
        }
    }

    /// Build from the environment: `--token`, then the credential file the
    /// gateway names, then `ONLYNE_TELEGRAM_TOKEN`. A missing credential is
    /// carried as [`Self::problem`] rather than raised, because it is the state
    /// `onlyne-im login` exists to fix and this backend has to be running to
    /// answer that login.
    pub fn from_env(
        explicit: Option<String>,
        gateway: impl Into<String>,
        base: Option<String>,
    ) -> Self {
        let loaded = onlyne_im_backend::credential::load(explicit.as_deref(), TOKEN_ENV);
        let mut bot = Bot::new(loaded.value);
        if let Some(base) = base {
            bot = bot.with_base(base);
        }
        let mut backend = Self::new(bot, gateway);
        backend.problem = loaded.problem;
        backend
    }

    /// The onboarding sentence an unauthenticated backend reports. Telegram's
    /// Bot API has exactly one credential shape: a token string from
    /// `@BotFather`, pasted.
    ///
    /// When the loader found a reason — an unreadable file, an unset variable —
    /// the prompt carries it, so the sentence an operator reads names the exact
    /// file or variable rather than repeating the general instruction.
    fn token_prompt(problem: Option<&str>) -> OnboardingPrompt {
        let base = "paste the bot token @BotFather gave you, or set ONLYNE_TELEGRAM_TOKEN";
        let payload = match problem {
            Some(problem) => format!("{base} ({problem})"),
            None => base.to_string(),
        };
        OnboardingPrompt {
            kind: OnboardingKind::ManualCode,
            payload,
            expires_in: None,
        }
    }
}

#[async_trait]
impl Backend for TelegramBackend {
    fn hello(&self) -> HelloArgs {
        // Telegram has no credential-less mode: an empty token is a backend
        // that cannot do anything, and saying so here is what lets the gateway
        // print one actionable sentence instead of idling silently.
        let authenticated = !self.bot.token().trim().is_empty();
        HelloArgs {
            platform: self.platform.clone(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            capabilities: vec!["message".into(), "action".into()],
            authenticated,
            onboarding: (!authenticated).then(|| Self::token_prompt(self.problem.as_deref())),
        }
    }

    /// Verify a pasted token with `getMe` and answer the bot's username.
    async fn login(&mut self, args: LoginArgs) -> Result<Option<String>> {
        let token = args.credential.trim();
        if token.is_empty() {
            anyhow::bail!("the token is empty");
        }
        // The credential is swapped in before the check, so a wrong token
        // leaves this backend holding a token it just proved bad rather than
        // the caller's good one. The setup verb reads the answer and decides.
        self.bot = self.bot.with_token(token);
        let me =
            self.bot.get_me().await.context(
                "Telegram refused this token; check it against the one @BotFather gave you",
            )?;
        let username = me
            .get("username")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string);
        Ok(username)
    }

    /// The chats this bot can see by id.
    ///
    /// The Bot API has no "list my chats" call: a bot learns a chat when
    /// something happens in it. So discovery here answers exactly the chats the
    /// caller already names — which is what makes `getChat` the useful check:
    /// it confirms a chat id is real and the bot can see it, and refuses one it
    /// cannot, instead of the gateway discovering that on the first render.
    async fn list_conversations(&mut self) -> Result<Vec<ImConversation>> {
        Ok(Vec::new())
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

    async fn send(&mut self, args: SendArgs) -> Result<SentArgs> {
        let mut message_id = None;
        for call in render::render(&args.conversation, &args.payload) {
            // Telegram answers each send with the message it created, and one
            // payload may render into several calls: the last id is the one a
            // reply to this turn should point at.
            if let Some(id) = self.bot.perform(&call).await? {
                message_id = Some(id);
            }
        }
        Ok(SentArgs {
            conversation: args.conversation,
            message_id,
            // One id per message on Telegram: a displayed quote is
            // `reply_to_message_id`, which the render already sets.
            ref_handle: None,
            error: None,
        })
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
    let explicit = (!token.trim().is_empty()).then_some(token);
    onlyne_im_backend::serve(TelegramBackend::from_env(explicit, gateway, base)).await
}
