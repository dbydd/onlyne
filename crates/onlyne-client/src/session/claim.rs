//! Hello claim: the sessions occupying dispatch live slots.
//!
//! Slots exist from assign until `release_locked`. A fresh process has an
//! empty map, so hello claims no session and the server requeues. A live
//! client whose link flaps still holds its slots, so the deliveries those
//! sessions are bound to stay `in_flight`. Heartbeat stale, stall reports, and
//! operator repair cover a pane that has already died.

use std::collections::BTreeMap;

use onlyne_proto::LiveSession;

/// One sorted, deduplicated claim from a set of live sessions.
///
/// One entry per session id, so the same session claimed from both the memory
/// slots and the durable rows is reported once. The first entry for an id is
/// the one kept: callers put what the process can prove it holds right now
/// first, and the store's older reading of the same session cannot overwrite it.
pub fn from_sessions(sessions: impl IntoIterator<Item = LiveSession>) -> Vec<LiveSession> {
    let mut by_id: BTreeMap<String, LiveSession> = BTreeMap::new();
    for session in sessions {
        by_id.entry(session.session_id.clone()).or_insert(session);
    }
    by_id.into_values().collect()
}

/// The claim for the sessions held in memory alone: a client-held session shares
/// its id with the delivery that opened it, and the slot is that delivery.
pub fn from_slots(ids: impl IntoIterator<Item = impl Into<String>>) -> Vec<LiveSession> {
    from_sessions(ids.into_iter().map(|id| {
        let id = id.into();
        LiveSession {
            session_id: id.clone(),
            task_id: Some(id),
            suspended: false,
        }
    }))
}

#[cfg(test)]
mod tests;
