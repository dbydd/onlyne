//! Onlyne ACP client: [Agent Client Protocol][acp] v1 over stdio.
//!
//! [acp]: https://agentclientprotocol.com
//!
//! This crate owns one child process that speaks ACP and the sessions negotiated
//! on it. It is synchronous and thread-based: no async runtime, no tokio, and no
//! task- local state, because the caller (`onlyne-session`'s exec-style backends)
//! parks a thread per turn already.
//!
//! # Shape
//!
//! ```text
//! Agent ── stdin ──▶  qoderclicn --acp  ── stdout ──▶ reader thread ──▶ Event fan-out
//!   ▲                      (own process group)                             │
//!   └──── one pending-request map keyed by JSON-RPC id ◀───────────────────┘
//! ```
//!
//! * [`Agent::start`] spawns the process and its three helper threads (a reader, a
//!   background writer that owns stdin, a stderr drain) but speaks no protocol.
//! * [`Agent::initialize`] is the handshake and the only thing that may be sent
//!   first; every session call refuses until it completes.
//! * [`Agent::new_session`] opens a session; one process hosts many, and each
//!   session's traffic is keyed by the id the agent chose.
//! * [`Agent::prompt`] parks its caller until the turn ends, streaming
//!   [`Event::Update`] meanwhile. There is no timeout in this crate: it detects and
//!   reports, and a supervisor decides how long a turn gets.
//! * [`Agent::subscribe`] must be called before the first prompt.
//!   `session/request_permission` is an agent-to-client *request* — the agent parks
//!   its turn on it — and a caller that has not subscribed cannot answer it.
//!
//! # What this crate refuses
//!
//! Agent-to-client requests other than `session/request_permission` are answered
//! with JSON-RPC `-32601` naming the method, so an agent falls back to its own
//! default rather than hanging: a client that does not proxy a filesystem or a
//! terminal says so on the wire. A permission request that arrives with no
//! subscriber is answered `cancelled`, which is the protocol's own "no decision",
//! and the fact is logged — never silently granted.
//!
//! # Errors
//!
//! Every call returns [`anyhow::Result`]. An agent's JSON-RPC error reply is
//! recoverable as [`RpcError`] with [`anyhow::Error::downcast_ref`], which is how a
//! caller tells `-32601` (the agent lacks the method) from `-32000` (the agent wants
//! a login) from a dead pipe. A frame that cannot be decoded is logged and skipped;
//! the stream stays open, because one bad line is not a reason to lose a live agent.
//!
//! ```no_run
//! use onlyne_acp::{Agent, AgentOptions, ClientCapabilities, ClientInfo, ContentBlock, Event};
//! use std::path::Path;
//!
//! # fn main() -> anyhow::Result<()> {
//! let agent = Agent::start(AgentOptions::new(vec![
//!     "qoderclicn".into(),
//!     "--acp".into(),
//! ]))?;
//! let negotiated = agent.initialize(ClientInfo::new("onlyne-client"), ClientCapabilities::default())?;
//! let events = agent.subscribe();
//! let session = agent.new_session(Path::new("/work/tree"), Vec::new())?;
//! let outcome = agent.prompt(&session.session_id, vec![ContentBlock::text("summarise the tree")])?;
//! while let Ok(event) = events.try_recv() {
//!     if let Event::Update { update, .. } = event {
//!         print!("{}", update.text().unwrap_or_default());
//!     }
//! }
//! assert_eq!(outcome.stop_reason, onlyne_acp::PromptOutcome::END_TURN);
//! agent.shutdown()?;
//! # Ok(())
//! # }
//! ```

mod agent;
mod rpc;
mod types;
mod wire;

pub use agent::{Agent, AgentOptions, PROTOCOL_VERSION};
pub use types::{
    AgentCapabilities, ClientCapabilities, ClientInfo, ContentBlock, ContentChunk, EnvVariable,
    Event, FsClientCapabilities, HttpHeader, McpServer, PermissionOption, PermissionOutcome,
    PermissionRequest, PromptOutcome, SessionStart, TerminalClientCapabilities, Update,
};
pub use wire::{RequestId, RpcError};
