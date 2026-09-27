//! The session mirror's row shape and the port its writers go through.
//!
//! [`SessionRecord`], [`VersionedSession`], and [`FaultRecord`] mirror the
//! columns the `sessions` and `faults` tables carry in both databases, and
//! [`SessionLedger`] is the seam the session reducer's persistence bridge writes
//! them through. This crate implements the port; the client's reconcile module
//! drives it.
//!
//! A stored session row answers for a session, which is the table's key in both
//! databases; the delivery a session serves is a binding of its own. The port
//! stays delivery-addressed because that is how the reducer holds a session —
//! one tuple per delivery it opened — and the implementation resolves the
//! delivery to its session through the binding.

use serde::{Deserialize, Serialize};

/// The only persistence surface the session bridge needs. The write gate is
/// monotonic: `upsert_session` applies a row only when its `(generation, seq)` is
/// strictly newer than the stored watermark, and reports whether the row changed.
pub trait SessionLedger: Send + Sync {
    /// Load the session row serving one delivery.
    fn get_session(&self, task_id: &str) -> anyhow::Result<Option<SessionRecord>>;
    /// Monotonic session upsert, which also opens the delivery's binding.
    /// Returns true when the row changed.
    fn upsert_session(&self, task_id: &str, version: &VersionedSession) -> anyhow::Result<bool>;
    /// Whether the task itself is tracked, so a stray report cannot conjure a row.
    fn task_is_known(&self, task_id: &str) -> anyhow::Result<bool>;
    /// Attempt count for a task, for the fault audit entry.
    fn task_attempt(&self, task_id: &str) -> anyhow::Result<i64>;
    /// Existing faults for one task, for the `(task_id, kind, generation)` dedupe.
    fn list_faults(&self, task_id: &str) -> anyhow::Result<Vec<FaultRecord>>;
    /// Append one fault row. Returns its id.
    fn insert_fault(&self, fault: &FaultRecord) -> anyhow::Result<i64>;
    /// Observe an emitted event. The bridge never reads back from this hook.
    fn emit(&self, kind: &str, data: serde_json::Value);
    /// Observe an operator-visible alert line.
    fn note_alert(&self, line: String);
}

/// One stored session tuple, as the ledger columns hold it. No public view is
/// among them: the projection is derived by whoever asks, from these columns
/// plus the task state that caller owns.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SessionRecord {
    /// The delivery this session serves, as the caller named it. Not the row's
    /// key: the session id is.
    pub task_id: String,
    pub agent_state: String,
    pub delivery_state: String,
    pub resource_state: String,
    pub recovery_substate: String,
    pub desired_json: String,
    pub observed_json: String,
    pub generation: i64,
    pub seq: i64,
    pub backend_ref: String,
    pub mismatch_count: i64,
    pub updated_at: i64,
}

/// One projected row ready for the ledger write gate.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct VersionedSession {
    pub agent_state: String,
    pub delivery_state: String,
    pub resource_state: String,
    pub recovery_substate: String,
    pub desired_json: String,
    pub observed_json: String,
    pub generation: i64,
    pub seq: i64,
    pub backend_ref: String,
    pub mismatch_count: i64,
    pub updated_at: i64,
}

/// One fault queue row.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FaultRecord {
    pub id: i64,
    pub task_id: String,
    pub session_id: String,
    pub generation: i64,
    pub seq: i64,
    pub desired_json: String,
    pub observed_json: String,
    pub intent: String,
    pub attempt: i64,
    pub backend_ref: String,
    pub kind: String,
    pub reason: String,
    pub state: String,
    pub created_at: i64,
}
