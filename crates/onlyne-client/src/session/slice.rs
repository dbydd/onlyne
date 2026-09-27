//! Role-slice hot reload after `Event::SpecReloaded`.
//!
//! `reconfigure` currently only runs on `welcome`. A live connection that
//! stays up across `onlyne reload` never sees that frame, so this module
//! compares the `query_roles` row against the dispatcher's current slice
//! and calls `reconfigure` only when `runtime`, `max_sessions`, or the relay
//! policy changed.
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
    /// Downstream handoffs a session of this role owes (`relay_required`).
    pub relay_required: Vec<String>,
    /// The count form of the same policy (`relay_count`); a non-empty list wins
    /// when both are present, which is the guard's own precedence.
    pub relay_count: Option<u32>,
}

impl RoleSlice {
    pub fn from_welcome(welcome: &Welcome) -> Self {
        let runtime = welcome.runtime.clone().unwrap_or_default();
        Self {
            drive: drive_of(runtime.drive),
            command: runtime.command,
            max_sessions: welcome.max_sessions,
            relay_required: welcome.relay_required.clone().unwrap_or_default(),
            relay_count: welcome.relay_count,
        }
    }

    pub fn from_role_info(info: &RoleInfo, _current: &RoleSlice) -> Self {
        Self {
            drive: drive_of(info.runtime.drive),
            command: info.runtime.command.clone(),
            max_sessions: info.max_sessions,
            relay_required: info.relay_required.clone().unwrap_or_default(),
            relay_count: info.relay_count,
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
