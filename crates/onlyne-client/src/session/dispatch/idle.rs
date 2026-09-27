//! Idle and suspended sessions: the two states a `task` or `role` session
//! reaches between deliveries.
//!
//! A delivery that settles leaves a scoped session idle rather than ending it,
//! and the scope's `idle_close` bound decides when this client does something
//! about that: it releases the process, which is what `suspended` means. Whether
//! it may release the process at all is the runtime's answer, and the degradation
//! is part of the contract — a runtime that cannot resume keeps its process, and
//! the session waits for its family's next delivery exactly as it stands.

use super::retire::{PendingClose, close_retired};
use super::state::{DispatchInner, binding_task_state};
use super::transport::names_session;
use super::*;

impl DispatchState {
    /// Release the process of every idle session whose scope bound has expired.
    ///
    /// A suspension is this client's own act and it is only worth doing for a
    /// session the runtime can bring back: the conversation lives in the
    /// runtime's own store, so the client starts the same command again when the
    /// family's next delivery arrives and the runtime resumes the session it was
    /// given. A runtime that declared no resume keeps its process instead — the
    /// "process alive, session alive" degradation the scope table names — and
    /// this sweep leaves it alone.
    ///
    /// The closes are host round trips, so they are collected here and run once
    /// the dispatch lock is off them, the way the sweeps that retire sessions
    /// already do. Answers the sessions it released, which are the ones whose
    /// row and claim this caller has to publish.
    pub fn suspend_idle_sessions(&self, now: Instant) -> Vec<String> {
        let mut pending: Vec<PendingClose> = Vec::new();
        let mut suspended: Vec<String> = Vec::new();
        {
            let mut inner = self.inner.lock();
            let Some(bound) = scope::idle_bound(&inner.session_policy) else {
                return Vec::new();
            };
            let due: Vec<String> = inner
                .sessions
                .iter()
                .filter(|(_, slot)| {
                    !slot.suspended
                        && !slot.read_only
                        && slot.task_id.is_none()
                        && slot
                            .idle_since
                            .is_some_and(|since| now.saturating_duration_since(since) >= bound)
                })
                .map(|(key, _)| key.clone())
                .collect();
            for key in due {
                let session_id = inner
                    .sessions
                    .get(&key)
                    .map(|slot| slot.session.task_id.clone())
                    .unwrap_or_else(|| key.clone());
                if !resumable(&inner, &key) {
                    tracing::info!(
                        session = %session_id,
                        idle_secs = inner
                            .sessions
                            .get(&key)
                            .and_then(|slot| slot.idle_since)
                            .map(|since| now.saturating_duration_since(since).as_secs())
                            .unwrap_or_default(),
                        "an idle session's runtime cannot resume it; the process stays and the session waits"
                    );
                    continue;
                }
                if suspend_locked(&mut inner, &key, &mut pending) {
                    suspended.push(session_id);
                }
            }
        }
        close_retired(pending);
        suspended
    }

    /// Whether one session's process is currently released.
    pub fn is_suspended(&self, session_id: &str) -> bool {
        let inner = self.inner.lock();
        inner
            .sessions
            .iter()
            .any(|(key, slot)| names_session(key, slot, session_id) && slot.suspended)
    }
}

/// Whether one session's runtime declared that it can resume a session this
/// client released.
///
/// The declaration rides the plugin's mount (`Capability::Resume`), because the
/// runtime is the only party that knows whether its conversation survives its
/// process: pi and DSH keep their own session files, an ACP agent declares
/// `session/resume` or `session/load`, and a runtime that has neither cannot be
/// brought back into a conversation it does not remember.
///
/// A self-driven backend owns its agent and talks to it without an adapter
/// socket, so no mount ever declares anything for it: this client answers no,
/// which keeps the process in place. Answering yes there would take the client's
/// own guarantee on behalf of a runtime nobody asked.
pub(super) fn resumable(inner: &DispatchInner, key: &str) -> bool {
    if inner.backend.self_driven() {
        return false;
    }
    let Some(slot) = inner.sessions.get(key) else {
        return false;
    };
    inner
        .transports
        .iter()
        .find(|(session, _)| names_session(key, slot, session))
        .is_some_and(|(_, (_, capabilities))| capabilities.contains(&Capability::Resume))
}

/// Release one idle session's process while its row and its conversation stay.
///
/// The order is the store's: the tuple moves first — `Suspend` closes the
/// resource while the generation stays live, which is what tells `project` this
/// session is idle rather than exited — and the binding the write re-takes is
/// handed back once the row is final. A session that keeps a binding while
/// serving nothing reads as bound to a delivery it has finished, which is the
/// reading `hello.live_sessions` and the mirror's `task_id` both take.
/// Whether one session reads `idle`: the state a suspension is defined from.
///
/// The row is the session's, and the delivery it last served is the binding's:
/// `projection_of` derives the lifecycle from both, which is the same reading
/// the reducer takes when it judges the `Suspend` event this client feeds it.
fn at_rest(inner: &DispatchInner, key: &str) -> bool {
    let Some(slot) = inner.sessions.get(key) else {
        return false;
    };
    let Ok(Some(row)) = inner.store.get_session(&slot.session.task_id) else {
        return false;
    };
    projection_of(&row, binding_task_state(inner, slot)).lifecycle == Lifecycle::Idle
}

pub(super) fn suspend_locked(
    inner: &mut DispatchInner,
    key: &str,
    pending: &mut Vec<PendingClose>,
) -> bool {
    let Some(slot) = inner.sessions.get(key) else {
        return false;
    };
    let session = slot.session.clone();
    let session_id = session.task_id.clone();
    // The plan's state machine suspends a session that is *idle*, and the
    // reducer reads that off the tuple: a row still projecting `working` — an
    // agent with a turn in flight, or a delivery whose intent is unanswered —
    // is not a session whose process may go, and its `Suspend` is an undefined
    // transition. The sweep's own `idle_since` says how long the session has
    // held no delivery; this says whether the runtime is at rest, which is the
    // half only its own reports can answer.
    if !at_rest(inner, key) {
        tracing::debug!(
            session = %session_id,
            "the session is not idle yet; its process stays and the family's next delivery still enters it"
        );
        return false;
    }
    if let Err(error) = feed_suspended(&inner.bridge, &inner.store, &session_id) {
        tracing::warn!(
            session = %session_id,
            error = %error,
            "a suspended session's row was not written; the process stays"
        );
        return false;
    }
    if let Err(error) = inner.store.release_binding(&session_id, &session_id) {
        tracing::warn!(
            session = %session_id,
            error = %error,
            "a suspended session's own binding was not handed back"
        );
    }
    let now = Instant::now();
    if let Some(slot) = inner.sessions.get_mut(key) {
        slot.suspended = true;
        slot.idle_since = None;
        // Nothing is expected to dial for a released session, and the two death
        // windows read a stamp a suspended session would never refresh: the
        // reconnect grace would retire the conversation this release just saved.
        slot.dropped_at = None;
        slot.last_beat = Some(now);
    }
    inner.transports.remove(&session_id);
    tracing::info!(
        session = %session_id,
        backend = %session.backend,
        "idle session suspended; its process was released and the conversation stays in the runtime"
    );
    pending.push(PendingClose {
        backend: Arc::clone(&inner.backend),
        session,
        reason: crate::backend::CloseReason::Operator,
    });
    true
}
