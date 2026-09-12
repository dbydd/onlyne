//! Role-slice hot reload after `Event::SpecReloaded`.
//!
//! `reconfigure` currently only runs on `welcome`. A live connection that
//! stays up across `onlyne reload` never sees that frame, so this module
//! compares the `query_roles` row against the dispatcher's current slice
//! and calls `reconfigure` only when `max_sessions`, `reuse`,
//! `session_command`, or the relay policy changed.

use onlyne_proto::{RoleInfo, Welcome};

/// The fields `reconfigure` consumes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoleSlice {
    pub command: Vec<String>,
    pub max_sessions: u32,
    pub reuse: bool,
    /// Downstream handoffs a session of this role owes (`relay_required`).
    pub relay_required: Vec<String>,
    /// The count form of the same policy (`relay_count`); a non-empty list wins
    /// when both are present, which is the guard's own precedence.
    pub relay_count: Option<u32>,
}

impl RoleSlice {
    pub fn from_welcome(welcome: &Welcome) -> Self {
        Self {
            command: welcome.session_command.clone().unwrap_or_default(),
            max_sessions: welcome.max_sessions,
            reuse: welcome.reuse,
            relay_required: welcome.relay_required.clone().unwrap_or_default(),
            relay_count: welcome.relay_count,
        }
    }

    pub fn from_role_info(info: &RoleInfo, _current: &RoleSlice) -> Self {
        Self {
            command: info.session_command.clone(),
            max_sessions: info.max_sessions,
            reuse: info.reuse,
            relay_required: info.relay_required.clone().unwrap_or_default(),
            relay_count: info.relay_count,
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
    if current.relay_required != next.relay_required {
        fields.push("relay_required");
    }
    if current.relay_count != next.relay_count {
        fields.push("relay_count");
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
            relay_required: Vec::new(),
            relay_count: None,
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
            relay_required: None,
            relay_count: None,
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
            relay_required: Vec::new(),
            relay_count: None,
        };
        let fields = slice_diff(&current, &next);
        assert_eq!(fields, ["session_command", "reuse"]);
    }

    #[test]
    fn a_changed_relay_policy_is_compared() {
        let current = slice(1, true);
        let armed = RoleSlice {
            relay_required: vec!["writer".into()],
            relay_count: Some(2),
            ..current.clone()
        };
        assert_eq!(
            slice_diff(&current, &armed),
            ["relay_required", "relay_count"],
            "a list, a count, and the pair each report the keys they moved"
        );

        let counted = RoleSlice {
            relay_count: Some(2),
            ..current.clone()
        };
        assert_eq!(slice_diff(&current, &counted), ["relay_count"]);

        // The role row the client adopts carries the same policy it would see
        // in a welcome, so a reload arms a live session's next spawn.
        let from_row = RoleSlice::from_role_info(
            &RoleInfo {
                relay_required: Some(vec!["writer".into()]),
                relay_count: Some(2),
                ..info(1)
            },
            &current,
        );
        assert_eq!(from_row.relay_required, vec!["writer".to_string()]);
        assert_eq!(
            slice_diff(&current, &from_row),
            ["relay_required", "relay_count"]
        );
    }
}
