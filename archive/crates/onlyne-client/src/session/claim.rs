//! Hello claim: the sessions this client holds, as `hello.live_sessions`.
//!
//! A slot exists from the delivery that opened its session until that session
//! ends. A fresh process has an empty map and empty rows, so its hello claims
//! nothing and the server requeues every unacknowledged row. A live client whose
//! link flaps still holds its slots, so the deliveries those sessions are
//! serving stay `in_flight`. Heartbeat stale, stall reports, and operator repair
//! cover a pane that has already died.

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
