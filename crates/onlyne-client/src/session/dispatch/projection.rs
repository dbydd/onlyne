use super::*;

use super::outbound::send_frame;
use super::state::{DispatchInner, DispatchState};

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
    let lifecycle = wire_lifecycle(project(
        phase(&row.agent_state, AgentState::Booting),
        phase(&row.delivery_state, DeliveryState::None),
        phase(&row.resource_state, ResourceState::Detached),
        phase(&row.recovery_substate, RecoveryState::None),
        task_state,
    ));
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

/// The wire word for a derived lifecycle. `PublicLifecycle` and `Lifecycle` are
/// the same view named in two crates, and a match is what notices if one of them
/// grows an arm the other does not have.
fn wire_lifecycle(lifecycle: PublicLifecycle) -> Lifecycle {
    match lifecycle {
        PublicLifecycle::Created => Lifecycle::Created,
        PublicLifecycle::Working => Lifecycle::Working,
        PublicLifecycle::Idle => Lifecycle::Idle,
        PublicLifecycle::Exited => Lifecycle::Exited,
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
/// on the wire. `session_id` is the row the server keys the mirror by, which for
/// a client-held session is its task id.
pub async fn sync_session(state: &DispatchState, task_id: &str) -> Result<()> {
    // One section for both reads: a publish that took the session tuple before a
    // settle and its verdict after would derive a lifecycle the pair never agreed
    // to, and the store's lock is what keeps the two rows in step.
    let (row, task_state) = {
        let inner = state.inner.lock();
        (
            inner.store.get_session(task_id)?,
            stored_task_state(&inner, task_id),
        )
    };
    let Some(row) = row else { return Ok(()) };
    let projection = projection_of(&row, task_state);
    send_frame(
        state,
        ClientOp::Report(Report::Heartbeat {
            task_id: row.task_id.clone(),
            session_id: row.task_id.clone(),
            generation: row.generation.max(0) as u64,
            seq: row.seq.max(0) as u64,
            observed: projection
                .observed
                .clone()
                .unwrap_or(serde_json::Value::Null),
            cluster_ref: None,
            projection: Some(projection),
        }),
    )
    .await
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
