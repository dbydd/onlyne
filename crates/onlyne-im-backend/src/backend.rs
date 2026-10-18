//! The trait a platform backend implements.
//!
//! A backend is the *only* place platform SDKs live. It answers two questions —
//! "what happened on the platform" and "send this standard payload" — and knows
//! nothing about Onlyne roles, sessions, or routing.

use crate::frame::{HealthArgs, HelloArgs, ImConversation, LoginArgs, SendArgs, SentArgs};
use crate::payload::ImEvent;
use anyhow::Result;
use async_trait::async_trait;

#[async_trait]
pub trait Backend: Send + Sync {
    /// What this backend bridges and what it can do.
    fn hello(&self) -> HelloArgs;

    /// Block until one platform event arrives, or `None` when the backend is
    /// done (the platform closed it).
    async fn next_event(&mut self) -> Result<Option<ImEvent>>;

    /// Render and send one standard payload into a conversation.
    /// Render and send one payload, answering what the platform made of it.
    ///
    /// The id is not decoration: it is the only thing that lets a later reply
    /// point at this message, so a backend that drops it removes the gateway's
    /// own half of a conversation from the reply chain.
    async fn send(&mut self, args: SendArgs) -> Result<SentArgs>;

    /// Accept one credential and authenticate with it, answering the identity
    /// the platform reports back (a bot username, an app name) when it names
    /// one.
    ///
    /// A backend that already holds its own credential answers `Ok(None)`
    /// without contacting the platform, so a caller that logs in out of habit
    /// is not an error. The default is that shape: only a backend that asked
    /// for a credential implements the call.
    async fn login(&mut self, _args: LoginArgs) -> Result<Option<String>> {
        Ok(None)
    }

    /// The conversations this backend can see on the platform.
    ///
    /// An empty list means the platform gives no listing, which is not a
    /// failure: an operator names the conversation id by hand instead.
    async fn list_conversations(&mut self) -> Result<Vec<ImConversation>> {
        Ok(Vec::new())
    }

    /// The backend's own health, for the core to record.
    fn health(&self) -> HealthArgs {
        HealthArgs::default()
    }

    /// Stop the backend cleanly.
    async fn stop(&mut self) -> Result<()> {
        Ok(())
    }
}
