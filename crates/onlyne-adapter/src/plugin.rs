//! In-process gateway plugin interfaces.

use async_trait::async_trait;
use onlyne_proto::{Capability, ConversationInfo, Envelope, HealthArgs, ImagePart, MsgKind, RegisterChannelArgs, TypingArgs};

use crate::{AdapterError, Result};

/// A message that a gateway plugin renders and sends to its platform.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outbound {
    pub conversation: String,
    pub text: String,
    pub image: Option<ImagePart>,
    pub reply_to: Option<String>,
    pub kind: MsgKind,
}

/// Platform-specific receipt for a sent message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SendReceipt {
    pub external_id: String,
}

/// Platform onboarding information.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OnboardingPrompt {
    pub kind: OnboardingKind,
    pub payload: String,
    pub expires_in: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OnboardingKind {
    Qr,
    ManualCode,
}

/// Health observation from a gateway plugin.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdapterHealth {
    pub state: String,
    pub detail: Option<String>,
    pub uptime_s: u64,
}

impl From<HealthArgs> for AdapterHealth {
    fn from(value: HealthArgs) -> Self {
        AdapterHealth {
            state: value.state,
            detail: value.detail,
            uptime_s: value.uptime_s,
        }
    }
}

#[async_trait]
pub trait GatewayPlugin: Send {
    fn platform(&self) -> &'static str;
    fn capabilities(&self) -> Vec<Capability>;
    async fn start(&mut self, host: &mut dyn GatewayHost) -> Result<(), AdapterError>;
    async fn send(&mut self, msg: &Outbound) -> Result<SendReceipt, AdapterError>;
    async fn probe(&mut self) -> Result<AdapterHealth, AdapterError>;
    async fn stop(&mut self, reason: &str) -> Result<(), AdapterError>;
    fn onboarding(&mut self) -> Result<Option<OnboardingPrompt>, AdapterError> {
        Ok(None)
    }
    async fn list_conversations(&mut self) -> Result<Vec<ConversationInfo>, AdapterError> {
        Ok(vec![])
    }
}

#[async_trait]
pub trait GatewayHost: Send {
    async fn deliver_inbound(&mut self, envelope: &Envelope) -> Result<(), AdapterError>;
    async fn report_health(&mut self, health: &HealthArgs) -> Result<(), AdapterError>;
    async fn register_channel(&mut self, args: &RegisterChannelArgs) -> Result<(), AdapterError>;
    async fn typing(&mut self, args: &TypingArgs) -> Result<(), AdapterError>;
}

pub use crate::{AgentHandle, GatewayHandle};
