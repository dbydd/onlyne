//! Observe working sessions whose owner role is offline.
//!
//! Detection records a `stale_working` fault and emits `Event::Fault`. Nothing
//! here retries, requeues, or mutates ledger state: recovery stays with the
//! supervisor and the `repair_*` verbs.

use crate::faults::{self, FaultDraft};
use crate::state::State;
use chrono::{DateTime, Utc};
use onlyne_proto::{FaultEvent, Lifecycle, QuerySessionsArgs};
use onlyne_store::{FaultQuery, ServerSessionRow};
use std::collections::HashSet;

/// Fault kind recorded when a working session's owner role is offline past grace.
pub const KIND_STALE_WORKING: &str = "stale_working";

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

/// Record stale-working faults for the server's current ledger snapshot.
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
            kind: Some(KIND_STALE_WORKING.to_string()),
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
}
