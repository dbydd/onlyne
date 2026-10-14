//! The IM gateway core.
//!
//! A special client ([`docs/v2.2-IM-GATEWAY-PLAN.md`]): one gateway root, one
//! process, holding one ordinary role link per IM conversation — every
//! conversation is a directory under `conversations/` whose name is its role —
//! handing inbound events to operator-configured trigger scripts, and asking a
//! platform backend to render outbound payloads. Each conversation keeps its
//! own runtime files (its chat log, its attachments) inside its own workspace.
//!
//! [`docs/v2.2-IM-GATEWAY-PLAN.md`]: ../../../docs/v2.2-IM-GATEWAY-PLAN.md

pub mod adopt;
pub mod backend_host;
pub mod chatkey;
pub mod chatlog;
pub mod config;
pub mod history;
pub mod link;
pub mod runloop;
pub mod thread;
pub mod trigger;

pub use chatkey::mint as mint_chat_key;
pub use config::{BackendEntry, Conversation, Gateway, GatewayFile, PushMode, PushScript};
pub use runloop::{RunOptions, run};

pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}
