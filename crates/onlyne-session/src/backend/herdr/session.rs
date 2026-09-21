//! The session layer: the backend's own type and its [`SessionBackend`]
//! implementation.

use super::agent_name;
use super::policy::{HerdrRef, is_agent_name};
use crate::backend::*;
use std::collections::BTreeMap;
use std::sync::Arc;

pub struct HerdrBackend {
    pub(super) runner: Arc<dyn Runner>,
    pub(super) command: String,
    pub(super) env: BTreeMap<String, String>,
}

impl HerdrBackend {
    pub fn new(runner: Arc<dyn Runner>) -> Self {
        Self::with_env(runner, process_env())
    }

    pub fn with_env(runner: Arc<dyn Runner>, env: BTreeMap<String, String>) -> Self {
        let command = env
            .get("HERDR_BIN_PATH")
            .cloned()
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| "herdr".into());
        Self {
            runner,
            command,
            env,
        }
    }
}

impl SessionBackend for HerdrBackend {
    fn name(&self) -> &'static str {
        "herdr"
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            spawn: true,
            attach: true,
            probe: true,
            close: true,
            focus: true,
            rename: true,
        }
    }

    fn available(&self) -> Result<bool> {
        Ok(herdr_host_present(&self.env))
    }

    fn spawn(&self, spec: SpawnSpec) -> Result<SessionRef> {
        if spec.command.is_empty() {
            anyhow::bail!("herdr spawn requires a command");
        }
        let workspace_label = Self::workspace_label(&spec);
        let workspace_id = self.find_or_create_workspace(&spec)?;
        let (tab_id, pane_count, base_pane) = self.find_or_create_tab(&workspace_id, &spec)?;
        // A missing tab-list pane_count is treated as 0, which plans a right split.
        let placement = spec
            .placement
            .unwrap_or_else(|| PanePlacement::from_pane_count(pane_count));
        tracing::info!(
            pane_count,
            direction = placement.direction.as_herdr(),
            ratio = placement.ratio,
            "herdr pane split"
        );
        let pane_id = self.split_pane(&base_pane, &spec, placement)?;
        let agent = agent_name(&Self::role(&spec), &spec.task_id);
        let managed_agent = self.start_in_pane(&pane_id, &spec, &agent)?;
        let reference = HerdrRef {
            workspace_id,
            tab_id,
            pane_id,
            // Only a managed agent answers `agent focus`. A `pane run` session
            // records an empty agent so focus takes the anchor path directly.
            agent: if managed_agent { agent } else { String::new() },
            workspace_label,
            base_pane,
            split_direction: placement.direction.as_herdr().to_string(),
        };
        Ok(SessionRef {
            task_id: spec.task_id,
            backend: self.name().into(),
            backend_ref: reference.to_value(),
            generation: 1,
        })
    }

    fn attach(&self, session: &SessionRef) -> Result<SessionRef> {
        let probe = self.probe(session)?;
        if !probe.alive {
            anyhow::bail!("herdr pane is gone");
        }
        Ok(session.clone())
    }

    fn probe(&self, session: &SessionRef) -> Result<ResourceProbe> {
        let reference = HerdrRef::from_session(session)?;
        let value = match self.json(vec!["pane".into(), "get".into(), reference.pane_id.clone()]) {
            Ok(value) => value,
            Err(error) => {
                return Ok(ResourceProbe {
                    alive: false,
                    attached: false,
                    detail: Some(serde_json::json!({"error": error.to_string()})),
                });
            }
        };
        let pane = value.get("pane").cloned().unwrap_or(value.clone());
        let agent_status = pane.get("agent_status").cloned();
        let mut detail = serde_json::json!({
            "pane": pane,
            "agent_status": agent_status,
        });
        if let Ok(info) = self.json(vec![
            "pane".into(),
            "process-info".into(),
            "--pane".into(),
            reference.pane_id,
        ]) {
            if let Some(map) = detail.as_object_mut() {
                map.insert("process_info".into(), info);
            }
        }
        Ok(ResourceProbe {
            alive: true,
            attached: true,
            detail: Some(detail),
        })
    }

    fn close(&self, session: &SessionRef, reason: CloseReason, force: bool) -> Result<()> {
        // herdr pane close has no force flag; both close paths use the same command.
        tracing::debug!(task = %session.task_id, ?reason, force, "closing herdr pane");
        let reference = HerdrRef::from_session(session)?;
        match self.json(vec![
            "pane".into(),
            "close".into(),
            reference.pane_id.clone(),
        ]) {
            Ok(_) => Ok(()),
            Err(error)
                if error
                    .downcast_ref::<CommandFailure>()
                    .and_then(CommandFailure::code)
                    == Some("pane_not_found") =>
            {
                tracing::debug!(
                    task = %session.task_id,
                    pane = %reference.pane_id,
                    "herdr pane already closed"
                );
                Ok(())
            }
            Err(error) => Err(error),
        }
    }

    fn rename(&self, session: &SessionRef, title: &str) -> Result<()> {
        let reference = HerdrRef::from_session(session)?;
        self.json(vec![
            "pane".into(),
            "rename".into(),
            reference.pane_id.clone(),
            title.into(),
        ])?;
        if !reference.agent.is_empty() && is_agent_name(title) {
            self.json(vec![
                "agent".into(),
                "rename".into(),
                reference.agent,
                title.into(),
            ])?;
        }
        Ok(())
    }

    fn focus(&self, session: &SessionRef) -> Result<()> {
        let reference = HerdrRef::from_session(session)?;
        let (workspace, tab, pane) = self.focused_ids().unwrap_or((None, None, None));
        if pane.as_deref() == Some(reference.pane_id.as_str()) {
            return Ok(());
        }
        if workspace.as_deref() != Some(reference.workspace_id.as_str()) {
            self.json(vec![
                "workspace".into(),
                "focus".into(),
                reference.workspace_id.clone(),
            ])?;
        }
        if tab.as_deref() != Some(reference.tab_id.as_str()) {
            self.json(vec!["tab".into(), "focus".into(), reference.tab_id.clone()])?;
        }
        self.focus_pane(&reference)?;
        self.expect_pane_focused(&reference.pane_id)
    }
}
