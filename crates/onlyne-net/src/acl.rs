use ed25519_dalek::VerifyingKey;
use std::collections::HashMap;

use crate::{identity::parse_public, NetError};

#[derive(Debug, Clone)]
pub struct RoleAcl {
    pub name: String,
    pub key: VerifyingKey,
    pub admin: bool,
    pub allowed_senders: Vec<String>,
    pub allowed_targets: Vec<String>,
}

#[derive(Debug, Clone, Default)]
pub struct AclTable {
    roles: HashMap<String, RoleAcl>,
}

impl AclTable {
    pub fn get(&self, name: &str) -> Option<&RoleAcl> {
        self.roles.get(name)
    }

    pub fn roles(&self) -> impl Iterator<Item = &RoleAcl> {
        self.roles.values()
    }
}

pub fn table_from(
    entries: impl IntoIterator<Item = (String, String, bool, Vec<String>, Vec<String>)>,
) -> Result<AclTable, NetError> {
    let mut roles = HashMap::new();
    for (name, key, admin, allowed_senders, allowed_targets) in entries {
        if roles.contains_key(&name) {
            return Err(NetError::MalformedKey(format!("duplicate role {name}")));
        }
        let key = parse_public(&key)?;
        roles.insert(name.clone(), RoleAcl { name, key, admin, allowed_senders, allowed_targets });
    }
    Ok(AclTable { roles })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MsgClass {
    Task,
    Completion,
    Note,
    Control,
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

pub fn acl_allows(
    table: &AclTable,
    from: &str,
    to: &str,
    class: MsgClass,
    owner: Option<&str>,
) -> Result<(), AclDeny> {
    let sender = table.roles.get(from).ok_or_else(|| AclDeny {
        reason: AclDenyReason::UnknownRole,
        field: "from.role",
        detail: format!("unknown sender role {from}"),
    })?;
    let target = table.roles.get(to).ok_or_else(|| AclDeny {
        reason: AclDenyReason::UnknownRole,
        field: "to.role",
        detail: format!("unknown target role {to}"),
    })?;
    if !target.allowed_senders.iter().any(|allowed| allowed == "*" || allowed == from) {
        return Err(AclDeny {
            reason: AclDenyReason::SenderNotAllowed,
            field: "from.role",
            detail: format!("sender role {from} is not allowed to deliver to {to}"),
        });
    }
    if !sender.allowed_targets.iter().any(|allowed| allowed == "*" || allowed == to) {
        return Err(AclDeny {
            reason: AclDenyReason::TargetNotAllowed,
            field: "to.role",
            detail: format!("sender role {from} may not reach target role {to}"),
        });
    }
    if class == MsgClass::Control && owner != Some(from) && !sender.admin {
        return Err(AclDeny {
            reason: AclDenyReason::AdminRequired,
            field: "admin",
            detail: format!("role {from} is not an administrator or task owner"),
        });
    }
    Ok(())
}
