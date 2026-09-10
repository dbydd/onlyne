use crate::spec::{ClientEntry, Spec};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashSet};

/// Reload diff between two validated [`Spec`] documents.
///
/// Computed with [`SpecDiff::between`]. The struct is order stable: every list
/// is sorted by name so `--dry-run` output compares deterministically.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
pub struct SpecDiff {
    /// Roles present in the new spec only.
    pub added_roles: Vec<String>,
    /// Roles present in the old spec only.
    pub removed_roles: Vec<String>,
    /// Roles present in both specs with changed fields.
    pub changed_roles: Vec<RoleChange>,
    /// Route keys present in the new spec only.
    pub added_routes: Vec<String>,
    /// Route keys present in the old spec only.
    pub removed_routes: Vec<String>,
}

/// Field-level changes for one role that survived the reload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct RoleChange {
    /// Role name.
    pub role: String,
    /// Changed top-level fields by key.
    pub changed_fields: Vec<String>,
}

/// One field value replacement inside a role.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct FieldChange {
    /// Field key.
    pub field: String,
    /// Previous rendering.
    pub before: String,
    /// New rendering.
    pub after: String,
}

impl SpecDiff {
    /// Diff two specs. Roles are keyed by `role`; routes are keyed by
    /// `gateway/channel/conversation/to.role/to.session` in document order.
    pub fn between(before: &Spec, after: &Spec) -> Self {
        let before_roles = role_map(&before.client);
        let after_roles = role_map(&after.client);
        let before_names: HashSet<&str> = before_roles.keys().copied().collect();
        let after_names: HashSet<&str> = after_roles.keys().copied().collect();

        let mut added_roles: Vec<String> = after_names
            .difference(&before_names)
            .map(ToString::to_string)
            .collect();
        added_roles.sort();
        let mut removed_roles: Vec<String> = before_names
            .difference(&after_names)
            .map(ToString::to_string)
            .collect();
        removed_roles.sort();
        let mut changed_roles = Vec::new();
        for name in before_names.intersection(&after_names) {
            let before_entry = &before_roles[name];
            let after_entry = &after_roles[name];
            let changed_fields = changed_client_fields(before_entry, after_entry);
            if !changed_fields.is_empty() {
                changed_roles.push(RoleChange {
                    role: name.to_string(),
                    changed_fields,
                });
            }
        }
        changed_roles.sort_by(|a, b| a.role.cmp(&b.role));

        let before_routes = route_keys(before);
        let after_routes = route_keys(after);
        let before_set: HashSet<&String> = before_routes.iter().collect();
        let after_set: HashSet<&String> = after_routes.iter().collect();
        let mut added_routes: Vec<String> = after_set
            .difference(&before_set)
            .map(|key| (*key).clone())
            .collect();
        added_routes.sort();
        let mut removed_routes: Vec<String> = before_set
            .difference(&after_set)
            .map(|key| (*key).clone())
            .collect();
        removed_routes.sort();

        Self {
            added_roles,
            removed_roles,
            changed_roles,
            added_routes,
            removed_routes,
        }
    }

    /// Empty when no role or route rows changed.
    pub fn is_empty(&self) -> bool {
        self.added_roles.is_empty()
            && self.removed_roles.is_empty()
            && self.changed_roles.is_empty()
            && self.added_routes.is_empty()
            && self.removed_routes.is_empty()
    }

    /// Human-readable `--dry-run` text with one section per non-empty delta.
    pub fn render(&self) -> String {
        if self.is_empty() {
            return "spec: no changes".to_string();
        }
        let mut lines = Vec::new();
        for role in &self.added_roles {
            lines.push(format!("add role {role}"));
        }
        for role in &self.removed_roles {
            lines.push(format!("remove role {role}"));
        }
        for change in &self.changed_roles {
            let mut fields = change.changed_fields.clone();
            fields.sort();
            lines.push(format!(
                "change role {}: {}",
                change.role,
                fields.join(", ")
            ));
        }
        for route in &self.added_routes {
            lines.push(format!("add route {route}"));
        }
        for route in &self.removed_routes {
            lines.push(format!("remove route {route}"));
        }
        lines.join("\n")
    }
}

fn role_map(clients: &[ClientEntry]) -> BTreeMap<&str, &ClientEntry> {
    clients
        .iter()
        .map(|entry| (entry.role.as_str(), entry))
        .collect()
}

fn route_keys(spec: &Spec) -> Vec<String> {
    spec.route
        .iter()
        .map(|route| {
            let conversation = route.conversation.as_deref().unwrap_or("*");
            let session = route.to.session.as_deref().unwrap_or("-");
            format!(
                "{}/{}/{}/{}/{session}",
                route.gateway, route.channel, conversation, route.to.role
            )
        })
        .collect()
}

fn changed_client_fields(before: &ClientEntry, after: &ClientEntry) -> Vec<String> {
    let mut fields = Vec::new();
    if before.key != after.key {
        fields.push("key".to_string());
    }
    if before.prose != after.prose {
        fields.push("prose".to_string());
    }
    if before.admin != after.admin {
        fields.push("admin".to_string());
    }
    if before.max_sessions != after.max_sessions {
        fields.push("max_sessions".to_string());
    }
    if before.reuse != after.reuse {
        fields.push("reuse".to_string());
    }
    if before.allowed_senders != after.allowed_senders {
        fields.push("allowed_senders".to_string());
    }
    if before.allowed_targets != after.allowed_targets {
        fields.push("allowed_targets".to_string());
    }
    if before.session_command != after.session_command {
        fields.push("session_command".to_string());
    }
    if before.timeout != after.timeout {
        fields.push("timeout".to_string());
    }
    if before.intent != after.intent {
        fields.push("intent".to_string());
    }
    if before.aggregate != after.aggregate {
        fields.push("aggregate".to_string());
    }
    fields
}
