//! Role-slice hot reload after `Event::SpecReloaded`.
//!
//! `reconfigure` currently only runs on `welcome`. A live connection that
//! stays up across `onlyne reload` never sees that frame, so this module
//! compares the `query_roles` row against the dispatcher's current slice
//! and calls `reconfigure` only when `max_sessions`,
//! `session_command`, or the relay policy changed.

use onlyne_proto::{RoleInfo, Welcome};

/// The fields `reconfigure` consumes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoleSlice {
    pub command: Vec<String>,
    pub max_sessions: u32,
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
            relay_required: welcome.relay_required.clone().unwrap_or_default(),
            relay_count: welcome.relay_count,
        }
    }

    pub fn from_role_info(info: &RoleInfo, _current: &RoleSlice) -> Self {
        Self {
            command: info.session_command.clone(),
            max_sessions: info.max_sessions,
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
mod tests;
