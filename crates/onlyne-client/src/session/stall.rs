//! Client-side zero-activity stall observation.
//!
//! A freeze past `stall_report_secs` is reported as `Report::Fault` kind
//! `stalled`. The ledger row stays as stored. Complete is not sent.

use onlyne_proto::Report;
use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

/// Fault kind stamped on a stall observation.
pub const STALLED: &str = "stalled";
/// Reason text carried with the stall fault.
pub const STALLED_REASON: &str = "no applied progress";

/// Per-task progress clock and one-shot freeze reporting.
#[derive(Debug, Default)]
pub struct StallWatch {
    last_progress: HashMap<String, Instant>,
    reported: HashSet<String>,
}

impl StallWatch {
    pub fn new() -> Self {
        Self::default()
    }

    /// Start the clock when a task is assigned. A later assign of the same
    /// task keeps the original instant.
    pub fn note_assigned(&mut self, task_id: impl Into<String>, now: Instant) {
        self.last_progress.entry(task_id.into()).or_insert(now);
    }

    /// An Applied persist refreshes the clock of an assigned task and clears
    /// the freeze report bit so a later freeze can be reported again. Clock
    /// creation belongs to [`Self::note_assigned`].
    pub fn note_applied(&mut self, task_id: &str, now: Instant) {
        if let Some(last_progress) = self.last_progress.get_mut(task_id) {
            *last_progress = now;
            self.reported.remove(task_id);
        }
    }

    /// Drop a task that has left the live set.
    pub fn forget(&mut self, task_id: &str) {
        self.last_progress.remove(task_id);
        self.reported.remove(task_id);
    }

    /// Task ids whose freeze exceeds `threshold_secs` and have not been
    /// reported in this freeze episode. A threshold of zero reports nothing.
    pub fn due(&self, now: Instant, threshold_secs: u64) -> Vec<String> {
        if threshold_secs == 0 {
            return Vec::new();
        }
        let limit = Duration::from_secs(threshold_secs);
        let mut due: Vec<String> = self
            .last_progress
            .iter()
            .filter(|(task_id, _)| !self.reported.contains(*task_id))
            .filter(|(_, last)| now.saturating_duration_since(**last) > limit)
            .map(|(task_id, _)| task_id.clone())
            .collect();
        due.sort();
        due
    }

    /// Remember that this freeze episode has been reported.
    pub fn mark_reported(&mut self, task_id: &str) {
        self.reported.insert(task_id.to_string());
    }
}

/// Observation-only fault for a stalled task. The server records a fault row
/// and an advisory event; the ledger row is not flipped.
pub fn report(
    task_id: &str,
    session_id: Option<String>,
    generation: Option<u64>,
    seq: Option<u64>,
) -> Report {
    Report::Fault {
        task_id: Some(task_id.to_string()),
        session_id,
        generation,
        seq,
        kind: STALLED.to_string(),
        reason: STALLED_REASON.to_string(),
        desired: None,
        observed: None,
    }
}

#[cfg(test)]
mod tests;
