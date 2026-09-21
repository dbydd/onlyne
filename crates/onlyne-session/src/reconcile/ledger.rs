use std::collections::HashMap;
use std::sync::Mutex;

use crate::backend::SessionRef;

use super::record::{FaultRecord, SessionRecord, VersionedSession};

/// The only persistence surface the bridge needs. A later phase implements it
/// against the store crate. The write gate is monotonic: `upsert_session`
/// applies a row only when its `(generation, seq)` is strictly newer than the
/// stored watermark, and reports whether the row changed.
pub trait SessionLedger: Send + Sync {
    /// Load one session row.
    fn get_session(&self, task_id: &str) -> anyhow::Result<Option<SessionRecord>>;
    /// Monotonic session upsert. Returns true when the row changed.
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

/// In-memory [`SessionLedger`] for tests and for callers that hold the live
/// [`SessionRef`] map next to the bridge.
#[derive(Debug, Default)]
pub struct MemoryLedger {
    sessions: Mutex<HashMap<String, (SessionRecord, VersionedSession)>>,
    known: Mutex<HashMap<String, i64>>,
    faults: Mutex<Vec<FaultRecord>>,
    events: Mutex<Vec<(String, serde_json::Value)>>,
    alerts: Mutex<Vec<String>>,
    next_fault_id: Mutex<i64>,
}

impl MemoryLedger {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a task the bridge may seed a row for, with its attempt count.
    pub fn track_task(&self, task_id: &str, attempt: i64) {
        self.known
            .lock()
            .unwrap()
            .insert(task_id.to_string(), attempt);
    }

    /// Insert a live session ref the bridge prefers over the stored reference.
    pub fn track_session(&self, session: SessionRef) {
        let mut sessions = self.sessions.lock().unwrap();
        let entry = sessions.entry(session.task_id.clone()).or_insert_with(|| {
            let record = SessionRecord {
                task_id: session.task_id.clone(),
                agent_state: String::new(),
                delivery_state: String::new(),
                resource_state: String::new(),
                recovery_substate: String::new(),
                desired_json: String::new(),
                observed_json: String::new(),
                generation: 0,
                seq: -1,
                backend_ref: String::new(),
                mismatch_count: 0,
                updated_at: 0,
            };
            let stored = VersionedSession {
                agent_state: String::new(),
                delivery_state: String::new(),
                resource_state: String::new(),
                recovery_substate: String::new(),
                desired_json: String::new(),
                observed_json: String::new(),
                generation: 0,
                seq: -1,
                backend_ref: String::new(),
                mismatch_count: 0,
                updated_at: 0,
            };
            (record, stored)
        });
        entry.0.backend_ref = serde_json::to_string(&session).unwrap_or_else(|_| "{}".into());
    }

    pub fn events(&self) -> Vec<(String, serde_json::Value)> {
        self.events.lock().unwrap().clone()
    }

    pub fn alerts(&self) -> Vec<String> {
        self.alerts.lock().unwrap().clone()
    }
}

impl SessionLedger for MemoryLedger {
    fn get_session(&self, task_id: &str) -> anyhow::Result<Option<SessionRecord>> {
        Ok(self
            .sessions
            .lock()
            .unwrap()
            .get(task_id)
            .map(|(record, _)| record.clone()))
    }

    fn upsert_session(&self, task_id: &str, version: &VersionedSession) -> anyhow::Result<bool> {
        let mut sessions = self.sessions.lock().unwrap();
        let entry = sessions.entry(task_id.to_string()).or_insert_with(|| {
            let record = SessionRecord {
                task_id: task_id.to_string(),
                agent_state: String::new(),
                delivery_state: String::new(),
                resource_state: String::new(),
                recovery_substate: String::new(),
                desired_json: String::new(),
                observed_json: String::new(),
                generation: 0,
                seq: -1,
                backend_ref: String::new(),
                mismatch_count: 0,
                updated_at: 0,
            };
            let stored = VersionedSession {
                agent_state: String::new(),
                delivery_state: String::new(),
                resource_state: String::new(),
                recovery_substate: String::new(),
                desired_json: String::new(),
                observed_json: String::new(),
                generation: 0,
                seq: -1,
                backend_ref: String::new(),
                mismatch_count: 0,
                updated_at: 0,
            };
            (record, stored)
        });
        let (record, _) = &*entry;
        let newer = version.generation > record.generation
            || (version.generation == record.generation && version.seq > record.seq);
        // An empty row (generation 0, seq -1) accepts the first write
        // unconditionally so a seed can land.
        let is_seed = record.generation == 0 && record.seq == -1;
        if !newer && !is_seed {
            return Ok(false);
        }
        entry.0 = SessionRecord {
            task_id: task_id.to_string(),
            agent_state: version.agent_state.clone(),
            delivery_state: version.delivery_state.clone(),
            resource_state: version.resource_state.clone(),
            recovery_substate: version.recovery_substate.clone(),
            desired_json: version.desired_json.clone(),
            observed_json: version.observed_json.clone(),
            generation: version.generation,
            seq: version.seq,
            backend_ref: version.backend_ref.clone(),
            mismatch_count: version.mismatch_count,
            updated_at: version.updated_at,
        };
        entry.1 = version.clone();
        Ok(true)
    }

    fn task_is_known(&self, task_id: &str) -> anyhow::Result<bool> {
        let known = self.known.lock().unwrap();
        if known.contains_key(task_id) {
            return Ok(true);
        }
        Ok(self.sessions.lock().unwrap().contains_key(task_id))
    }

    fn task_attempt(&self, task_id: &str) -> anyhow::Result<i64> {
        Ok(self
            .known
            .lock()
            .unwrap()
            .get(task_id)
            .copied()
            .unwrap_or(0))
    }

    fn list_faults(&self, task_id: &str) -> anyhow::Result<Vec<FaultRecord>> {
        Ok(self
            .faults
            .lock()
            .unwrap()
            .iter()
            .filter(|f| f.task_id == task_id)
            .cloned()
            .collect())
    }

    fn insert_fault(&self, fault: &FaultRecord) -> anyhow::Result<i64> {
        let mut next = self.next_fault_id.lock().unwrap();
        *next += 1;
        let id = *next;
        let mut faults = self.faults.lock().unwrap();
        faults.push(FaultRecord {
            id,
            ..fault.clone()
        });
        Ok(id)
    }

    fn emit(&self, kind: &str, data: serde_json::Value) {
        self.events.lock().unwrap().push((kind.to_string(), data));
    }

    fn note_alert(&self, line: String) {
        self.alerts.lock().unwrap().push(line);
    }
}
