//! The resource layer: panes and their focus, as herdr reports them.

use super::cli::{AGENT_START_TIMEOUT_MS, absolute_cwd, kind_of, pane_run_line};
use super::policy::HerdrRef;
use super::session::HerdrBackend;
use crate::backend::*;

impl HerdrBackend {
    pub(super) fn base_pane(&self, workspace_id: &str, tab_id: &str) -> Result<String> {
        let listed = self.json(vec![
            "pane".into(),
            "list".into(),
            "--workspace".into(),
            workspace_id.into(),
        ])?;
        let panes: Vec<&Value> = listed
            .get("panes")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter(|row| row.get("tab_id").and_then(Value::as_str) == Some(tab_id))
            .collect();
        let chosen = panes
            .iter()
            .find(|row| row.get("focused").and_then(Value::as_bool) == Some(true))
            .or(panes.first())
            .ok_or_else(|| anyhow::anyhow!("herdr tab {tab_id} has no pane to split"))?;
        chosen
            .get("pane_id")
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| anyhow::anyhow!("herdr pane list row missing pane_id"))
    }

    pub(super) fn split_pane(
        &self,
        base_pane: &str,
        spec: &SpawnSpec,
        placement: PanePlacement,
    ) -> Result<String> {
        let mut args = vec![
            "pane".into(),
            "split".into(),
            "--pane".into(),
            base_pane.into(),
            "--direction".into(),
            placement.direction.as_herdr().into(),
            "--ratio".into(),
            format!("{}", placement.ratio),
            "--cwd".into(),
            absolute_cwd(&spec.cwd),
        ];
        for (key, value) in &spec.env {
            args.push("--env".into());
            args.push(format!("{key}={value}"));
        }
        args.push("--no-focus".into());
        let created = self.json(args)?;
        created
            .pointer("/pane/pane_id")
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| anyhow::anyhow!("herdr pane split returned no pane_id"))
    }

    /// Put the session command into the pane, reporting the track taken.
    ///
    /// `true` means herdr runs a managed agent in the pane, which is what makes
    /// `agent focus` answer later. `false` means the command ran as a shell
    /// line through `pane run`, and that pane is reached by anchor navigation.
    pub(super) fn start_in_pane(
        &self,
        pane_id: &str,
        spec: &SpawnSpec,
        agent: &str,
    ) -> Result<bool> {
        match kind_of(&spec.command) {
            Some(kind) => {
                let mut args = vec![
                    "agent".into(),
                    "start".into(),
                    agent.into(),
                    "--kind".into(),
                    kind,
                    "--pane".into(),
                    pane_id.into(),
                    "--timeout".into(),
                    AGENT_START_TIMEOUT_MS.into(),
                ];
                // herdr 0.9.0 spells the call
                // `agent start <NAME> --kind <KIND> --pane <ID> [OPTIONS]
                // [-- [AGENT_ARG]...]`, and `--kind` already selects the
                // executable named by command token 0. The remaining tokens are
                // the agent's own arguments — a `session_command` carries
                // `--session-id <task> --session-dir .pi/sessions` there — so
                // they travel after the `--` separator. A pane started without
                // them runs a bare agent that cannot name its client.
                if spec.command.len() > 1 {
                    args.push("--".into());
                    args.extend(spec.command[1..].iter().cloned());
                }
                let started = self.json(args);
                if started.is_ok() {
                    return Ok(true);
                }
                tracing::warn!(
                    error = %started.as_ref().unwrap_err(),
                    pane = pane_id,
                    "herdr agent start failed; falling back to pane run"
                );
                self.pane_run(pane_id, spec)?;
                Ok(false)
            }
            None => {
                self.pane_run(pane_id, spec)?;
                Ok(false)
            }
        }
    }

    fn pane_run(&self, pane_id: &str, spec: &SpawnSpec) -> Result<()> {
        if spec.command.is_empty() {
            anyhow::bail!("herdr spawn requires a command");
        }
        let line = pane_run_line(&spec.command);
        self.run_line(vec!["pane".into(), "run".into(), pane_id.into(), line])
            .map(|_| ())
    }

    pub(super) fn focused_ids(&self) -> Result<(Option<String>, Option<String>, Option<String>)> {
        let listed = self.json(vec!["pane".into(), "list".into()])?;
        let focused = listed
            .get("panes")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .find(|row| row.get("focused").and_then(Value::as_bool) == Some(true));
        Ok((
            focused
                .and_then(|row| row.get("workspace_id").and_then(Value::as_str))
                .map(str::to_string),
            focused
                .and_then(|row| row.get("tab_id").and_then(Value::as_str))
                .map(str::to_string),
            focused
                .and_then(|row| row.get("pane_id").and_then(Value::as_str))
                .map(str::to_string),
        ))
    }
}

impl HerdrBackend {
    /// Move herdr's pane focus onto one session's pane.
    ///
    /// A managed agent answers `agent focus <pane_id>`. A plain shell pane from
    /// `pane run` has no agent, and herdr answers `agent_not_found` there, so
    /// that pane is reached with the anchor navigation herdr does accept:
    /// `pane focus --pane <base> --direction <d>` focuses the neighbour of
    /// `<base>` in direction `<d>`, which is the pane this session was split
    /// out of `<base>` into. Both values come from the recorded split.
    pub(super) fn focus_pane(&self, reference: &HerdrRef) -> Result<()> {
        if !reference.agent.is_empty() {
            match self.json(vec![
                "agent".into(),
                "focus".into(),
                reference.pane_id.clone(),
            ]) {
                Ok(_) => return Ok(()),
                Err(error) if failure_code(&error) == Some("agent_not_found") => {}
                Err(error) => return Err(error),
            }
        }
        if reference.base_pane.is_empty() || reference.split_direction.is_empty() {
            return Err(anyhow::anyhow!(
                "herdr pane {} has no recorded split anchor, so focus has no direction to walk",
                reference.pane_id
            ));
        }
        self.json(vec![
            "pane".into(),
            "focus".into(),
            "--pane".into(),
            reference.base_pane.clone(),
            "--direction".into(),
            reference.split_direction.clone(),
        ])?;
        Ok(())
    }

    /// Confirm the pane holds herdr's focus, naming the pane that took it.
    ///
    /// The layout can drift when an operator closes or moves panes, and a
    /// neighbour hop then lands somewhere else. A wrong pane holding focus
    /// reports as a failure: silently focusing a neighbour would send the
    /// operator's keyboard to another session.
    pub(super) fn expect_pane_focused(&self, pane_id: &str) -> Result<()> {
        let info = self.json(vec!["pane".into(), "get".into(), pane_id.into()])?;
        if info.pointer("/pane/focused").and_then(Value::as_bool) == Some(true) {
            return Ok(());
        }
        let (_, _, observed) = self.focused_ids().unwrap_or((None, None, None));
        Err(anyhow::anyhow!(
            "herdr left pane {pane_id} unfocused (focused pane {})",
            observed.unwrap_or_else(|| "none".to_string())
        ))
    }
}
