use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::lifecycle::{
    self, AgentState, DeliveryState, Observation, RecoveryState, ResourceState, Version,
};

use super::bridge::{Bridge, initial_observation};
use super::fault::{DEFAULT_ISOLATE_AFTER, DEFAULT_TERMINATE_AFTER};
use super::ledger::SessionLedger;

/// One stored session tuple, as the ledger columns hold it. No public view is
/// among them: the projection is derived by whoever asks, from these columns
/// plus the task state that caller owns.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SessionRecord {
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

/// What recording one fault produced: the queue row id when the dedupe gate
/// let it through.
#[derive(Debug, Clone, Default, Serialize)]
pub struct FaultOutcome {
    /// `None` when the dedupe gate found this generation already queued.
    pub fault_id: Option<i64>,
}

impl FaultOutcome {
    pub(super) fn recorded(&self) -> bool {
        self.fault_id.is_some()
    }
}

/// Serialize a lifecycle enum into the short tag its ledger column stores.
fn tag<T: Serialize>(value: &T) -> anyhow::Result<String> {
    match serde_json::to_value(value)? {
        serde_json::Value::String(text) => Ok(text),
        other => anyhow::bail!("lifecycle state must serialize to a string: {other}"),
    }
}

/// Ledger counters are `i64`, the reducer's are `u64`, so the conversion is
/// lossless for anything the protocol accepts and saturates on a corrupt row.
fn counter(value: u64) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}

pub(super) fn now_unix() -> i64 {
    chrono::Utc::now().timestamp()
}

pub(super) fn short(task_id: &str) -> &str {
    &task_id[..8.min(task_id.len())]
}

/// Project a reducer observation into one ledger row. `desired_json` records the
/// event that was accepted: the reducer has no separate desired model yet, so
/// the accepted intent is the honest audit entry next to the observed tuple.
pub fn to_versioned(
    obs: &Observation,
    backend_ref: &str,
    desired_json: &str,
) -> anyhow::Result<VersionedSession> {
    Ok(VersionedSession {
        agent_state: tag(&obs.agent)?,
        delivery_state: tag(&obs.delivery)?,
        resource_state: tag(&obs.resource)?,
        recovery_substate: tag(&obs.recovery)?,
        desired_json: desired_json.to_string(),
        observed_json: serde_json::to_string(obs)?,
        generation: counter(obs.version.generation),
        seq: counter(obs.version.seq),
        backend_ref: backend_ref.to_string(),
        mismatch_count: counter(u64::from(obs.mismatch_count)),
        updated_at: now_unix(),
    })
}

/// `backend_ref` names the backend resource the tuple was observed on. The live
/// in-memory `SessionRef` is the freshest answer and is stored whole, so a
/// restart can rebuild a probe target from the row alone. Without an in-memory
/// session the previous reference survives: an event never invents a resource.
pub(super) fn backend_ref_json(
    bridge: &Bridge,
    task_id: &str,
    row: Option<&SessionRecord>,
) -> String {
    if let Some(session) = bridge.live.lock().unwrap().get(task_id) {
        if let Ok(json) = serde_json::to_string(session) {
            return json;
        }
    }
    if let Some(row) = row {
        let stored = row.backend_ref.trim();
        if !stored.is_empty() && stored != "{}" {
            return stored.to_string();
        }
    }
    "{}".to_string()
}

/// Decode the stored tuple, stamped with the row's column watermark.
///
/// The columns are the one authority on where a session's watermark stands. The
/// write gate compares them (`upsert_session` refuses anything not strictly
/// newer), `bump_session_version` advances them without touching the tuple's
/// bytes, and the corrupt-row path below rebuilds from them. Reading a parsed
/// tuple's own embedded `version` instead made the two ends of one gate disagree
/// as soon as a heartbeat landed in the no-op bump: the tuple still carried the
/// older sequence, so every local write the same client then allocated was
/// refused by a watermark its own reader never saw. A live session whose agent
/// had died could then never publish its exit, and `onlyne sessions` kept reading
/// `working` beside a task row already refused `session_dead`.
///
/// A row whose `observed_json` is unparsable or is not a legal observation is
/// rebuilt from `Observation::initial` at the row's own watermark: the reducer's
/// protection against stale events is the watermark, and rewinding it would let
/// an old report overwrite newer truth.
pub fn stored_observation(ledger: &dyn SessionLedger, row: Option<&SessionRecord>) -> Observation {
    let Some(row) = row else {
        return initial_observation();
    };
    match serde_json::from_str::<Observation>(&row.observed_json) {
        Ok(obs) if lifecycle::is_legal(&obs) => Observation {
            version: Version::new(
                if row.generation > 0 {
                    row.generation as u64
                } else {
                    obs.version.generation
                },
                row.seq.max(0) as u64,
            ),
            ..obs
        },
        Ok(obs) => corrupt_observation(ledger, row, &format!("illegal tuple {obs:?}")),
        Err(err) => corrupt_observation(ledger, row, &err.to_string()),
    }
}

fn corrupt_observation(
    ledger: &dyn SessionLedger,
    row: &SessionRecord,
    reason: &str,
) -> Observation {
    tracing::warn!(
        task = %row.task_id,
        reason,
        "session row is corrupt; rebuilt the reducer watermark from its columns"
    );
    ledger.note_alert(format!(
        "session {} observed_json is corrupt",
        short(&row.task_id)
    ));
    ledger.emit(
        "lifecycle_corrupt",
        json!({"task_id": row.task_id, "reason": reason}),
    );
    Observation::build(
        Version::new(
            if row.generation > 0 {
                row.generation as u64
            } else {
                1
            },
            row.seq.max(0) as u64,
        ),
        true,
        DEFAULT_ISOLATE_AFTER,
        DEFAULT_TERMINATE_AFTER,
        row.mismatch_count.clamp(0, i64::from(u32::MAX)) as u32,
        AgentState::Booting,
        DeliveryState::None,
        ResourceState::Detached,
        RecoveryState::None,
    )
}
