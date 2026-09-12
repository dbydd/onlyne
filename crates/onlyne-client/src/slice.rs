//! Role-slice hot reload after `Event::SpecReloaded`.
//!
//! `reconfigure` currently only runs on `welcome`. A live connection that
//! stays up across `onlyne reload` never sees that frame, so this module
//! compares the `query_roles` row against the dispatcher's current slice
//! and calls `reconfigure` only when `max_sessions`, `reuse`, or
//! `session_command` changed.

use onlyne_proto::RoleInfo;

/// The three fields `reconfigure` consumes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoleSlice {
    pub command: Vec<String>,
    pub max_sessions: u32,
    pub reuse: bool,
}

impl RoleSlice {
    pub fn from_welcome(command: Vec<String>, max_sessions: u32, reuse: bool) -> Self {
        Self {
            command,
            max_sessions,
            reuse,
        }
    }

    pub fn from_role_info(info: &RoleInfo, _current: &RoleSlice) -> Self {
        Self {
            command: info.session_command.clone(),
            max_sessions: info.max_sessions,
            reuse: info.reuse,
        }
    }
}

/// Fields that would change if `next` were applied.
pub fn slice_diff(current: &RoleSlice, next: &RoleSlice) -> Vec<&'static str> {
    let mut fields = Vec::new();
    if current.command != next.command {
        fields.push("session_command");
    }
    if current.max_sessions != next.max_sessions {
        fields.push("max_sessions");
    }
    if current.reuse != next.reuse {
        fields.push("reuse");
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
    use onlyne_proto::Presence;

    fn slice(max: u32, reuse: bool) -> RoleSlice {
        RoleSlice {
            command: vec!["pi".into()],
            max_sessions: max,
            reuse,
        }
    }

    fn info(max: u32) -> RoleInfo {
        RoleInfo {
            name: "planner".into(),
            admin: false,
            max_sessions: max,
            reuse: true,
            session_command: vec!["pi".into()],
            spec_hash: "h".into(),
            prose: None,
            state: Presence::Online,
            sessions: 0,
            detail: None,
            edges: Vec::new(),
            aggregate: None,
        }
    }

    #[test]
    fn a_changed_max_sessions_is_applied() {
        let current = slice(1, true);
        let next = RoleSlice::from_role_info(&info(2), &current);
        let (applied, fields) = apply_if_changed(&current, next).expect("a change");
        assert_eq!(applied.max_sessions, 2);
        assert_eq!(fields, ["max_sessions"]);
    }

    #[test]
    fn an_identical_slice_is_a_no_op() {
        let current = slice(2, true);
        let next = RoleSlice::from_role_info(&info(2), &current);
        assert!(apply_if_changed(&current, next).is_none());
    }

    #[test]
    fn reuse_and_command_are_compared() {
        let current = slice(1, false);
        let next = RoleSlice {
            command: vec!["other".into()],
            max_sessions: 1,
            reuse: true,
        };
        let fields = slice_diff(&current, &next);
        assert_eq!(fields, ["session_command", "reuse"]);
    }
}
