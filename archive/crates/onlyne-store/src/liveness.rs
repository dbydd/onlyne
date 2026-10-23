//! The session liveness this process holds in memory, and what a reader is handed.
//!
//! A session beats far more often than its projection changes: the client
//! republishes an unchanged tuple every ten seconds. v1 turned each of those
//! beats into a session-row write, a durable `session_state` event, and a
//! broadcast to every subscriber, all serialized through one SQLite writer, and
//! that path was the cluster's concurrency ceiling (plan §"网络与并发", v1
//! finding 6). v2 keeps the beat here instead: the row moves when its content
//! changes, when the session ends, and once per presence window.
//!
//! Two facts about one session are in play, and the precedence between them is
//! the whole point of this module:
//!
//! - the row's `last_seen`, the freshness a reader judges the mirror by;
//! - the freshest beat this process took, which the row has not been told.
//!
//! A reader must never be handed the older of the two while the server holds
//! the newer — that is v1's frozen mirror in its second form — so every read of
//! the table passes through [`LiveSessions::freshest`], and `last_seen` never
//! regresses: an entry is dropped only when the row's own value catches up with
//! it.

use std::collections::HashMap;
use std::sync::Mutex;

/// The freshest beat of each session this process took without writing it down.
///
/// An entry is spent as soon as the row's own `last_seen` reaches it, which a
/// content write or an interval flush does. The map therefore holds the
/// sessions that are currently ahead of their row rather than every session
/// ever seen: a session that ends writes its ending, and the write settles it.
#[derive(Debug, Default)]
pub(crate) struct LiveSessions {
    beats: Mutex<HashMap<String, i64>>,
}

impl LiveSessions {
    /// Take one beat: the entry moves to `at`, and never backwards.
    ///
    /// The clock is the kernel's unix seconds, the same one the row's
    /// `last_seen` is written from, so the two values compare directly. A beat
    /// older than one already taken — two links beating for one session, which
    /// a plugin loaded twice produces — does not move the entry back.
    pub(crate) fn note(&self, session_id: &str, at: i64) {
        if let Ok(mut beats) = self.beats.lock() {
            let entry = beats.entry(session_id.to_string()).or_insert(at);
            *entry = (*entry).max(at);
        }
    }

    /// The freshest `last_seen` this process can answer for one row.
    ///
    /// The beat it holds when that beat is the newer one, the persisted value
    /// otherwise: a restart has no beats at all, and a row the server holds
    /// nothing new about is answered exactly as it was written.
    pub(crate) fn freshest(&self, session_id: &str, persisted: i64) -> i64 {
        self.beats
            .lock()
            .ok()
            .and_then(|beats| beats.get(session_id).copied())
            .map_or(persisted, |beat| beat.max(persisted))
    }

    /// Forget a beat the row has caught up with.
    ///
    /// `persisted` is the value a write just left in the row. A beat at or
    /// below it is spent — every reader sees the same value either way — and
    /// dropping it keeps this map to the sessions currently ahead of their row.
    /// A newer beat stays: it is still the fresher fact, and the write that
    /// carried the older value did not take it away.
    pub(crate) fn settle(&self, session_id: &str, persisted: i64) {
        if let Ok(mut beats) = self.beats.lock() {
            if beats.get(session_id).is_some_and(|beat| *beat <= persisted) {
                beats.remove(session_id);
            }
        }
    }
}
