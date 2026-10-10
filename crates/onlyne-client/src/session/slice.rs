//! Role-slice hot reload after `Event::SpecReloaded`.
//!
//! `reconfigure` currently only runs on `welcome`. A live connection that
//! stays up across `onlyne reload` never sees that frame, so this module
//! compares the `query_roles` row against the dispatcher's current slice
//! and calls `reconfigure` only when `runtime`, `max_sessions`, or the role's
//! `owes_targets` changed.
//!
//! The drive travels in the slice because it is the runtime's property, read
//! from the spec's `[client.runtime]`; the placement it pairs with is the
//! machine's and never appears here (`docs/v2-PLAN.md` §"驱动与放置").

use onlyne_config::Drive;
use onlyne_proto::{RoleInfo, Welcome};

/// The fields `reconfigure` consumes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoleSlice {
    /// How the client talks to the runtime (`[client.runtime] drive`).
    pub drive: Drive,
    pub command: Vec<String>,
    pub max_sessions: u32,
    /// The roles a session of this role owes a delivery to (`owes_targets`).
    /// Reach and obligation are separate declarations: the server gates the
    /// ACL on `allowed_targets`, and the completion guard owes this list.
    pub required_targets: Vec<String>,
}

impl RoleSlice {
    pub fn from_welcome(welcome: &Welcome) -> Self {
        let runtime = welcome.runtime.clone().unwrap_or_default();
        Self {
            drive: drive_of(runtime.drive),
            command: runtime.command,
            max_sessions: welcome.max_sessions,
            required_targets: welcome.owes_targets.clone(),
        }
    }

    pub fn from_role_info(info: &RoleInfo, _current: &RoleSlice) -> Self {
        Self {
            drive: drive_of(info.runtime.drive),
            command: info.runtime.command.clone(),
            max_sessions: info.max_sessions,
            required_targets: info.owes_targets.clone(),
        }
    }
}

/// The wire's drive as the file's drive.
///
/// The two vocabularies are separate on purpose — `onlyne-config` owns the
/// spelling a file uses and `onlyne-proto` the one a frame uses — so this is
/// the one place the client crosses between them, and the pair rule
/// (`onlyne_config::validate_drive_placement`) is written in the file's terms.
pub fn drive_of(wire: onlyne_proto::Drive) -> Drive {
    match wire {
        onlyne_proto::Drive::Plugin => Drive::Plugin,
        onlyne_proto::Drive::Acp => Drive::Acp,
        onlyne_proto::Drive::Exec => Drive::Exec,
    }
}

/// Fields that would change if `next` were applied.
pub fn slice_diff(current: &RoleSlice, next: &RoleSlice) -> Vec<&'static str> {
    let mut fields = Vec::new();
    if current.drive != next.drive || current.command != next.command {
        fields.push("runtime");
    }
    if current.max_sessions != next.max_sessions {
        fields.push("max_sessions");
    }
    if current.required_targets != next.required_targets {
        fields.push("owes_targets");
    }
    fields
}

/// Apply `next` when it differs; return the fields that changed.
pub fn apply_if_changed(
    current: &RoleSlice,
    next: RoleSlice,
) -> Option<(RoleSlice, Vec<&'static str>)> {
    let fields = slice_diff(current, &next);
    if fields.is_empty() {
        None
    } else {
        Some((next, fields))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn welcome(edges: &[&str], owes: &[&str]) -> Welcome {
        Welcome {
            cluster: "cluster-a".to_string(),
            server: "srv".to_string(),
            role: "planner".to_string(),
            admin: false,
            max_sessions: 3,
            prose: String::new(),
            spec_hash: "hash".to_string(),
            aggregate: None,
            allowed_targets: edges.iter().map(|name| name.to_string()).collect(),
            owes_targets: owes.iter().map(|name| name.to_string()).collect(),
            allowed_senders: Vec::new(),
            runtime: None,
            timeout_ready_ms: None,
            timeout_idle_ms: None,
            intent_attempts: None,
            intent_backoff_ms: None,
            seq: 1,
        }
    }

    /// Reach is permission and never compels a delivery: the slice's obligation
    /// is `owes_targets`, and `allowed_targets` — which may name ten roles the
    /// session never owes — is not read here at all.
    #[test]
    fn the_slice_owes_owes_targets_not_the_reach() {
        let slice = RoleSlice::from_welcome(&welcome(&["writer", "auditor"], &["writer"]));
        assert_eq!(slice.required_targets, ["writer"], "{slice:?}");

        let reach_only = RoleSlice::from_welcome(&welcome(&["writer", "auditor"], &[]));
        assert!(
            reach_only.required_targets.is_empty(),
            "a role that declares no obligation owes nothing: {reach_only:?}"
        );
    }

    /// A reload that moves only the obligation is worth a `reconfigure`, and
    /// one that moves only the reach is not — the client holds no ACL.
    #[test]
    fn the_slice_diff_reports_the_obligation_alone() {
        let base = RoleSlice::from_welcome(&welcome(&["writer"], &["writer"]));
        let moved_obligation =
            RoleSlice::from_welcome(&welcome(&["writer", "auditor"], &["auditor"]));
        assert_eq!(
            slice_diff(&base, &moved_obligation),
            vec!["owes_targets"],
            "the reach moved too, and it is the server's to read"
        );
        let moved_reach = RoleSlice::from_welcome(&welcome(&["writer", "auditor"], &["writer"]));
        assert!(
            slice_diff(&base, &moved_reach).is_empty(),
            "a reach-only change reconfigures nothing on the client"
        );
    }
}
