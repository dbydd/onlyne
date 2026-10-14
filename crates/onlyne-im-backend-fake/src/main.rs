//! A platform backend that behaves, for tests: the standard backend protocol
//! with scripted answers and no platform at all.
//!
//! Every case that needs a backend running — the gateway's own integration
//! test, the setup wizard, a hand-run smoke — otherwise has to reach a real
//! platform. This one is a real process speaking the real frames, so the thing
//! under test is the protocol and the wizard rather than a mock inside them.
//!
//! Scripted from the environment, all optional:
//!
//! - `ONLYNE_FAKE_BACKEND_TOKEN` — the credential this backend accepts. Any
//!   other credential is refused, which is how a test drives the failure path.
//!   Unset means `let-me-in`.
//! - `ONLYNE_FAKE_BACKEND_CHATS` — the conversation list it reports, as
//!   `id:title:kind` triples separated by commas; unset reports none, which is
//!   the platform that gives no listing.
//! - `ONLYNE_FAKE_BACKEND_EVENTS` — events to emit, one per comma, each read as
//!   `conversation|text`; unset emits none.
//! - `ONLYNE_FAKE_BACKEND_IDENTITY` — what a successful login answers.
//!
//! A credential file the gateway points at (`ONLYNE_IM_CREDENTIAL`) is read
//! through the same loader every real backend uses, so the storage convention
//! is exercised rather than bypassed.

use anyhow::Result;
use async_trait::async_trait;
use onlyne_im_backend::backend::Backend;
use onlyne_im_backend::frame::{
    HealthArgs, HelloArgs, ImConversation, LoginArgs, OnboardingKind, OnboardingPrompt, SendArgs,
};
use onlyne_im_backend::payload::{ImEvent, ImEventKind};
use std::collections::VecDeque;

/// The token this backend accepts when the environment names none.
const DEFAULT_TOKEN: &str = "let-me-in";
/// The platform name it reports.
const PLATFORM: &str = "fake";
/// The variable holding the credential file the gateway hands over.
const TOKEN_ENV: &str = "ONLYNE_FAKE_BACKEND_TOKEN";

fn var(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

/// One fake backend: what it accepts, what it lists, and what it still owes.
pub struct FakeBackend {
    /// The credential this backend accepts.
    token: String,
    /// Why no credential was found, when that is the case.
    problem: Option<String>,
    /// The conversations it reports.
    conversations: Vec<ImConversation>,
    /// Events still to emit, in order.
    events: VecDeque<ImEvent>,
    /// What a successful login answers.
    identity: String,
    /// What this run has been sent, for a test to read back.
    sent: Vec<SendArgs>,
}

impl FakeBackend {
    /// Build from the environment, through the credential loader every real
    /// backend shares.
    pub fn from_env() -> Self {
        let loaded = onlyne_im_backend::credential::load(None, TOKEN_ENV);
        let token = if loaded.is_present() {
            loaded.value
        } else {
            DEFAULT_TOKEN.to_string()
        };
        let conversations = var("ONLYNE_FAKE_BACKEND_CHATS")
            .map(|raw| raw.split(',').map(parse_conversation).collect())
            .unwrap_or_default();
        let events = var("ONLYNE_FAKE_BACKEND_EVENTS")
            .map(|raw| {
                raw.split(',')
                    .map(parse_event)
                    .collect::<VecDeque<ImEvent>>()
            })
            .unwrap_or_default();
        let identity =
            var("ONLYNE_FAKE_BACKEND_IDENTITY").unwrap_or_else(|| "fake-backend".to_string());
        Self {
            token,
            problem: loaded.problem,
            conversations,
            events,
            identity,
            sent: Vec::new(),
        }
    }
}

/// `id:title:kind`, with the trailing parts optional.
fn parse_conversation(raw: &str) -> ImConversation {
    let mut parts = raw.trim().splitn(3, ':');
    let id = parts.next().unwrap_or_default().trim().to_string();
    let title = parts
        .next()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);
    let kind = parts
        .next()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);
    ImConversation { id, title, kind }
}

/// `conversation|text`.
fn parse_event(raw: &str) -> ImEvent {
    let raw = raw.trim();
    let (conversation, text) = raw.split_once('|').unwrap_or((raw, ""));
    ImEvent {
        gateway: var("ONLYNE_IM_GATEWAY").unwrap_or_else(|| "fake".to_string()),
        platform: PLATFORM.to_string(),
        channel: PLATFORM.to_string(),
        conversation: conversation.trim().to_string(),
        kind: ImEventKind::Message,
        id: format!("fake-{}", conversation.trim()),
        text: Some(text.to_string()),
        at: chrono::Utc::now().to_rfc3339(),
        ..ImEvent::default()
    }
}

#[async_trait]
impl Backend for FakeBackend {
    fn hello(&self) -> HelloArgs {
        HelloArgs {
            platform: PLATFORM.to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            capabilities: vec!["message".into(), "action".into()],
            authenticated: !self.token.is_empty(),
            onboarding: self.token.is_empty().then(|| OnboardingPrompt {
                kind: OnboardingKind::ManualCode,
                payload: match &self.problem {
                    Some(problem) => format!("paste a token ({problem})"),
                    None => format!("set {TOKEN_ENV} or run `onlyne-im login`"),
                },
                expires_in: None,
            }),
        }
    }

    async fn login(&mut self, args: LoginArgs) -> Result<Option<String>> {
        let given = args.credential.trim();
        if given == self.token {
            Ok(Some(self.identity.clone()))
        } else {
            anyhow::bail!("the fake backend accepts `{}`", self.token)
        }
    }

    async fn list_conversations(&mut self) -> Result<Vec<ImConversation>> {
        Ok(self.conversations.clone())
    }

    async fn next_event(&mut self) -> Result<Option<ImEvent>> {
        match self.events.pop_front() {
            Some(event) => Ok(Some(event)),
            // Park forever rather than end: a backend that returned `None`
            // would make the gateway read it as a platform that closed.
            None => {
                futures_pending().await;
                Ok(None)
            }
        }
    }

    async fn send(&mut self, args: SendArgs) -> Result<()> {
        self.sent.push(args);
        Ok(())
    }

    fn health(&self) -> HealthArgs {
        HealthArgs {
            state: "online".to_string(),
            detail: Some(format!("{} sent", self.sent.len())),
            uptime_s: 0,
        }
    }
}

/// Wait forever. A test drives this process through the protocol; the event
/// stream has nothing more to give, and ending it would look like a crash.
async fn futures_pending() {
    std::future::pending::<()>().await
}

#[tokio::main]
async fn main() {
    onlyne_im_backend::install_crypto_provider();
    if let Err(error) = onlyne_im_backend::serve(FakeBackend::from_env()).await {
        eprintln!("onlyne-im-backend-fake: {error:#}");
        std::process::exit(1);
    }
}
