//! Session persistence bridge: the reducer facts in, one ledger trait out.
//!
//! The bridge reduces one [`LifecycleEvent`](crate::lifecycle::LifecycleEvent)
//! against the stored tuple for a task and persists the result. Writes happen
//! on `Applied` only, so a stale or illegal report cannot move the ledger
//! backwards. Every read and write of the session ledger goes through
//! [`SessionLedger`]; the store crate implements it against SQLite.

use std::collections::HashMap;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::backend::SessionRef;
use crate::lifecycle::{
    self, AgentState, DeliveryState, IgnoredReason, LifecycleEvent, Observation, Outcome,
    PublicLifecycle, RecoveryState, ResourceState, Verdict, Version,
};

/// Consecutive reconcile mismatches tolerated before the session is isolated.
pub const DEFAULT_ISOLATE_AFTER: u32 = 1;
/// Consecutive mismatches tolerated before the generation is terminated and the
/// unsettled work is recorded as a fault.
pub const DEFAULT_TERMINATE_AFTER: u32 = 3;

/// One stored session tuple, as the ledger columns hold it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SessionRecord {
    pub task_id: String,
    pub agent_state: String,
    pub delivery_state: String,
    pub resource_state: String,
    pub public_lifecycle: String,
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
    pub public_lifecycle: String,
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
    fn recorded(&self) -> bool {
        self.fault_id.is_some()
    }
}

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
        let entry = sessions
            .entry(session.task_id.clone())
            .or_insert_with(|| {
                let record = SessionRecord {
                    task_id: session.task_id.clone(),
                    agent_state: String::new(),
                    delivery_state: String::new(),
                    resource_state: String::new(),
                    public_lifecycle: String::new(),
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
                    public_lifecycle: String::new(),
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
                public_lifecycle: String::new(),
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
                public_lifecycle: String::new(),
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
            public_lifecycle: version.public_lifecycle.clone(),
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
        Ok(self.known.lock().unwrap().get(task_id).copied().unwrap_or(0))
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
        faults.push(FaultRecord { id, ..fault.clone() });
        Ok(id)
    }

    fn emit(&self, kind: &str, data: serde_json::Value) {
        self.events
            .lock()
            .unwrap()
            .push((kind.to_string(), data));
    }

    fn note_alert(&self, line: String) {
        self.alerts.lock().unwrap().push(line);
    }
}

/// A bridge instance: the reducer facts plus the ledger and the live session
/// map it resolves probe targets from.
#[derive(Debug, Default)]
pub struct Bridge {
    live: Mutex<HashMap<String, SessionRef>>,
}

impl Bridge {
    pub fn new() -> Self {
        Self::default()
    }

    /// Remember a live session ref. The bridge prefers it over the stored
    /// reference when it names the backend resource.
    pub fn track_live(&self, session: SessionRef) {
        self.live
            .lock()
            .unwrap()
            .insert(session.task_id.clone(), session);
    }

    /// Forget a live session ref.
    pub fn untrack_live(&self, task_id: &str) {
        self.live.lock().unwrap().remove(task_id);
    }
}

fn initial_observation() -> Observation {
    Observation::initial(DEFAULT_ISOLATE_AFTER, DEFAULT_TERMINATE_AFTER)
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

fn now_unix() -> i64 {
    chrono::Utc::now().timestamp()
}

fn short(task_id: &str) -> &str {
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
        public_lifecycle: tag(&obs.public)?,
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
fn backend_ref_json(
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

/// Decode the stored tuple. A row whose `observed_json` is unparsable or is not
/// a legal observation is rebuilt from `Observation::initial` at the row's own
/// watermark: the reducer's protection against stale events is the watermark,
/// and rewinding it would let an old report overwrite newer truth.
pub fn stored_observation(
    ledger: &dyn SessionLedger,
    row: Option<&SessionRecord>,
) -> Observation {
    let Some(row) = row else {
        return initial_observation();
    };
    match serde_json::from_str::<Observation>(&row.observed_json) {
        Ok(obs) if lifecycle::is_legal(&obs) => obs,
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
        Outcome::Pending,
    )
}

/// The version an event carries.
fn version_of(event: &LifecycleEvent) -> Version {
    match event {
        LifecycleEvent::Created { v }
        | LifecycleEvent::Ready { v }
        | LifecycleEvent::TurnStarted { v }
        | LifecycleEvent::TurnEnded { v }
        | LifecycleEvent::Heartbeat { v, .. }
        | LifecycleEvent::Complete { v }
        | LifecycleEvent::IntentPending { v }
        | LifecycleEvent::IntentRetry { v }
        | LifecycleEvent::IntentReceipt { v }
        | LifecycleEvent::IntentExhausted { v }
        | LifecycleEvent::ResourceAttach { v }
        | LifecycleEvent::ResourceCloseRequested { v }
        | LifecycleEvent::ResourceClosed { v }
        | LifecycleEvent::AgentGone { v }
        | LifecycleEvent::Cancel { v }
        | LifecycleEvent::Fail { v }
        | LifecycleEvent::ReconcileMismatch { v }
        | LifecycleEvent::ReconcileOk { v }
        | LifecycleEvent::AdoptNewGeneration { v }
        | LifecycleEvent::Supersede { v, .. } => *v,
    }
}

fn event_name(event: &LifecycleEvent) -> &'static str {
    match event {
        LifecycleEvent::Created { .. } => "created",
        LifecycleEvent::Ready { .. } => "ready",
        LifecycleEvent::TurnStarted { .. } => "turn_started",
        LifecycleEvent::TurnEnded { .. } => "turn_ended",
        LifecycleEvent::Heartbeat { .. } => "heartbeat",
        LifecycleEvent::Complete { .. } => "complete",
        LifecycleEvent::IntentPending { .. } => "intent_pending",
        LifecycleEvent::IntentRetry { .. } => "intent_retry",
        LifecycleEvent::IntentReceipt { .. } => "intent_receipt",
        LifecycleEvent::IntentExhausted { .. } => "intent_exhausted",
        LifecycleEvent::ResourceAttach { .. } => "resource_attach",
        LifecycleEvent::ResourceCloseRequested { .. } => "resource_close_requested",
        LifecycleEvent::ResourceClosed { .. } => "resource_closed",
        LifecycleEvent::AgentGone { .. } => "agent_gone",
        LifecycleEvent::Cancel { .. } => "cancel",
        LifecycleEvent::Fail { .. } => "fail",
        LifecycleEvent::ReconcileMismatch { .. } => "reconcile_mismatch",
        LifecycleEvent::ReconcileOk { .. } => "reconcile_ok",
        LifecycleEvent::AdoptNewGeneration { .. } => "adopt_new_generation",
        LifecycleEvent::Supersede { .. } => "supersede",
    }
}

/// Reduce `event` against the stored tuple for `task_id` and persist the result.
///
/// `Applied` writes the row (unless a concurrent writer already carries a newer
/// watermark) and emits one `lifecycle` event describing the transition.
/// `Ignored` and `Rejected` leave the ledger untouched and are logged with the
/// reducer's reason.
///
/// `Created` is the one event whose meaning at this boundary is the row itself:
/// `Observation::initial` already is the post-`Created` tuple, so for a task
/// with no stored row it seeds the row at the event's version. Use
/// [`feed_created`] for the idempotent form.
pub fn apply_persist(
    bridge: &Bridge,
    ledger: &dyn SessionLedger,
    task_id: &str,
    event: &LifecycleEvent,
) -> anyhow::Result<Verdict> {
    let row = ledger.get_session(task_id)?;
    if row.is_none() {
        let known = ledger.task_is_known(task_id)? || bridge.live.lock().unwrap().contains_key(task_id);
        if !known {
            tracing::warn!(
                task = %task_id,
                event = event_name(event),
                "lifecycle event for a session never tracked; nothing persisted"
            );
            anyhow::bail!("unknown session {task_id}");
        }
    }
    let current = stored_observation(ledger, row.as_ref());
    if row.is_none() && matches!(event, LifecycleEvent::Created { .. }) {
        return Ok(Verdict::Applied(seed_created(
            bridge,
            ledger,
            task_id,
            version_of(event),
        )?));
    }
    let verdict = lifecycle::apply(&current, event);
    record_verdict(bridge, ledger, task_id, row.as_ref(), event, &current, verdict)?;
    Ok(verdict)
}

/// Write the `Created` seed row. The reducer defines no transition into
/// `Booting` from `Booting` — `initial()` is already the created tuple — so the
/// seed is written directly at the event's version.
fn seed_created(
    bridge: &Bridge,
    ledger: &dyn SessionLedger,
    task_id: &str,
    version: Version,
) -> anyhow::Result<Observation> {
    let mut seeded = initial_observation();
    seeded.version = version;
    let backend_ref = backend_ref_json(bridge, task_id, None);
    let desired = serde_json::to_string(&LifecycleEvent::Created { v: version })?;
    let stored = to_versioned(&seeded, &backend_ref, &desired)?;
    if ledger.upsert_session(task_id, &stored)? {
        ledger.emit(
            "lifecycle",
            transition_payload(task_id, &seeded, &seeded, "created"),
        );
    } else {
        tracing::warn!(
            task = %task_id,
            "a newer session row appeared while seeding Created; kept the newer watermark"
        );
    }
    Ok(seeded)
}

/// One transition on the bus: both projections plus the full tuple the reducer
/// settled on, so a subscriber never has to re-derive `public`.
fn transition_payload(
    task_id: &str,
    from: &Observation,
    to: &Observation,
    event: &str,
) -> serde_json::Value {
    json!({
        "task_id": task_id,
        "from": from.public,
        "to": to.public,
        "agent": to.agent,
        "delivery": to.delivery,
        "resource": to.resource,
        "recovery": to.recovery,
        "outcome": to.outcome,
        "generation": to.version.generation,
        "seq": to.version.seq,
        "event": event,
    })
}

#[allow(clippy::too_many_arguments)]
fn record_verdict(
    bridge: &Bridge,
    ledger: &dyn SessionLedger,
    task_id: &str,
    row: Option<&SessionRecord>,
    event: &LifecycleEvent,
    current: &Observation,
    verdict: Verdict,
) -> anyhow::Result<()> {
    match verdict {
        Verdict::Applied(ref next) => {
            let backend_ref = backend_ref_json(bridge, task_id, row);
            let desired = serde_json::to_string(event)?;
            let stored = to_versioned(next, &backend_ref, &desired)?;
            if !ledger.upsert_session(task_id, &stored)? {
                tracing::warn!(
                    task = %task_id,
                    generation = next.version.generation,
                    seq = next.version.seq,
                    "session write lost to a newer watermark; left the row alone"
                );
                return Ok(());
            }
            tracing::debug!(
                task = %task_id,
                from = ?current.public,
                to = ?next.public,
                generation = next.version.generation,
                seq = next.version.seq,
                "lifecycle transition applied"
            );
            ledger.emit(
                "lifecycle",
                transition_payload(task_id, current, next, event_name(event)),
            );
        }
        Verdict::Ignored(reason) => {
            tracing::debug!(
                task = %task_id,
                reason = ?reason,
                event = event_name(event),
                generation = current.version.generation,
                seq = current.version.seq,
                "lifecycle event ignored; ledger left as-is"
            );
        }
        Verdict::Rejected(reason) => {
            tracing::warn!(
                task = %task_id,
                reason = ?reason,
                event = event_name(event),
                generation = current.version.generation,
                seq = current.version.seq,
                "lifecycle event rejected; ledger left as-is"
            );
        }
    }
    Ok(())
}

/// The next version for a session observed locally: same generation, one
/// sequence past the stored watermark.
pub fn next_version(
    ledger: &dyn SessionLedger,
    task_id: &str,
) -> anyhow::Result<Version> {
    let row = ledger.get_session(task_id)?;
    let current = stored_observation(ledger, row.as_ref());
    Ok(Version::new(
        current.version.generation,
        current.version.seq.saturating_add(1),
    ))
}

/// Reduce an event whose version the caller allocates itself. Every feed helper
/// below goes through here, so an event that arrives twice is dropped by the
/// reducer's watermark.
pub fn apply_at_next(
    bridge: &Bridge,
    ledger: &dyn SessionLedger,
    task_id: &str,
    make: impl FnOnce(Version) -> LifecycleEvent,
) -> anyhow::Result<Verdict> {
    let version = next_version(ledger, task_id)?;
    apply_persist(bridge, ledger, task_id, &make(version))
}

/// Best-effort feed for local observation points.
pub fn try_feed(
    bridge: &Bridge,
    ledger: &dyn SessionLedger,
    task_id: &str,
    make: impl FnOnce(Version) -> LifecycleEvent,
) {
    if let Err(err) = apply_at_next(bridge, ledger, task_id, make) {
        tracing::warn!(
            task = %task_id,
            error = %err,
            "could not record a lifecycle event in the session ledger"
        );
    }
}

/// Seed the session row for a task just given a resource. Idempotent: a row
/// that already exists keeps its own history.
pub fn feed_created(
    bridge: &Bridge,
    ledger: &dyn SessionLedger,
    task_id: &str,
) -> anyhow::Result<Verdict> {
    if ledger.get_session(task_id)?.is_some() {
        tracing::debug!(task = %task_id, "session row already exists; created is a no-op");
        return Ok(Verdict::Ignored(IgnoredReason::NoOp));
    }
    apply_at_next(bridge, ledger, task_id, |v| LifecycleEvent::Created { v })
}

/// The dispatch path proved both facts at once: the session is bound to its task
/// and the backend resource is attached.
pub fn feed_dispatched(bridge: &Bridge, ledger: &dyn SessionLedger, task_id: &str) {
    if let Err(err) = feed_created(bridge, ledger, task_id) {
        tracing::warn!(task = %task_id, error = %err, "could not seed the session row");
    }
    match feed_resource_attached(bridge, ledger, task_id) {
        Ok(verdict) => {
            if matches!(verdict, Verdict::Rejected(_)) {
                tracing::warn!(task = %task_id, ?verdict, "resource attach refused for a dispatched session");
            }
        }
        Err(err) => {
            tracing::warn!(task = %task_id, error = %err, "could not record the resource attach")
        }
    }
}

/// The backend resource for this session exists.
pub fn feed_resource_attached(
    bridge: &Bridge,
    ledger: &dyn SessionLedger,
    task_id: &str,
) -> anyhow::Result<Verdict> {
    apply_at_next(bridge, ledger, task_id, |v| {
        LifecycleEvent::ResourceAttach { v }
    })
}

/// The agent finished booting and its channel is reachable.
pub fn feed_ready(
    bridge: &Bridge,
    ledger: &dyn SessionLedger,
    task_id: &str,
) -> anyhow::Result<Verdict> {
    apply_at_next(bridge, ledger, task_id, |v| LifecycleEvent::Ready { v })
}

/// The current turn went from "may change state" to "may emit an artifact".
pub fn feed_turn_started(
    bridge: &Bridge,
    ledger: &dyn SessionLedger,
    task_id: &str,
) -> anyhow::Result<Verdict> {
    apply_at_next(bridge, ledger, task_id, |v| {
        LifecycleEvent::TurnStarted { v }
    })
}

/// The current turn ended with no completion receipt yet.
pub fn feed_turn_ended(
    bridge: &Bridge,
    ledger: &dyn SessionLedger,
    task_id: &str,
) -> anyhow::Result<Verdict> {
    apply_at_next(bridge, ledger, task_id, |v| LifecycleEvent::TurnEnded { v })
}

/// The resource was recycled: the resource is closed and the generation is over.
pub fn feed_resource_closed(
    bridge: &Bridge,
    ledger: &dyn SessionLedger,
    task_id: &str,
) -> anyhow::Result<Verdict> {
    apply_at_next(bridge, ledger, task_id, |v| {
        LifecycleEvent::ResourceClosed { v }
    })
}

/// The agent process is gone while the ledger may still owe a result.
pub fn feed_agent_gone(
    bridge: &Bridge,
    ledger: &dyn SessionLedger,
    task_id: &str,
) -> anyhow::Result<Verdict> {
    apply_at_next(bridge, ledger, task_id, |v| LifecycleEvent::AgentGone { v })
}

/// Work given up on: the outcome becomes failed while the generation stays
/// open, so a later resource close still finalizes the row.
pub fn feed_fail(
    bridge: &Bridge,
    ledger: &dyn SessionLedger,
    task_id: &str,
) -> anyhow::Result<Verdict> {
    apply_at_next(bridge, ledger, task_id, |v| LifecycleEvent::Fail { v })
}

/// Work the operator cancelled: the result is settled, the exit proceeds.
pub fn feed_cancel(
    bridge: &Bridge,
    ledger: &dyn SessionLedger,
    task_id: &str,
) -> anyhow::Result<Verdict> {
    apply_at_next(bridge, ledger, task_id, |v| LifecycleEvent::Cancel { v })
}

/// A reconcile fact disagreed with the tuple (probe, heartbeat, snapshot).
pub fn feed_mismatch(
    bridge: &Bridge,
    ledger: &dyn SessionLedger,
    task_id: &str,
) -> anyhow::Result<Verdict> {
    apply_at_next(bridge, ledger, task_id, |v| {
        LifecycleEvent::ReconcileMismatch { v }
    })
}

/// A reconcile fact confirmed the tuple, which is what clears `idle_waiting` and
/// `idle_fault` without moving any other dimension.
pub fn feed_reconcile_ok(
    bridge: &Bridge,
    ledger: &dyn SessionLedger,
    task_id: &str,
) -> anyhow::Result<Verdict> {
    apply_at_next(bridge, ledger, task_id, |v| {
        LifecycleEvent::ReconcileOk { v }
    })
}

/// The handoff was consumed: the completion intent opened and its receipt came
/// back accepted. Then the `done` result is mirrored, so the
/// later resource close settles on top of the recorded result.
pub fn feed_delivered(
    bridge: &Bridge,
    ledger: &dyn SessionLedger,
    task_id: &str,
) -> anyhow::Result<Verdict> {
    let written = apply_at_next(bridge, ledger, task_id, |v| LifecycleEvent::Complete { v })?;
    if matches!(written, Verdict::Applied(_)) {
        apply_at_next(bridge, ledger, task_id, |v| {
            LifecycleEvent::IntentReceipt { v }
        })?;
    }
    settle(bridge, ledger, task_id, Outcome::Done)
}

/// Mirror a settled task result into the session tuple.
///
/// `Outcome` is the ledger's dimension and the reducer accepts it only inside a
/// full `Heartbeat` snapshot carrying the tuple the caller just proved.
/// Legality stays the reducer's decision.
pub fn settle(
    bridge: &Bridge,
    ledger: &dyn SessionLedger,
    task_id: &str,
    outcome: Outcome,
) -> anyhow::Result<Verdict> {
    let row = ledger.get_session(task_id)?;
    let current = stored_observation(ledger, row.as_ref());
    let version = Version::new(
        current.version.generation,
        current.version.seq.saturating_add(1),
    );
    let body = settle_body(&current, outcome);
    apply_persist(
        bridge,
        ledger,
        task_id,
        &LifecycleEvent::Heartbeat { v: version, body },
    )
}

fn settle_body(obs: &Observation, outcome: Outcome) -> Observation {
    let agent = if obs.agent == AgentState::Booting {
        AgentState::Idle
    } else {
        obs.agent
    };
    let delivery = if outcome == Outcome::Done {
        DeliveryState::Accepted
    } else {
        obs.delivery
    };
    let recovery = if outcome == Outcome::Done && agent == AgentState::Idle && obs.generation_live
    {
        RecoveryState::Draining
    } else {
        RecoveryState::None
    };
    Observation::build(
        obs.version,
        obs.generation_live,
        obs.isolate_after,
        obs.terminate_after,
        obs.mismatch_count,
        agent,
        delivery,
        obs.resource,
        recovery,
        outcome,
    )
}

/// Record one divergence that survived a crash. Deduplicated on
/// `(task_id, kind, generation)` so a repeated pass cannot bury the queue.
/// Recovery task creation is out of scope here: the supervisor opens it as an
/// explicit `control` op from the recorded fault.
pub fn record_fault(
    ledger: &dyn SessionLedger,
    task_id: &str,
    kind: &str,
    intent: &str,
    reason: &str,
) -> anyhow::Result<FaultOutcome> {
    let row = ledger.get_session(task_id)?;
    let generation = row.as_ref().map(|r| r.generation).unwrap_or(0);
    if ledger
        .list_faults(task_id)?
        .iter()
        .any(|f| f.kind == kind && f.generation == generation)
    {
        tracing::debug!(task = %task_id, kind, "this generation's fault is already in the queue");
        return Ok(FaultOutcome::default());
    }
    let attempt = ledger.task_attempt(task_id)?;
    let keep = |pick: fn(&SessionRecord) -> String| {
        row.as_ref()
            .map(pick)
            .map(|text| text.trim().to_string())
            .filter(|text| !text.is_empty())
            .unwrap_or_else(|| "{}".to_string())
    };
    let fault = FaultRecord {
        id: 0,
        task_id: task_id.to_string(),
        session_id: task_id.to_string(),
        generation,
        seq: row.as_ref().map(|r| r.seq).unwrap_or(0),
        desired_json: keep(|r| r.desired_json.clone()),
        observed_json: keep(|r| r.observed_json.clone()),
        intent: intent.to_string(),
        attempt,
        backend_ref: keep(|r| r.backend_ref.clone()),
        kind: kind.to_string(),
        reason: reason.to_string(),
        state: "open".to_string(),
        created_at: now_unix(),
    };
    let id = ledger.insert_fault(&fault)?;
    tracing::warn!(task = %task_id, fault_id = id, kind, intent, reason, "recorded session fault");
    ledger.note_alert(format!("session fault {kind} on {}", short(task_id)));
    ledger.emit(
        "session_fault",
        json!({
            "fault_id": id,
            "task_id": task_id,
            "kind": kind,
            "intent": intent,
            "reason": reason,
            "generation": generation,
        }),
    );
    Ok(FaultOutcome { fault_id: Some(id) })
}

/// The resource is provably gone. Unsettled work fails first, then the
/// generation ends. Fault recording covers the divergence the ledger cannot
/// describe: work still owed when the generation ended. Returns true when a
/// fault row was recorded.
pub fn reconcile_dead(
    bridge: &Bridge,
    ledger: &dyn SessionLedger,
    task_id: &str,
    obs: &Observation,
    detail: &str,
) -> anyhow::Result<bool> {
    let reason = format!("backend resource gone: {detail}");
    tracing::warn!(task = %task_id, %reason, "open session lost its agent");
    let mut faulted = false;
    if obs.outcome == Outcome::Pending {
        let outcome = record_fault(ledger, task_id, "probe_dead", "reconcile:probe_dead", &reason)?;
        faulted = outcome.recorded();
        let verdict = feed_fail(bridge, ledger, task_id)?;
        let _ = verdict;
    }
    let verdict = feed_agent_gone(bridge, ledger, task_id)?;
    let _ = verdict;
    Ok(faulted)
}

/// The resource is alive and disagrees with the tuple. The reducer's own ladder
/// decides when to isolate and when to end the generation. Returns true when a
/// fault row was recorded at the terminate ceiling.
pub fn reconcile_mismatch(
    bridge: &Bridge,
    ledger: &dyn SessionLedger,
    task_id: &str,
    obs: &Observation,
    detail: &str,
) -> anyhow::Result<bool> {
    let reason = format!("backend disagrees with the session tuple: {detail}");
    tracing::warn!(
        task = %task_id,
        mismatch_count = obs.mismatch_count,
        isolate_after = obs.isolate_after,
        terminate_after = obs.terminate_after,
        %reason,
        "session reconcile mismatch"
    );
    let verdict = feed_mismatch(bridge, ledger, task_id)?;
    let at_the_limit = obs.mismatch_count.saturating_add(1) >= obs.terminate_after;
    let terminated = matches!(&verdict, Verdict::Applied(next)
        if next.public == PublicLifecycle::Exited && next.outcome == Outcome::Failed);
    if at_the_limit && terminated {
        let outcome = record_fault(ledger, task_id, "mismatch_terminate", "reconcile:mismatch", &reason)?;
        return Ok(outcome.recorded());
    }
    Ok(false)
}

/// Resolve the backend resource the stored tuple names: the live in-memory
/// session first, then the row's own `backend_ref` when it parses as a whole
/// `SessionRef` for this task. Anything else is inconclusive and yields `None`.
pub fn probe_target(
    bridge: &Bridge,
    task_id: &str,
    row: &SessionRecord,
) -> Option<SessionRef> {
    if let Some(session) = bridge.live.lock().unwrap().get(task_id) {
        return Some(session.clone());
    }
    if let Ok(session) = serde_json::from_str::<SessionRef>(&row.backend_ref) {
        if session.task_id == task_id {
            return Some(session);
        }
        tracing::warn!(
            task = %task_id,
            stored = %session.task_id,
            "session backend_ref names another task; refusing to probe it"
        );
        return None;
    }
    None
}

/// Reconcile one open session row against a probe result. A probe error, or a
/// row that names no resource, is inconclusive: the tuple stays exactly where
/// it is. This is the fault-recording branch of the old `hop_timeouts` shape:
/// a dead resource fails unsettled work and records the fault; replay lives
/// with the supervisor now.
pub fn reconcile_probe(
    bridge: &Bridge,
    ledger: &dyn SessionLedger,
    backend: &dyn crate::backend::SessionBackend,
    task_id: &str,
) -> anyhow::Result<ProbeVerdict> {
    let Some(row) = ledger.get_session(task_id)? else {
        return Ok(ProbeVerdict::Unknown);
    };
    let current = stored_observation(ledger, Some(&row));
    if current.public == PublicLifecycle::Exited {
        return Ok(ProbeVerdict::Exited);
    }
    let Some(session) = probe_target(bridge, task_id, &row) else {
        return Ok(ProbeVerdict::Unknown);
    };
    let probe = match backend.probe(&session) {
        Ok(probe) => probe,
        Err(err) => {
            tracing::warn!(
                task = %task_id,
                error = %err,
                "session probe was inconclusive; not judging the generation dead"
            );
            return Ok(ProbeVerdict::Unknown);
        }
    };
    let detail = match &probe.detail {
        Some(value) => value.to_string(),
        None => format!("alive={} attached={}", probe.alive, probe.attached),
    };
    if !probe.alive {
        let faulted = reconcile_dead(bridge, ledger, task_id, &current, &detail)?;
        return Ok(if faulted {
            ProbeVerdict::DeadFaulted
        } else {
            ProbeVerdict::Dead
        });
    }
    if !current.generation_live || current.agent == AgentState::Gone {
        let faulted = reconcile_mismatch(bridge, ledger, task_id, &current, &detail)?;
        return Ok(if faulted {
            ProbeVerdict::MismatchFaulted
        } else {
            ProbeVerdict::Mismatch
        });
    }
    if current.resource == ResourceState::Attached && !probe.attached {
        let faulted = reconcile_mismatch(bridge, ledger, task_id, &current, &detail)?;
        return Ok(if faulted {
            ProbeVerdict::MismatchFaulted
        } else {
            ProbeVerdict::Mismatch
        });
    }
    if current.resource == ResourceState::Detached {
        feed_resource_attached(bridge, ledger, task_id)?;
    }
    if current.mismatch_count > 0 || current.recovery != RecoveryState::None {
        feed_reconcile_ok(bridge, ledger, task_id)?;
    }
    Ok(ProbeVerdict::Ok)
}

/// What one [`reconcile_probe`] call found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProbeVerdict {
    Ok,
    Dead,
    DeadFaulted,
    Mismatch,
    MismatchFaulted,
    Unknown,
    Exited,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::fake::FakeBackend;
    use crate::backend::{Capabilities, CloseReason, SessionBackend, SpawnSpec};
    use std::collections::BTreeMap;

    fn tracked(ledger: &MemoryLedger, task: &str) -> (Bridge, Version) {
        ledger.track_task(task, 1);
        let bridge = Bridge::new();
        let verdict = feed_created(&bridge, ledger, task).unwrap();
        assert!(matches!(verdict, Verdict::Applied(_)), "{verdict:?}");
        let row = ledger.get_session(task).unwrap().unwrap();
        let obs: Observation = serde_json::from_str(&row.observed_json).unwrap();
        (bridge, obs.version)
    }

    #[test]
    fn created_ready_working_chain_applies_and_persists() {
        let ledger = MemoryLedger::new();
        let (bridge, _) = tracked(&ledger, "chain-1");
        let first = ledger.get_session("chain-1").unwrap().unwrap();
        assert_eq!(first.public_lifecycle, "created");
        assert_eq!((first.generation, first.seq), (1, 1));
        assert_eq!(first.backend_ref, "{}");

        bridge.track_live(SessionRef {
            task_id: "chain-1".into(),
            backend: "fake".into(),
            backend_ref: serde_json::json!({"handle": "term-chain"}),
            generation: 1,
        });
        feed_ready(&bridge, &ledger, "chain-1").unwrap();
        let second = ledger.get_session("chain-1").unwrap().unwrap();
        assert_eq!(second.public_lifecycle, "idle");
        assert_eq!(second.seq, 2);
        assert!(second.backend_ref.contains("term-chain"), "{}", second.backend_ref);
        feed_resource_attached(&bridge, &ledger, "chain-1").unwrap();
        feed_turn_started(&bridge, &ledger, "chain-1").unwrap();
        let third = ledger.get_session("chain-1").unwrap().unwrap();
        assert_eq!(third.agent_state, "running");
        assert_eq!(third.resource_state, "attached");
        assert_eq!(third.public_lifecycle, "working");
        assert_eq!(third.seq, 4);
        let obs: Observation = serde_json::from_str(&third.observed_json).unwrap();
        assert!(lifecycle::is_legal(&obs));
        assert!(third.desired_json.contains("turn_started"), "{}", third.desired_json);

        feed_turn_ended(&bridge, &ledger, "chain-1").unwrap();
        let events = ledger.events();
        let last = events.iter().rev().find(|(k, _)| k == "lifecycle").unwrap();
        assert_eq!(last.1["task_id"], "chain-1");
        assert_eq!(last.1["from"], "working");
        assert_eq!(last.1["to"], "idle");
        assert_eq!(last.1["event"], "turn_ended");
    }

    #[test]
    fn duplicate_and_stale_events_never_write() {
        let ledger = MemoryLedger::new();
        let (bridge, _) = tracked(&ledger, "dup-1");
        assert!(matches!(
            feed_created(&bridge, &ledger, "dup-1").unwrap(),
            Verdict::Ignored(IgnoredReason::NoOp)
        ));
        let before = ledger.get_session("dup-1").unwrap().unwrap();
        assert_eq!(before.seq, 1);
        let verdict = apply_persist(
            &bridge,
            &ledger,
            "dup-1",
            &LifecycleEvent::Ready {
                v: Version::new(1, 1),
            },
        )
        .unwrap();
        assert!(matches!(
            verdict,
            Verdict::Ignored(lifecycle::IgnoredReason::StaleOrDuplicateSeq)
        ));
        let after = ledger.get_session("dup-1").unwrap().unwrap();
        assert_eq!(after.observed_json, before.observed_json);
        assert_eq!(after.updated_at, before.updated_at);
        let stale = VersionedSession {
            seq: 0,
            generation: 0,
            ..to_versioned(
                &serde_json::from_str(&after.observed_json).unwrap(),
                &after.backend_ref,
                "{}",
            )
            .unwrap()
        };
        assert!(!ledger.upsert_session("dup-1", &stale).unwrap());
        assert_eq!(
            ledger.get_session("dup-1").unwrap().unwrap().observed_json,
            before.observed_json
        );
    }
    #[test]
    fn older_seq_after_newer_seq_is_ignored_without_writing() {
        let ledger = MemoryLedger::new();
        let (bridge, _) = tracked(&ledger, "mono-1");
        feed_ready(&bridge, &ledger, "mono-1").unwrap();
        feed_resource_attached(&bridge, &ledger, "mono-1").unwrap();
        feed_turn_started(&bridge, &ledger, "mono-1").unwrap();
        feed_turn_ended(&bridge, &ledger, "mono-1").unwrap();
        let before = ledger.get_session("mono-1").unwrap().unwrap();
        assert_eq!((before.generation, before.seq), (1, 5));
        let event_count = ledger.events().len();
        let verdict = apply_persist(
            &bridge,
            &ledger,
            "mono-1",
            &LifecycleEvent::TurnStarted {
                v: Version::new(1, 4),
            },
        )
        .unwrap();
        assert!(matches!(
            verdict,
            Verdict::Ignored(lifecycle::IgnoredReason::StaleOrDuplicateSeq)
        ));
        let after = ledger.get_session("mono-1").unwrap().unwrap();
        assert_eq!(after.observed_json, before.observed_json);
        assert_eq!((after.generation, after.seq), (1, 5));
        assert_eq!(ledger.events().len(), event_count);
    }


    #[test]
    fn unadopted_generation_is_rejected_without_writing() {
        let ledger = MemoryLedger::new();
        ledger.track_task("gen-1", 1);
        let bridge = Bridge::new();
        bridge.track_live(SessionRef {
            task_id: "gen-1".into(),
            backend: "fake".into(),
            backend_ref: serde_json::json!({"id": "gen-1"}),
            generation: 1,
        });
        feed_created(&bridge, &ledger, "gen-1").unwrap();
        feed_ready(&bridge, &ledger, "gen-1").unwrap();
        feed_turn_started(&bridge, &ledger, "gen-1").unwrap();
        let before = ledger.get_session("gen-1").unwrap().unwrap();
        let verdict = apply_persist(
            &bridge,
            &ledger,
            "gen-1",
            &LifecycleEvent::Ready {
                v: Version::new(2, 1),
            },
        )
        .unwrap();
        assert!(matches!(
            verdict,
            Verdict::Rejected(lifecycle::RejectReason::UnadoptedGeneration)
        ));
        let after = ledger.get_session("gen-1").unwrap().unwrap();
        assert_eq!(after.generation, before.generation);
        assert_eq!(after.seq, before.seq);
        assert_eq!(after.observed_json, before.observed_json);
    }

    #[test]
    fn corrupt_row_keeps_its_watermark_and_reports_itself() {
        let ledger = MemoryLedger::new();
        ledger.track_task("cr-1", 1);
        let bridge = Bridge::new();
        let mut broken = to_versioned(&Observation::initial(1, 3), "{}", "{}").unwrap();
        broken.generation = 1;
        broken.seq = 5;
        broken.observed_json = "{ not json".into();
        assert!(ledger.upsert_session("cr-1", &broken).unwrap());
        let verdict = feed_turn_ended(&bridge, &ledger, "cr-1").unwrap();
        assert!(matches!(verdict, Verdict::Applied(_)), "{verdict:?}");
        let after = ledger.get_session("cr-1").unwrap().unwrap();
        assert_eq!(after.generation, 1);
        assert_eq!(after.seq, 6);
        assert!(ledger.alerts().iter().any(|line| line.contains("cr-1") && line.contains("corrupt")));
        assert!(ledger.events().iter().any(|(k, _)| k == "lifecycle_corrupt"));
    }

    #[test]
    fn probe_dead_fails_the_session_and_records_one_fault() {
        let ledger = MemoryLedger::new();
        ledger.track_task("dead-1", 1);
        let bridge = Bridge::new();
        let backend = FakeBackend::new();
        let session = backend
            .spawn(SpawnSpec {
                cwd: ".".into(),
                task_id: "dead-1".into(),
                command: vec!["agent".into()],
                env: BTreeMap::new(),
                focus: None,
                rename: None,
            })
            .unwrap();
        bridge.track_live(session.clone());
        feed_created(&bridge, &ledger, "dead-1").unwrap();
        feed_resource_attached(&bridge, &ledger, "dead-1").unwrap();
        feed_ready(&bridge, &ledger, "dead-1").unwrap();
        feed_turn_started(&bridge, &ledger, "dead-1").unwrap();
        backend.close(&session, CloseReason::Fault, true).unwrap();
        bridge.untrack_live("dead-1");
        // The stored row keeps the live resource reference, so the probe
        // resolves its target from the live map.
        let row = ledger.get_session("dead-1").unwrap().unwrap();
        assert!(row.backend_ref.contains("dead-1"));
        bridge.track_live(session.clone());
        let verdict = reconcile_probe(&bridge, &ledger, &backend, "dead-1").unwrap();
        assert_eq!(verdict, ProbeVerdict::DeadFaulted);
        let session_row = ledger.get_session("dead-1").unwrap().unwrap();
        assert_eq!(session_row.agent_state, "gone");
        assert_eq!(session_row.public_lifecycle, "exited");
        let faults = ledger.list_faults("dead-1").unwrap();
        assert_eq!(faults.len(), 1, "{faults:?}");
        assert_eq!(faults[0].kind, "probe_dead");
        assert_eq!(faults[0].state, "open");
        // A repeated pass dedupes on (task_id, kind, generation).
        bridge.track_live(session.clone());
        let _ = reconcile_probe(&bridge, &ledger, &backend, "dead-1").unwrap();
        assert_eq!(ledger.list_faults("dead-1").unwrap().len(), 1);
    }

    #[test]
    fn inconclusive_probe_never_judges_a_generation_dead() {
        let ledger = MemoryLedger::new();
        ledger.track_task("unk-1", 1);
        let bridge = Bridge::new();
        let seed = to_versioned(&Observation::initial(1, 3), "{}", "{}").unwrap();
        assert!(ledger.upsert_session("unk-1", &seed).unwrap());
        let backend = FakeBackend::new();
        let verdict = reconcile_probe(&bridge, &ledger, &backend, "unk-1").unwrap();
        assert_eq!(verdict, ProbeVerdict::Unknown);
        assert!(ledger.list_faults("unk-1").unwrap().is_empty());
    }

    #[test]
    fn delivered_hop_settles_as_done_before_the_resource_closes() {
        let ledger = MemoryLedger::new();
        ledger.track_task("out-1", 1);
        let bridge = Bridge::new();
        bridge.track_live(SessionRef {
            task_id: "out-1".into(),
            backend: "fake".into(),
            backend_ref: serde_json::json!({"id": "out-1"}),
            generation: 1,
        });
        feed_created(&bridge, &ledger, "out-1").unwrap();
        feed_resource_attached(&bridge, &ledger, "out-1").unwrap();
        feed_ready(&bridge, &ledger, "out-1").unwrap();
        feed_turn_started(&bridge, &ledger, "out-1").unwrap();
        let verdict = feed_delivered(&bridge, &ledger, "out-1").unwrap();
        assert!(matches!(verdict, Verdict::Applied(_)), "{verdict:?}");
        let session = ledger.get_session("out-1").unwrap().unwrap();
        assert_eq!(session.delivery_state, "accepted");
        assert_eq!(session.public_lifecycle, "exited");
        let verdict = feed_resource_closed(&bridge, &ledger, "out-1").unwrap();
        assert!(matches!(verdict, Verdict::Applied(_)), "{verdict:?}");
        let session = ledger.get_session("out-1").unwrap().unwrap();
        assert_eq!(session.resource_state, "closed");
        assert_eq!(session.agent_state, "gone");
    }

    #[test]
    fn fake_backend_spawn_probe_close_drives_the_reducer() {
        let ledger = MemoryLedger::new();
        ledger.track_task("fake-1", 1);
        let bridge = Bridge::new();
        let backend = FakeBackend::new();
        assert!(backend.capabilities() == Capabilities {
            spawn: true,
            attach: true,
            probe: true,
            close: true,
            focus: true,
            rename: true,
        });
        let session = backend
            .spawn(SpawnSpec {
                cwd: ".".into(),
                task_id: "fake-1".into(),
                command: vec!["agent".into()],
                env: BTreeMap::new(),
                focus: None,
                rename: None,
            })
            .unwrap();
        bridge.track_live(session.clone());
        feed_created(&bridge, &ledger, "fake-1").unwrap();
        assert_eq!(
            ledger.get_session("fake-1").unwrap().unwrap().public_lifecycle,
            "created"
        );
        let probe = backend.probe(&session).unwrap();
        assert!(probe.alive);
        feed_resource_attached(&bridge, &ledger, "fake-1").unwrap();
        feed_ready(&bridge, &ledger, "fake-1").unwrap();
        assert_eq!(
            ledger.get_session("fake-1").unwrap().unwrap().public_lifecycle,
            "idle"
        );
        feed_turn_started(&bridge, &ledger, "fake-1").unwrap();
        assert_eq!(
            ledger.get_session("fake-1").unwrap().unwrap().public_lifecycle,
            "working"
        );
        feed_delivered(&bridge, &ledger, "fake-1").unwrap();
        backend.close(&session, CloseReason::Completed, false).unwrap();
        assert!(!backend.probe(&session).unwrap().alive);
        feed_resource_closed(&bridge, &ledger, "fake-1").unwrap();
        let closed = ledger.get_session("fake-1").unwrap().unwrap();
        assert_eq!(closed.public_lifecycle, "exited");
        assert_eq!(closed.resource_state, "closed");
        assert_eq!(closed.agent_state, "gone");
    }
}
