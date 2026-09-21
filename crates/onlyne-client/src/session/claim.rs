//! Hello claim: task ids occupying dispatch live slots.
//!
//! Slots exist from assign until `release_locked`. A fresh process has an
//! empty map, so hello sends no `live_tasks` and the server requeues. A live
//! client whose link flaps still holds its slots, so those rows stay
//! `in_flight`. Heartbeat stale, stall reports, and operator repair cover a
//! pane that has already died.

use std::collections::BTreeSet;

/// Sorted, deduplicated task ids from dispatch live slots.
pub fn from_slots(ids: impl IntoIterator<Item = impl Into<String>>) -> Vec<String> {
    let tasks: BTreeSet<String> = ids.into_iter().map(Into::into).collect();
    tasks.into_iter().collect()
}

#[cfg(test)]
mod tests;
