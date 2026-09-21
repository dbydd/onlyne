//! Herdr's own policy: what a workspace, a tab and an agent are called, and
//! how each is found or created.

use super::cli::absolute_cwd;
use super::session::HerdrBackend;
use crate::backend::*;

/// Address of one herdr session, stored under `backend_ref.herdr`.
///
/// `base_pane` is the pane that was split. `split_direction` is the
/// string sent to `pane split --direction`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct HerdrRef {
    pub(super) workspace_id: String,
    pub(super) tab_id: String,
    pub(super) pane_id: String,
    pub(super) agent: String,
    pub(super) workspace_label: String,
    pub(super) base_pane: String,
    pub(super) split_direction: String,
}

impl HerdrRef {
    pub(super) fn from_session(session: &SessionRef) -> Result<Self> {
        let herdr = session
            .backend_ref
            .get("herdr")
            .ok_or_else(|| anyhow::anyhow!("herdr session ref missing herdr object"))?;
        let field = |key: &str| -> Result<String> {
            herdr
                .get(key)
                .and_then(Value::as_str)
                .map(str::to_string)
                .ok_or_else(|| anyhow::anyhow!("herdr session ref missing {key}"))
        };
        Ok(Self {
            workspace_id: field("workspace_id")?,
            tab_id: field("tab_id")?,
            pane_id: field("pane_id")?,
            agent: herdr
                .get("agent")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
            workspace_label: herdr
                .get("workspace_label")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
            base_pane: herdr
                .get("base_pane")
                .and_then(Value::as_str)
                .map(str::to_string)
                .unwrap_or_default(),
            split_direction: herdr
                .get("split_direction")
                .and_then(Value::as_str)
                .map(str::to_string)
                .unwrap_or_default(),
        })
    }

    pub(super) fn to_value(&self) -> Value {
        serde_json::json!({
            "herdr": {
                "workspace_id": self.workspace_id,
                "tab_id": self.tab_id,
                "pane_id": self.pane_id,
                "agent": self.agent,
                "workspace_label": self.workspace_label,
                "base_pane": self.base_pane,
                "split_direction": self.split_direction,
            }
        })
    }
}

impl HerdrBackend {
    pub(super) fn workspace_label(spec: &SpawnSpec) -> String {
        let cluster = spec
            .env
            .get("ONLYNE_CLUSTER")
            .map(|value| value.trim())
            .filter(|value| !value.is_empty())
            .unwrap_or("default");
        format!("onlyne:{cluster}")
    }

    pub(super) fn role(spec: &SpawnSpec) -> String {
        spec.env
            .get("ONLYNE_ROLE")
            .map(|value| value.trim())
            .filter(|value| !value.is_empty())
            .unwrap_or("role")
            .to_string()
    }

    pub(super) fn find_or_create_workspace(&self, spec: &SpawnSpec) -> Result<String> {
        let label = Self::workspace_label(spec);
        let listed = self.json(vec!["workspace".into(), "list".into()])?;
        if let Some(id) = listed
            .get("workspaces")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .find(|row| row.get("label").and_then(Value::as_str) == Some(label.as_str()))
            .and_then(|row| row.get("workspace_id").and_then(Value::as_str))
        {
            return Ok(id.to_string());
        }
        let created = self.json(vec![
            "workspace".into(),
            "create".into(),
            "--label".into(),
            label.clone(),
            "--cwd".into(),
            absolute_cwd(&spec.cwd),
            "--no-focus".into(),
        ])?;
        let workspace_id = created
            .pointer("/workspace/workspace_id")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow::anyhow!("herdr workspace create returned no workspace_id"))?
            .to_string();
        // The lookup keys on the label alone, so a workspace holding this
        // session's panes under another label reads as absent here and gains a
        // labelled sibling. The warning names both ids and the rename that
        // makes the next spawn find the operator's workspace.
        tracing::warn!(
            label = label.as_str(),
            workspace_id = workspace_id.as_str(),
            "herdr created a workspace for {label}; rename the workspace to use with \
             `herdr workspace rename <WORKSPACE_ID> {label}` before spawning again"
        );
        Ok(workspace_id)
    }

    pub(super) fn find_or_create_tab(
        &self,
        workspace_id: &str,
        spec: &SpawnSpec,
    ) -> Result<(String, usize, String)> {
        let role = Self::role(spec);
        let listed = self.json(vec![
            "tab".into(),
            "list".into(),
            "--workspace".into(),
            workspace_id.into(),
        ])?;
        if let Some(row) = listed
            .get("tabs")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .find(|row| row.get("label").and_then(Value::as_str) == Some(role.as_str()))
        {
            let tab_id = row
                .get("tab_id")
                .and_then(Value::as_str)
                .ok_or_else(|| anyhow::anyhow!("herdr tab list row missing tab_id"))?
                .to_string();
            // pane_count comes from tab list result.tabs[].pane_count.
            // A missing field is treated as 0, which plans a right split at 0.5.
            let pane_count = row.get("pane_count").and_then(Value::as_u64).unwrap_or(0) as usize;
            let base = self.base_pane(workspace_id, &tab_id)?;
            return Ok((tab_id, pane_count, base));
        }
        let created = self.json(vec![
            "tab".into(),
            "create".into(),
            "--workspace".into(),
            workspace_id.into(),
            "--label".into(),
            role,
            "--cwd".into(),
            absolute_cwd(&spec.cwd),
            "--no-focus".into(),
        ])?;
        let tab_id = created
            .pointer("/tab/tab_id")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow::anyhow!("herdr tab create returned no tab_id"))?
            .to_string();
        let pane_count = created
            .pointer("/tab/pane_count")
            .and_then(Value::as_u64)
            .unwrap_or(1) as usize;
        let base = created
            .pointer("/root_pane/pane_id")
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| anyhow::anyhow!("herdr tab create returned no root_pane.pane_id"))?;
        Ok((tab_id, pane_count, base))
    }
}

pub(super) fn is_agent_name(name: &str) -> bool {
    let bytes = name.as_bytes();
    (1..=32).contains(&bytes.len())
        && bytes[0].is_ascii_lowercase()
        && bytes.iter().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || *byte == b'_' || *byte == b'-'
        })
}

/// Agent name: `onlyne-<role>-<first 8 hex of task_id>`, truncated to 32.
pub(crate) fn agent_name(role: &str, task_id: &str) -> String {
    let mut role: String = role
        .chars()
        .flat_map(char::to_lowercase)
        .filter(|ch| ch.is_ascii_alphanumeric() || *ch == '_' || *ch == '-')
        .collect();
    if role.is_empty() {
        role = "role".into();
    }
    if !role.as_bytes()[0].is_ascii_lowercase() {
        role = format!("r{role}");
    }
    let hex: String = task_id
        .chars()
        .filter(char::is_ascii_hexdigit)
        .take(8)
        .map(|ch| ch.to_ascii_lowercase())
        .collect();
    let hex = if hex.is_empty() {
        "00000000".into()
    } else {
        hex
    };
    let mut name = format!("onlyne-{role}-{hex}");
    name.truncate(32);
    name
}
