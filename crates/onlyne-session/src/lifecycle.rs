//! Pure session-lifecycle core for swarm sessions.
//!
//! This module holds the orthogonal state dimensions, the versioned event type,
//! and a total reducer over them. It is deliberately free of scheduler,
//! database, transport, and backend dependencies: callers feed observations
//! and events, the reducer returns a verdict.
//!
//! State dimensions (docs/SWARM-REFACTOR-GRILLME.md §2.2):
//! - `AgentState`     — process-side agent fact reported by Pi/pi-onlyne.
//! - `DeliveryState`  — intent delivery fact for the current turn exit.
//! - `ResourceState`  — backend resource fact (pane/tab/terminal).
//! - `RecoveryState`  — recovery substate of an idle/draining session.
//!
//! The task's result (`TaskState`: pending/done/failed/cancelled) is not a
//! dimension of this tuple. The task ledger owns it; `project` reads it as an
//! argument, so a session row no longer doubles as the task record and the
//! reducer never has to invent a receipt to go with a result.
//!
//! Public projection (`PublicLifecycle`) is derived on demand from the four
//! dimensions plus the caller's task state. Nothing stores it: a reader that
//! wants the public view calls `project` on the tuple it just read.
//!
//! Versioning (§2.3): every observation and event carries `(generation, seq)`.
//! The reducer gates events on the current version watermark:
//! - stale generation or stale/duplicate seq: `Ignored` with a diagnostic,
//!   current state kept.
//! - same-generation duplicate event id: `Ignored` (idempotent replay).
//! - a newer generation arrives only through `AdoptNewGeneration` (old
//!   generation already gone) or `Supersede` (operator repair with attested
//!   dead old generation); every other newer-generation event is `Rejected`.
//! - a new generation while the previous one is still live is `Rejected`
//!   (duplicate live generation); the core keeps the first generation.
//!
//! §3.3: same-task duplicate Pi processes are gated by these adoption rules;
//! post-`Gone` sessions only accept adoption, supersede, and heartbeat.

mod event;
mod project;
mod reduce;
mod state;

pub use event::{IgnoredReason, LifecycleEvent, RejectReason, Verdict, event_version};
pub use project::{is_legal, project};
pub use reduce::apply;
pub use state::{
    AgentState, DeliveryState, Observation, PublicLifecycle, RecoveryState, ResourceState,
    TaskState, Version,
};

#[cfg(test)]
mod tests;
