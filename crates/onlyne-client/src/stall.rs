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
mod tests {
    use super::*;

    #[test]
    fn freeze_past_threshold_reports_once_until_applied_resets() {
        let mut watch = StallWatch::new();
        let t0 = Instant::now();
        watch.note_assigned("t-1", t0);

        assert!(watch.due(t0 + Duration::from_secs(1799), 1800).is_empty());
        assert_eq!(
            watch.due(t0 + Duration::from_secs(1801), 1800),
            vec!["t-1".to_string()]
        );

        watch.mark_reported("t-1");
        assert!(
            watch.due(t0 + Duration::from_secs(5000), 1800).is_empty(),
            "one freeze episode reports once"
        );

        let t_applied = t0 + Duration::from_secs(5000);
        watch.note_applied("t-1", t_applied);
        assert!(
            watch
                .due(t_applied + Duration::from_secs(1800), 1800)
                .is_empty()
        );
        assert_eq!(
            watch.due(t_applied + Duration::from_secs(1801), 1800),
            vec!["t-1".to_string()]
        );
    }

    #[test]
    fn applied_after_forget_does_not_recreate_a_progress_clock() {
        let mut watch = StallWatch::new();
        let t0 = Instant::now();
        watch.note_assigned("retired", t0);
        watch.forget("retired");
        watch.note_applied("retired", t0 + Duration::from_secs(10));

        assert!(
            watch.due(t0 + Duration::from_secs(10_000), 1800).is_empty(),
            "a retired task stays outside the stall watch"
        );
    }

    #[test]
    fn zero_threshold_never_reports() {
        let mut watch = StallWatch::new();
        let t0 = Instant::now();
        watch.note_assigned("t-1", t0);
        assert!(watch.due(t0 + Duration::from_secs(10_000), 0).is_empty());
    }

    #[test]
    fn noop_beat_leaves_the_clock_untouched() {
        let mut watch = StallWatch::new();
        let t0 = Instant::now();
        watch.note_assigned("t-1", t0);
        // A No-op beat is not Applied, so the watch is left as assigned.
        assert_eq!(
            watch.due(t0 + Duration::from_secs(1801), 1800),
            vec!["t-1".to_string()]
        );
    }

    #[test]
    fn stalled_fault_serializes_with_kind_and_task() {
        let report = report("task-1", Some("task-1".into()), Some(2), Some(9));
        let value = serde_json::to_value(&report).expect("encode");
        assert_eq!(value["kind"], "fault");
        assert_eq!(value["data"]["kind"], STALLED);
        assert_eq!(value["data"]["task_id"], "task-1");
        assert_eq!(value["data"]["session_id"], "task-1");
        assert_eq!(value["data"]["generation"], 2);
        assert_eq!(value["data"]["seq"], 9);
        assert_eq!(value["data"]["reason"], STALLED_REASON);
        let round: Report = serde_json::from_value(value).expect("round-trip");
        assert_eq!(round, report);
    }

    #[test]
    fn old_fault_frame_without_optional_fields_still_deserializes() {
        let raw = r#"{"kind":"fault","data":{"kind":"intent_exhausted","reason":"peer gone"}}"#;
        let report: Report = serde_json::from_str(raw).expect("old frame");
        match report {
            Report::Fault {
                task_id,
                session_id,
                generation,
                seq,
                kind,
                reason,
                desired,
                observed,
            } => {
                assert_eq!(task_id, None);
                assert_eq!(session_id, None);
                assert_eq!(generation, None);
                assert_eq!(seq, None);
                assert_eq!(kind, "intent_exhausted");
                assert_eq!(reason, "peer gone");
                assert_eq!(desired, None);
                assert_eq!(observed, None);
            }
            other => panic!("expected fault, got {other:?}"),
        }
    }
}
