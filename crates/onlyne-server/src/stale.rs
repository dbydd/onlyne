//! Observe working sessions for two silence conditions.
//!
//! An offline owner past 600s records `stale_working`. An online owner silent
//! past `[server].heartbeat_grace_secs` records `heartbeat_missing`. Detection
//! records a fault and emits `Event::Fault`. Recovery stays with the supervisor
//! and the `repair_*` verbs.

use crate::faults::{self, FaultDraft};
use crate::state::State;
use chrono::{DateTime, Utc};
use onlyne_proto::{FaultEvent, Lifecycle, QuerySessionsArgs};
use onlyne_store::{FaultQuery, ServerSessionRow};
use std::collections::HashSet;

/// Fault kind recorded when a working session's owner role is offline past grace.
pub const KIND_STALE_WORKING: &str = "stale_working";
/// Fault kind recorded when a working session's online owner has gone silent.
pub const KIND_HEARTBEAT_MISSING: &str = "heartbeat_missing";
/// Fault kind recorded when an accepted write revives an exited row inside one generation.
pub const KIND_HEARTBEAT_AFTER_COMPLETE: &str = "heartbeat_after_complete";

/// Age a working session must exceed, in seconds, before it is observed.
///
/// The client-side `stale_grace_secs` is not visible to the server, so this
/// constant is the observation window. It is not a third config field: the
/// scan interval is `[server].stale_watch_secs` and the client grace is
/// `[client] stale_grace_secs`.
pub const STALE_WATCH_GRACE_SECS: u64 = 600;

/// One working session considered for observation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkingSession {
    pub task_id: String,
    pub role: String,
    pub updated_at: DateTime<Utc>,
}

impl WorkingSession {
    pub fn from_row(row: &ServerSessionRow) -> Option<Self> {
        if row.public_lifecycle != "working" {
            return None;
        }
        Some(Self {
            task_id: row.task_id.clone(),
            role: row.role.clone(),
            updated_at: DateTime::from_timestamp(row.updated_at, 0).unwrap_or(Utc::now()),
        })
    }
}

/// One open fault of `kind`, keyed by task.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenFault {
    pub task_id: String,
    pub kind: String,
}

/// Decide which working sessions should produce a `stale_working` draft.
///
/// A row is skipped when its owner is online, its age is at most `grace_secs`
/// (equality stays), or an unacked fault of the same kind already exists for
/// the task.
pub fn scan(
    working_rows: &[WorkingSession],
    online_roles: &HashSet<String>,
    now: DateTime<Utc>,
    grace_secs: u64,
    open_faults: &[OpenFault],
) -> Vec<FaultDraft> {
    let grace = chrono::Duration::seconds(grace_secs as i64);
    working_rows
        .iter()
        .filter(|row| !online_roles.contains(&row.role))
        .filter(|row| now.signed_duration_since(row.updated_at) > grace)
        .filter(|row| {
            !open_faults
                .iter()
                .any(|fault| fault.task_id == row.task_id && fault.kind == KIND_STALE_WORKING)
        })
        .map(|row| {
            FaultDraft::new(
                KIND_STALE_WORKING,
                format!(
                    "role {} is offline and session {} has been working past {grace_secs}s",
                    row.role, row.task_id
                ),
            )
            .with_role(&row.role)
            .with_task(&row.task_id)
        })
        .collect()
}

/// Decide which working sessions should produce a `heartbeat_missing` draft.
///
/// A row is selected when its owner is online, the server has seen it since
/// open, its age exceeds `grace_secs`, and no unacked `heartbeat_missing` fault
/// exists for the task.
pub fn scan_heartbeat(
    working_rows: &[WorkingSession],
    online_roles: &HashSet<String>,
    seen: &impl Fn(&str) -> bool,
    now: DateTime<Utc>,
    grace_secs: u64,
    open_faults: &[OpenFault],
) -> Vec<FaultDraft> {
    let grace = chrono::Duration::seconds(grace_secs as i64);
    working_rows
        .iter()
        .filter(|row| online_roles.contains(&row.role))
        .filter(|row| seen(&row.task_id))
        .filter(|row| now.signed_duration_since(row.updated_at) > grace)
        .filter(|row| {
            !open_faults
                .iter()
                .any(|fault| fault.task_id == row.task_id && fault.kind == KIND_HEARTBEAT_MISSING)
        })
        .map(|row| {
            let age_secs = now
                .signed_duration_since(row.updated_at)
                .num_seconds()
                .max(0) as u64;
            FaultDraft::new(
                KIND_HEARTBEAT_MISSING,
                format!(
                    "session {} has sent no heartbeat for {age_secs}s while role {} stays connected",
                    row.task_id, row.role
                ),
            )
            .with_role(&row.role)
            .with_task(&row.task_id)
        })
        .collect()
}

/// Record stale-working and heartbeat-missing faults for the current snapshot.
pub fn observe_once(state: &State, now: DateTime<Utc>) -> anyhow::Result<Vec<FaultEvent>> {
    let working_rows = state
        .ledger
        .list_sessions(QuerySessionsArgs {
            lifecycle: Some(Lifecycle::Working),
            limit: 500,
            ..QuerySessionsArgs::default()
        })?
        .iter()
        .filter_map(WorkingSession::from_row)
        .collect::<Vec<_>>();
    let online_roles = state
        .roles
        .read()
        .map(|roles| roles.entries.keys().cloned().collect::<HashSet<_>>())
        .unwrap_or_default();
    let open_faults = state
        .ledger
        .faults_query(FaultQuery {
            open_only: true,
            limit: 500,
            ..FaultQuery::default()
        })?
        .into_iter()
        .filter_map(|row| {
            Some(OpenFault {
                task_id: row.task_id?,
                kind: row.kind,
            })
        })
        .collect::<Vec<_>>();
    let grace_secs = state
        .spec_snapshot()
        .map(|spec| spec.server.heartbeat_grace_secs)
        .unwrap_or(onlyne_config::DEFAULT_HEARTBEAT_GRACE_SECS);
    let mut recorded = Vec::new();
    for draft in scan(
        &working_rows,
        &online_roles,
        now,
        STALE_WATCH_GRACE_SECS,
        &open_faults,
    ) {
        recorded.push(faults::record(state, draft)?);
    }
    for draft in scan_heartbeat(
        &working_rows,
        &online_roles,
        &|task_id| state.has_seen_session(task_id),
        now,
        grace_secs,
        &open_faults,
    ) {
        recorded.push(faults::record(state, draft)?);
    }
    Ok(recorded)
}

/// Project one stored session onto the residual-account view.
pub fn working_from_lifecycle(
    lifecycle: Lifecycle,
    row: &ServerSessionRow,
) -> Option<WorkingSession> {
    if lifecycle != Lifecycle::Working {
        return None;
    }
    WorkingSession::from_row(row)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn now() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-09-12T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc)
    }

    fn row(task: &str, role: &str, age_secs: i64) -> WorkingSession {
        WorkingSession {
            task_id: task.to_string(),
            role: role.to_string(),
            updated_at: now() - chrono::Duration::seconds(age_secs),
        }
    }

    fn seen_all(_task: &str) -> bool {
        true
    }

    #[test]
    fn offline_and_expired_records_once() {
        let rows = [row("t-stale", "planner", 601)];
        let drafts = scan(&rows, &HashSet::new(), now(), 600, &[]);
        assert_eq!(drafts.len(), 1);
        assert_eq!(drafts[0].kind, KIND_STALE_WORKING);
        assert_eq!(drafts[0].task_id.as_deref(), Some("t-stale"));
        assert_eq!(drafts[0].role.as_deref(), Some("planner"));
    }

    #[test]
    fn online_role_is_skipped() {
        let rows = [row("t-live", "planner", 9_000)];
        let online = HashSet::from(["planner".to_string()]);
        assert!(scan(&rows, &online, now(), 600, &[]).is_empty());
    }

    #[test]
    fn unexpired_is_skipped() {
        let rows = [row("t-young", "planner", 600)];
        assert!(scan(&rows, &HashSet::new(), now(), 600, &[]).is_empty());
    }

    #[test]
    fn second_scan_does_not_repeat_an_open_fault() {
        let rows = [row("t-stale", "planner", 9_000)];
        let open = [OpenFault {
            task_id: "t-stale".to_string(),
            kind: KIND_STALE_WORKING.to_string(),
        }];
        assert!(scan(&rows, &HashSet::new(), now(), 600, &open).is_empty());
    }

    #[test]
    fn online_and_stale_records_heartbeat_missing_once() {
        let rows = [row("t-quiet", "planner", 91)];
        let online = HashSet::from(["planner".to_string()]);
        let drafts = scan_heartbeat(&rows, &online, &seen_all, now(), 90, &[]);
        assert_eq!(drafts.len(), 1);
        assert_eq!(drafts[0].kind, KIND_HEARTBEAT_MISSING);
        assert_eq!(drafts[0].task_id.as_deref(), Some("t-quiet"));
        assert_eq!(drafts[0].role.as_deref(), Some("planner"));
        assert_eq!(
            drafts[0].reason,
            "session t-quiet has sent no heartbeat for 91s while role planner stays connected"
        );
    }

    #[test]
    fn fresh_online_row_is_quiet() {
        let rows = [row("t-fresh", "planner", 90)];
        let online = HashSet::from(["planner".to_string()]);
        assert!(scan_heartbeat(&rows, &online, &seen_all, now(), 90, &[]).is_empty());
    }

    #[test]
    fn offline_row_is_handled_by_stale_working_only() {
        let rows = [row("t-offline", "planner", 9_000)];
        let drafts = scan(&rows, &HashSet::new(), now(), 600, &[]);
        assert_eq!(drafts.len(), 1);
        assert_eq!(drafts[0].kind, KIND_STALE_WORKING);
        assert!(scan_heartbeat(&rows, &HashSet::new(), &seen_all, now(), 90, &[]).is_empty());
    }

    #[test]
    fn never_seen_row_is_quiet() {
        let rows = [row("t-unseen", "planner", 9_000)];
        let online = HashSet::from(["planner".to_string()]);
        let seen = |task: &str| task != "t-unseen";
        assert!(scan_heartbeat(&rows, &online, &seen, now(), 90, &[]).is_empty());
    }

    #[test]
    fn open_heartbeat_missing_fault_dedups() {
        let rows = [row("t-quiet", "planner", 9_000)];
        let online = HashSet::from(["planner".to_string()]);
        let open = [OpenFault {
            task_id: "t-quiet".to_string(),
            kind: KIND_HEARTBEAT_MISSING.to_string(),
        }];
        assert!(scan_heartbeat(&rows, &online, &seen_all, now(), 90, &open).is_empty());
    }
}
