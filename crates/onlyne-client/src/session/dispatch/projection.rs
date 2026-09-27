use super::*;

use super::outbound::send_frame;
use super::state::{DispatchInner, DispatchState, binding_task_state, slot_key_named};

/// Stamp the origin cluster on the state-carrying report kinds.
///
/// `cluster_ref` names the cluster whose supervisor observed this projection
/// (`docs/v1-PLAN.md` line 248, the `aggregate` annotation of the role's own
/// spec entry, which is `Principal::Cluster` on the wire at line 122). A plain
/// role leaves the field unset, which is the `skip_serializing_if` shape the
/// byte-identical replay rule at line 502 depends on.
///
/// A heartbeat that carries a projection is the one state-carrying report this
/// client builds rather than relays, and it stays unstamped: the server mirrors
/// such a projection verbatim, and the publish has never named an origin
/// cluster.
pub fn with_cluster(state: &DispatchState, report: Report) -> Report {
    let cluster = state.cluster_ref();
    if cluster.is_empty() {
        return report;
    }
    match report {
        Report::Ready {
            task_id,
            session_id,
            generation,
            seq,
            ..
        } => Report::Ready {
            task_id,
            session_id,
            generation,
            seq,
            cluster_ref: Some(cluster),
        },
        Report::Heartbeat {
            projection: None,
            task_id,
            session_id,
            generation,
            seq,
            observed,
            ..
        } => Report::Heartbeat {
            task_id,
            session_id,
            generation,
            seq,
            observed,
            projection: None,
            cluster_ref: Some(cluster),
        },
        Report::Complete {
            task_id,
            outcome,
            head,
            reply_to,
            ..
        } => Report::Complete {
            task_id,
            outcome,
            head,
            reply_to,
            cluster_ref: Some(cluster),
        },
        other => other,
    }
}

/// The wire projection of one stored session row, given how its task ended.
///
/// The row and the task record answer different questions. The row holds the
/// session's own tuple — agent, intent, resource, recovery line — and the task
/// table holds the verdict the agent filed. Neither one says whether the session
/// is over on its own, so nothing here is read as a lifecycle: `project` takes
/// the tuple's dimensions plus `task_state` and derives the public view. A
/// caller that wants a projection reads both rows.
pub fn projection_of(row: &SessionRecord, task_state: TaskState) -> SessionProjection {
    let observed: Option<serde_json::Value> = serde_json::from_str(&row.observed_json).ok();
    // The dimension columns hold the reducer's own words, written from the same
    // observation as `observed_json`, so the derivation reads them rather than
    // the JSON bytes. A word that does not decode falls back to the freshly
    // created dimension, which is the reading the wire columns below give too.
    let lifecycle = project(
        phase(&row.agent_state, AgentPhase::Booting),
        phase(&row.delivery_state, DeliveryPhase::NoIntent),
        phase(&row.resource_state, ResourcePhase::Detached),
        phase(&row.recovery_substate, RecoveryPhase::NoRecovery),
        task_state,
    );
    SessionProjection {
        lifecycle,
        agent: phase(&row.agent_state, AgentPhase::Booting),
        delivery: phase(&row.delivery_state, DeliveryPhase::NoIntent),
        resource: phase(&row.resource_state, ResourcePhase::Detached),
        recovery: phase(&row.recovery_substate, RecoveryPhase::NoRecovery),
        outcome: task_outcome_of(task_state),
        observed,
    }
}

/// The task state a reported outcome names. The wire has no word for work still
/// in flight — a completion either says how it ended or is not a completion — so
/// `pending` comes from the absence of a verdict, not from a report.
pub fn task_state_of(outcome: Outcome) -> TaskState {
    match outcome {
        Outcome::Done => TaskState::Done,
        Outcome::Failed => TaskState::Failed,
        Outcome::Cancelled => TaskState::Cancelled,
        // A blocked delivery is a settled delivery whose work waits on
        // something outside it, which is the task record's own blocked state.
        Outcome::Blocked => TaskState::Blocked,
    }
}

/// The wire verdict for one task state, which is what a published projection
/// carries beside its derived lifecycle. `pending` has no wire word.
pub fn task_outcome_of(task_state: TaskState) -> Option<Outcome> {
    match task_state {
        TaskState::Pending => None,
        TaskState::Done => Some(Outcome::Done),
        TaskState::Failed => Some(Outcome::Failed),
        TaskState::Cancelled => Some(Outcome::Cancelled),
        TaskState::Blocked => Some(Outcome::Blocked),
    }
}

/// How the task of one stored session ended, read from its own record.
///
/// A task with no record is `pending`: the open and the settle both write the
/// row, so nothing having been written about a task means no delivery became a
/// session for it and no verdict came in for it. Callers hold the dispatch guard,
/// which this read needs to reach the store; the lock is not reentrant.
pub(super) fn stored_task_state(inner: &DispatchInner, task_id: &str) -> TaskState {
    inner
        .store
        .task(task_id)
        .ok()
        .flatten()
        .map(|record| record.task_state)
        .unwrap_or(TaskState::Pending)
}

/// Decode one stored enum word, falling back to the freshly created phase.
pub(super) fn phase<T: serde::de::DeserializeOwned>(word: &str, fallback: T) -> T {
    serde_json::from_value(serde_json::Value::String(word.to_string())).unwrap_or(fallback)
}

/// Publish the current projection of one session.
///
/// The frame is the client's heartbeat report carrying the whole projection:
/// one frame per session activity, and the only one that puts a session's state
/// on the wire. The argument names the session, or the delivery a caller has in
/// hand: a client-held session takes its id from the delivery that opened it, so
/// both spellings reach the same row and the slot the client holds decides which
/// session that is.
pub async fn sync_session(state: &DispatchState, session_id: &str) -> Result<()> {
    let Some(op) = sync_frame(state, session_id)? else {
        return Ok(());
    };
    send_frame(state, op).await
}

/// The durable frame one session's current projection publishes, or `None`
/// when the store holds no row for it.
///
/// `sync_session` sends this through `send_frame`, which already queues it when
/// the link is down; the frame is exposed separately for the caller whose send
/// failed for some other reason and owes the exit a second attempt through
/// [`DispatchState::enqueue_op`].
///
/// The session's own id is what the server keys the mirror by, and it stays put
/// while a scope hands the session delivery after delivery. The delivery travels
/// beside it as `task_id`: the one the session is bound to now, or the last one
/// it served once the binding is released. Whether the session is between
/// deliveries is the projection's own delivery dimension, not the id's — so the
/// mirror keeps reading the delivery a row belongs to while the pair says the
/// session is live.
///
/// A session whose row was never given a delivery sends an empty `task_id`,
/// which is the wire's only spelling for "this session is on no delivery"; a
/// client-held session always has the delivery that opened it.
pub fn sync_frame(state: &DispatchState, session_id: &str) -> Result<Option<ClientOp>> {
    // One section for both reads: a publish that took the session tuple before a
    // settle and its verdict after would derive a lifecycle the pair never agreed
    // to, and the store's lock is what keeps the two rows in step.
    let (key, row, task_state) = {
        let inner = state.inner.lock();
        let key = slot_key_named(&inner, session_id);
        let task_state = match key.as_deref().and_then(|key| inner.sessions.get(key)) {
            // A session this client holds answers from its slot: the delivery it
            // serves now is the binding, and a session serving nothing reads as
            // `pending`, which is a live session rather than an exit.
            Some(slot) => binding_task_state(&inner, slot),
            // A session whose slot is gone is read from the delivery the caller
            // named, which is how a retired session's row stays publishable.
            None => match inner.store.get_session(session_id)? {
                Some(row) => stored_task_state(&inner, &row.task_id),
                None => TaskState::Pending,
            },
        };
        (key, inner.store.get_session(session_id)?, task_state)
    };
    let Some(row) = row else { return Ok(None) };
    let session_id = key.unwrap_or_else(|| row.session_id.clone());
    let projection = projection_of(&row, task_state);
    Ok(Some(ClientOp::Report(Report::Heartbeat {
        task_id: row.task_id.clone(),
        session_id,
        generation: row.generation.max(0) as u64,
        seq: row.seq.max(0) as u64,
        observed: projection
            .observed
            .clone()
            .unwrap_or(serde_json::Value::Null),
        cluster_ref: None,
        projection: Some(projection),
    })))
}

/// Log a reducer verdict and answer the version it advanced to.
pub fn note_verdict(verdict: &Verdict, task_id: &str) -> Option<Version> {
    match verdict {
        Verdict::Applied(observation) => Some(observation.version),
        Verdict::Ignored(reason) => {
            tracing::debug!(task = %task_id, ?reason, "lifecycle event ignored");
            None
        }
        Verdict::Rejected(reason) => {
            tracing::warn!(task = %task_id, ?reason, "lifecycle event rejected; the ledger kept its state");
            None
        }
    }
}

/// Feed the reducer the receipt the outbound queue observed for one session.
///
/// The durable queue is the only witness of the server's answer, so this is the
/// door a receipt comes in by: the flusher learns that the completion was taken
/// and the tuple records it as a fact it observed rather than as a body this
/// client composed for itself. A session with no row, and a drain that is not
/// open, are the reducer's own answers and leave the ledger alone — the verdict
/// is logged like every other feed's and nothing is retried here.
pub fn note_intent_receipt(state: &DispatchState, task_id: &str) {
    let verdict = {
        let inner = state.inner.lock();
        feed_intent_receipt(&inner.bridge, &inner.store, task_id)
    };
    match verdict {
        Ok(verdict) => {
            note_verdict(&verdict, task_id);
        }
        Err(error) => tracing::debug!(
            task = %task_id,
            error = %error,
            "intent receipt not recorded in the session ledger"
        ),
    }
}
