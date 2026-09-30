//! Role admission and the concrete ACL evaluator.
//!
//! `onlyne-config` owns wildcard meaning: [`onlyne_config::Spec::acl_edges`]
//! expands `"*"` against the registered role names and returns concrete pairs.
//! This module stores those pairs and answers one permit question by looking the
//! pair up. A `"*"` endpoint that reaches [`AclTable::new`] is a wiring bug, and
//! the constructor refuses it.
//!
//! [`AclTable`] also carries the role registry, which is what the `hello`
//! challenge verifies a presented key against and what turns an unregistered
//! name into [`AclDenyReason::UnknownRole`].
//!
//! Source of the class split: `docs/v1-PLAN.md` §3 and §5.

use ed25519_dalek::VerifyingKey;
use std::collections::HashMap;

use crate::{NetError, identity::parse_public};

/// The one string that onlyne-config expands and this module refuses.
const WILDCARD: &str = "*";

/// The three classes every permitted pair carries, in `onlyne_config` order.
const ACL_CLASSES: [MsgClass; 3] = [MsgClass::Any, MsgClass::Note, MsgClass::Control];

/// One registered role: its name, its ed25519 key, and its spec `admin` flag.
#[derive(Debug, Clone)]
pub struct RoleAcl {
    pub name: String,
    pub key: VerifyingKey,
    pub admin: bool,
}

/// One permitted directed pair for one message class.
///
/// The row shape mirrors `onlyne_config::AclEdge` field for field, with
/// [`MsgClass`] standing in for `onlyne_config::MsgKindClass` so this crate stays
/// below `onlyne-config` in the layering. `admin` is the sender's `admin`
/// setting, which the `Control` rule consults.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AclEdge {
    pub from: String,
    pub to: String,
    pub class: MsgClass,
    pub admin: bool,
}

/// Registered roles plus the concrete pairs between them.
#[derive(Debug, Clone, Default)]
pub struct AclTable {
    roles: HashMap<String, RoleAcl>,
    edges: HashMap<String, Vec<EdgeRow>>,
}

#[derive(Debug, Clone)]
struct EdgeRow {
    to: String,
    class: MsgClass,
    admin: bool,
}

impl AclTable {
    /// Build the table from registered roles and concrete pairs.
    ///
    /// One role entry is `(name, key, admin)` with the key written
    /// `ed25519/<base64>`. One [`AclEdge`] is `{from, to, class, admin}`, the row
    /// shape `onlyne_config::Spec::acl_edges` returns once it has expanded every
    /// wildcard. A `"*"` endpoint answers [`NetError::MalformedKey`], because the
    /// expansion belongs to `onlyne-config` and a wildcard here would make this
    /// evaluator a second arbiter. Repeated rows collapse.
    pub fn new(
        roles: impl IntoIterator<Item = (String, String, bool)>,
        edges: impl IntoIterator<Item = AclEdge>,
    ) -> Result<Self, NetError> {
        let mut table = AclTable::default();
        for (name, key, admin) in roles {
            if table.roles.contains_key(&name) {
                return Err(NetError::MalformedKey(format!("duplicate role {name}")));
            }
            let key = parse_public(&key)?;
            table
                .roles
                .insert(name.clone(), RoleAcl { name, key, admin });
        }
        for edge in edges {
            table.push_edge(edge)?;
        }
        Ok(table)
    }

    /// Register one role after construction.
    pub fn insert_role(&mut self, name: String, key: String, admin: bool) -> Result<(), NetError> {
        if self.roles.contains_key(&name) {
            return Err(NetError::MalformedKey(format!("duplicate role {name}")));
        }
        let key = parse_public(&key)?;
        self.roles
            .insert(name.clone(), RoleAcl { name, key, admin });
        Ok(())
    }

    /// Add one concrete pair.
    pub fn insert_edge(&mut self, edge: AclEdge) -> Result<(), NetError> {
        self.push_edge(edge)
    }

    fn push_edge(&mut self, edge: AclEdge) -> Result<(), NetError> {
        let AclEdge {
            from,
            to,
            class,
            admin,
        } = edge;
        for endpoint in [&from, &to] {
            if endpoint == WILDCARD {
                return Err(NetError::MalformedKey(format!(
                    "acl edge {from} -> {to} carries the wildcard {WILDCARD:?}; expand it in onlyne-config first"
                )));
            }
        }
        let rows = self.edges.entry(from).or_default();
        if rows.iter().any(|row| row.to == to && row.class == class) {
            return Ok(());
        }
        rows.push(EdgeRow { to, class, admin });
        Ok(())
    }

    pub fn get(&self, name: &str) -> Option<&RoleAcl> {
        self.roles.get(name)
    }

    pub fn roles(&self) -> impl Iterator<Item = &RoleAcl> {
        self.roles.values()
    }

    /// Whether the pair is present for this class.
    pub fn permits(&self, from: &str, to: &str, class: MsgClass) -> bool {
        self.edges
            .get(from)
            .is_some_and(|rows| rows.iter().any(|row| row.to == to && row.class == class))
    }
}

/// Build the table from one concrete allow list per role side.
///
/// Entry shape: `(name, key, admin, allowed_senders, allowed_targets)` with both
/// lists holding concrete role names. A `"*"` in either list answers
/// [`NetError::MalformedKey`], because `onlyne-config` owns wildcard expansion
/// and a second arbiter would let the two disagree. A pair is permitted when the
/// sender names the target and the target names the sender, and the pair carries
/// all three classes.
///
/// Callers that already hold concrete pairs pass them to [`AclTable::new`]
/// instead. `onlyne-server` derives both lists from `Spec::acl_edges`, the
/// concrete pair set, so the two entry points agree there.
pub fn table_from(
    entries: impl IntoIterator<Item = (String, String, bool, Vec<String>, Vec<String>)>,
) -> Result<AclTable, NetError> {
    let entries: Vec<_> = entries.into_iter().collect();
    let mut table = AclTable::default();
    for (name, key, admin, senders, targets) in &entries {
        for (side, list) in [("allowed_senders", senders), ("allowed_targets", targets)] {
            if list.iter().any(|endpoint| endpoint == WILDCARD) {
                return Err(NetError::MalformedKey(format!(
                    "role {name} list {side} carries the wildcard {WILDCARD:?}; expand it in onlyne-config first"
                )));
            }
        }
        table.insert_role(name.clone(), key.clone(), *admin)?;
    }
    for (name, _, admin, _, targets) in &entries {
        for to in targets {
            let accepted = entries.iter().any(|(other, _, _, senders, _)| {
                other == to && senders.iter().any(|sender| sender == name)
            });
            if !accepted {
                continue;
            }
            for class in ACL_CLASSES {
                table.insert_edge(AclEdge {
                    from: name.clone(),
                    to: to.clone(),
                    class,
                    admin: *admin,
                })?;
            }
        }
    }
    Ok(table)
}

/// Message class dimension of the ACL table.
///
/// [`MsgClass`] mirrors `onlyne_config::MsgKindClass` one for one: [`MsgClass::Any`]
/// covers `Task` and `Completion` traffic, [`MsgClass::Note`] covers free-text
/// `Note` traffic that creates no session, and [`MsgClass::Control`] covers
/// `recycle`, `probe`, `snapshot`, and `cancel`. The server maps an
/// `onlyne_proto::MsgKind` onto this class before it asks the table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MsgClass {
    /// Ordinary directed delivery: `Task` and `Completion` traffic.
    Any,
    /// Free-text `Note` traffic that creates no session.
    Note,
    /// Control-plane ops: `recycle`, `probe`, `snapshot`, `cancel`.
    Control,
}

impl MsgClass {
    /// Wire spelling shared with `onlyne_config::MsgKindClass`.
    pub fn name(self) -> &'static str {
        match self {
            MsgClass::Any => "any",
            MsgClass::Note => "note",
            MsgClass::Control => "control",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AclDenyReason {
    UnknownRole,
    SenderNotAllowed,
    TargetNotAllowed,
    AdminRequired,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AclDeny {
    pub reason: AclDenyReason,
    pub field: &'static str,
    pub detail: String,
}

/// Decide one delivery: a registered pair carries the message, and `Control`
/// additionally needs the sender's `admin` flag or task ownership.
///
/// `owner` is the task owner the caller resolved from `causality.task`. The
/// `field` values `from.role`, `to.role`, and `admin` travel to the wire in
/// `ErrorPayload::field`, so callers must not rename them.
///
/// The operator's standing is built in, not declared. `_supervisor` names
/// whoever runs the cluster — agent or human — rather than a process, so a
/// cluster that never carried a `[[client]]` entry for it still answers its
/// sends: to every registered role, in every class, `admin` implied. Its inbox
/// is the same standing read the other way: a registered role's delivery to the
/// undeclared operator carries, because a task the operator dispatched has
/// nowhere else to return. A declared `_supervisor` is an ordinary role; the
/// spec's own rows govern it, and an explicit narrowing of its `allowed_targets`
/// holds.
pub fn acl_allows(
    table: &AclTable,
    from: &str,
    to: &str,
    class: MsgClass,
    owner: Option<&str>,
) -> Result<(), AclDeny> {
    if from == OPERATOR_ROLE && !table.roles.contains_key(from) {
        return table.roles.get(to).map(|_| ()).ok_or_else(|| AclDeny {
            reason: AclDenyReason::UnknownRole,
            field: "to.role",
            detail: format!("unknown target role {to}"),
        });
    }
    if to == OPERATOR_ROLE && !table.roles.contains_key(to) {
        return table.roles.get(from).map(|_| ()).ok_or_else(|| AclDeny {
            reason: AclDenyReason::UnknownRole,
            field: "from.role",
            detail: format!("unknown sender role {from}"),
        });
    }
    table.roles.get(from).ok_or_else(|| AclDeny {
        reason: AclDenyReason::UnknownRole,
        field: "from.role",
        detail: format!("unknown sender role {from}"),
    })?;
    table.roles.get(to).ok_or_else(|| AclDeny {
        reason: AclDenyReason::UnknownRole,
        field: "to.role",
        detail: format!("unknown target role {to}"),
    })?;
    let edge = table
        .edges
        .get(from)
        .and_then(|rows| rows.iter().find(|row| row.to == to && row.class == class))
        .ok_or_else(|| AclDeny {
            reason: AclDenyReason::TargetNotAllowed,
            field: "to.role",
            detail: format!(
                "role {from} may not reach role {to} with a {} message",
                class.name()
            ),
        })?;
    if class == MsgClass::Control && owner != Some(from) && !edge.admin {
        return Err(AclDeny {
            reason: AclDenyReason::AdminRequired,
            field: "admin",
            detail: format!("role {from} is not an administrator or task owner"),
        });
    }
    Ok(())
}

/// The operator's reserved name. `onlyne_config::SUPERVISOR_ROLE` owns the
/// word; this crate sits below `onlyne-config` in the layering and spells the
/// one reserved string locally, so the evaluator and the server's principal
/// resolvers answer it identically.
pub const OPERATOR_ROLE: &str = "_supervisor";

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: &str = "ed25519/AQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQE=";

    /// The undeclared operator's standing, and the rules it does not touch.
    #[test]
    fn the_undeclared_operator_reaches_every_registered_role() {
        let table = AclTable::new(
            [
                ("planner".to_string(), KEY.to_string(), false),
                ("scriber".to_string(), KEY.to_string(), false),
            ],
            [AclEdge {
                from: "planner".to_string(),
                to: "scriber".to_string(),
                class: MsgClass::Any,
                admin: false,
            }],
        )
        .unwrap();
        for class in [MsgClass::Any, MsgClass::Note, MsgClass::Control] {
            assert!(acl_allows(&table, "_supervisor", "planner", class, None).is_ok());
        }
        // The inbox is the same standing read the other way: a registered
        // role's delivery to the undeclared operator carries.
        for class in [MsgClass::Any, MsgClass::Note, MsgClass::Control] {
            assert!(acl_allows(&table, "planner", "_supervisor", class, None).is_ok());
        }
        // An unregistered sender is still an unregistered sender.
        let deny = acl_allows(&table, "ghost", "_supervisor", MsgClass::Any, None).unwrap_err();
        assert_eq!(deny.reason, AclDenyReason::UnknownRole);
        // An unregistered target stays unregistered.
        let deny = acl_allows(&table, "_supervisor", "ghost", MsgClass::Any, None).unwrap_err();
        assert_eq!(deny.reason, AclDenyReason::UnknownRole);
        // The operator's standing grants nothing to an ordinary role.
        let deny = acl_allows(&table, "scriber", "planner", MsgClass::Any, None).unwrap_err();
        assert_eq!(deny.reason, AclDenyReason::TargetNotAllowed);
    }

    /// A declared `_supervisor` is governed by its own rows, so an explicit
    /// narrowing holds.
    #[test]
    fn a_declared_operator_keeps_its_declared_reach() {
        let table = AclTable::new(
            [
                ("_supervisor".to_string(), KEY.to_string(), true),
                ("planner".to_string(), KEY.to_string(), false),
                ("scriber".to_string(), KEY.to_string(), false),
            ],
            [AclEdge {
                from: "_supervisor".to_string(),
                to: "planner".to_string(),
                class: MsgClass::Any,
                admin: true,
            }],
        )
        .unwrap();
        assert!(acl_allows(&table, "_supervisor", "planner", MsgClass::Any, None).is_ok());
        let deny = acl_allows(&table, "_supervisor", "scriber", MsgClass::Any, None).unwrap_err();
        assert_eq!(deny.reason, AclDenyReason::TargetNotAllowed);
    }
}
