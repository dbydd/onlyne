//! The trait a platform backend implements.
//!
//! A backend is the *only* place platform SDKs live. It answers two questions —
//! "what happened on the platform" and "send this standard payload" — and knows
//! nothing about Onlyne roles, sessions, or routing.

use crate::frame::{HealthArgs, HelloArgs, SendArgs};
use crate::payload::ImEvent;
use anyhow::Result;
use async_trait::async_trait;

/// What a backend can do. Absence is a declared gap the core degrades around.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct BackendCapabilities {
    /// The backend reports message events.
    pub message: bool,
    /// The backend reports button-action events.
    pub action: bool,
    /// The backend renders a typing indicator.
    pub typing: bool,
}

#[async_trait]
pub trait Backend: Send + Sync {
    /// What this backend bridges and what it can do.
    fn hello(&self) -> HelloArgs;

    /// Block until one platform event arrives, or `None` when the backend is
    /// done (the platform closed it).
    async fn next_event(&mut self) -> Result<Option<ImEvent>>;

    /// Render and send one standard payload into a conversation.
    async fn send(&mut self, args: SendArgs) -> Result<()>;

    /// The backend's own health, for the core to record.
    fn health(&self) -> HealthArgs {
        HealthArgs::default()
    }

    /// Stop the backend cleanly.
    async fn stop(&mut self) -> Result<()> {
        Ok(())
    }
}
