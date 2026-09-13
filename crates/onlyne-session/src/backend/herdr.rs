//! Herdr session backend: one pane per onlyne session.
//!
//! A role lives in one herdr tab. Each session splits a new pane inside that
//! tab, then either starts a recognized agent in it or runs the spawn command
//! as a single shell line.

use super::*;
use serde_json::Value;
use std::sync::Arc;

/// herdr `agent start --kind` values, taken from `herdr agent start --help`.
const KIND_TABLE: &[&str] = &[
    "pi",
    "claude",
    "codex",
    "gemini",
    "cursor",
    "devin",
    "agy",
    "cline",
    "omp",
    "mastracode",
    "opencode",
    "copilot",
    "kimi",
    "kiro",
    "droid",
    "amp",
    "grok",
    "hermes",
    "kilo",
    "qodercli",
    "qwen",
    "maki",
    "muse",
];

/// Wait for `agent start` interactive readiness. Measured on herdr 0.9.0.
const AGENT_START_TIMEOUT_MS: &str = "25000";

/// Address of one herdr session, stored under `backend_ref.herdr`.
///
/// `base_pane` is the pane that was split. `split_direction` is the
/// string sent to `pane split --direction`.
#[derive(Debug, Clone, PartialEq, Eq)]
struct HerdrRef {
    workspace_id: String,
    tab_id: String,
    pane_id: String,
    agent: String,
    workspace_label: String,
    base_pane: String,
    split_direction: String,
}

pub struct HerdrBackend {
    runner: Arc<dyn Runner>,
    command: String,
    env: BTreeMap<String, String>,
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

    fn cli_env(&self) -> BTreeMap<String, String> {
        self.env
            .iter()
            .filter(|(key, _)| key.starts_with("HERDR_"))
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect()
    }

    fn json(&self, args: Vec<String>) -> Result<Value> {
        run_json(
            self.runner.as_ref(),
            &self.command,
            &args,
            None,
            &self.cli_env(),
        )
    }

    fn run_line(&self, args: Vec<String>) -> Result<CommandOutput> {
        run_checked(
            self.runner.as_ref(),
            &self.command,
            &args,
            None,
            &self.cli_env(),
        )
    }

    fn workspace_label(spec: &SpawnSpec) -> String {
        let cluster = spec
            .env
            .get("ONLYNE_CLUSTER")
            .map(|value| value.trim())
            .filter(|value| !value.is_empty())
            .unwrap_or("default");
        format!("onlyne:{cluster}")
    }

    fn role(spec: &SpawnSpec) -> String {
        spec.env
            .get("ONLYNE_ROLE")
            .map(|value| value.trim())
            .filter(|value| !value.is_empty())
            .unwrap_or("role")
            .to_string()
    }

    fn find_or_create_workspace(&self, spec: &SpawnSpec) -> Result<String> {
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
            label,
            "--cwd".into(),
            spec.cwd.to_string_lossy().into_owned(),
            "--no-focus".into(),
        ])?;
        created
            .pointer("/workspace/workspace_id")
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| anyhow::anyhow!("herdr workspace create returned no workspace_id"))
    }

    fn find_or_create_tab(
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
            spec.cwd.to_string_lossy().into_owned(),
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

    fn base_pane(&self, workspace_id: &str, tab_id: &str) -> Result<String> {
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

    fn split_pane(
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
            spec.cwd.to_string_lossy().into_owned(),
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
    fn start_in_pane(&self, pane_id: &str, spec: &SpawnSpec, agent: &str) -> Result<bool> {
        match kind_of(&spec.command) {
            Some(kind) => {
                let started = self.json(vec![
                    "agent".into(),
                    "start".into(),
                    agent.into(),
                    "--kind".into(),
                    kind,
                    "--pane".into(),
                    pane_id.into(),
                    "--timeout".into(),
                    AGENT_START_TIMEOUT_MS.into(),
                ]);
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
        let line = spec
            .command
            .iter()
            .map(|arg| shell_quote(arg))
            .collect::<Vec<_>>()
            .join(" ");
        self.run_line(vec!["pane".into(), "run".into(), pane_id.into(), line])
            .map(|_| ())
    }

    fn focused_ids(&self) -> Result<(Option<String>, Option<String>, Option<String>)> {
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
        self.json(vec!["pane".into(), "close".into(), reference.pane_id])
            .map(|_| ())
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

impl HerdrBackend {
    /// Move herdr's pane focus onto one session's pane.
    ///
    /// A managed agent answers `agent focus <pane_id>`. A plain shell pane from
    /// `pane run` has no agent, and herdr answers `agent_not_found` there, so
    /// that pane is reached with the anchor navigation herdr does accept:
    /// `pane focus --pane <base> --direction <d>` focuses the neighbour of
    /// `<base>` in direction `<d>`, which is the pane this session was split
    /// out of `<base>` into. Both values come from the recorded split.
    fn focus_pane(&self, reference: &HerdrRef) -> Result<()> {
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
    fn expect_pane_focused(&self, pane_id: &str) -> Result<()> {
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

impl HerdrRef {
    fn from_session(session: &SessionRef) -> Result<Self> {
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

    fn to_value(&self) -> Value {
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

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn kind_of(command: &[String]) -> Option<String> {
    let first = command.first()?;
    let name = std::path::Path::new(first)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(first)
        .to_ascii_lowercase();
    KIND_TABLE
        .iter()
        .find(|kind| **kind == name)
        .map(|kind| (*kind).to_string())
}

fn is_agent_name(name: &str) -> bool {
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

#[cfg(test)]
mod tests {
    use super::*;
    use parking_lot::Mutex;

    #[derive(Default)]
    struct Script {
        calls: Mutex<Vec<String>>,
        script: Mutex<Vec<(String, i32, String, String)>>,
    }

    impl Script {
        fn reply(self, fragment: &str, status: i32, body: impl Into<String>) -> Self {
            self.script
                .lock()
                .push((fragment.to_string(), status, body.into(), String::new()));
            self
        }

        /// A refused call: herdr puts its error document on stderr with stdout empty.
        fn reply_err(self, fragment: &str, status: i32, stderr: impl Into<String>) -> Self {
            self.script
                .lock()
                .push((fragment.to_string(), status, String::new(), stderr.into()));
            self
        }

        fn calls(&self) -> Vec<String> {
            self.calls.lock().clone()
        }
    }

    impl Runner for Script {
        fn run(
            &self,
            program: &str,
            args: &[String],
            _cwd: Option<&Path>,
            _env: &BTreeMap<String, String>,
        ) -> Result<CommandOutput> {
            let call = format!("{program} {}", args.join(" "));
            self.calls.lock().push(call.clone());
            let mut script = self.script.lock();
            let index = script
                .iter()
                .position(|(fragment, ..)| call.contains(fragment.as_str()))
                .unwrap_or_else(|| panic!("unscripted herdr call: {call}"));
            let (_, status, stdout, stderr) = script.remove(index);
            Ok(CommandOutput {
                status,
                stdout: stdout.into_bytes(),
                stderr: stderr.into_bytes(),
            })
        }
    }

    fn envelope(result: Value) -> String {
        serde_json::json!({"id": "cli:test", "result": result}).to_string()
    }

    fn spec(command: Vec<&str>) -> SpawnSpec {
        let mut env = BTreeMap::new();
        env.insert("ONLYNE_ROLE".into(), "planner".into());
        env.insert("ONLYNE_CLUSTER".into(), "lab".into());
        env.insert("ONLYNE_TASK_ID".into(), "abcd1234ffff".into());
        SpawnSpec {
            cwd: PathBuf::from("/tmp/ws"),
            task_id: "abcd1234-ffff-4000-8000-000000000001".into(),
            command: command.into_iter().map(str::to_string).collect(),
            env,
            focus: None,
            placement: Some(PanePlacement {
                direction: SplitDirection::Right,
                ratio: 0.5,
            }),
            rename: None,
        }
    }

    fn session_ref(pane: &str) -> SessionRef {
        session_ref_with(pane, "onlyne-planner-abcd1234", "wF:p1", "right")
    }

    /// The ref a `pane run` session gets: a pane, an anchor, no managed agent.
    fn shell_session_ref(pane: &str) -> SessionRef {
        session_ref_with(pane, "", "wF:p1", "right")
    }

    fn session_ref_with(pane: &str, agent: &str, base_pane: &str, direction: &str) -> SessionRef {
        SessionRef {
            task_id: "abcd1234-ffff-4000-8000-000000000001".into(),
            backend: "herdr".into(),
            backend_ref: serde_json::json!({
                "herdr": {
                    "workspace_id": "wF",
                    "tab_id": "wF:t1",
                    "pane_id": pane,
                    "agent": agent,
                    "workspace_label": "onlyne:lab",
                    "base_pane": base_pane,
                    "split_direction": direction,
                }
            }),
            generation: 1,
        }
    }

    /// `pane get` answer for one pane, with the focus flag the caller wants.
    fn pane_info(pane: &str, focused: bool) -> String {
        envelope(serde_json::json!({
            "type": "pane_info",
            "pane": {"pane_id": pane, "focused": focused, "agent_status": "unknown"},
        }))
    }

    /// `pane list` answer naming which workspace/tab/pane holds focus.
    fn focused_pane(workspace: &str, tab: &str, pane: &str) -> String {
        envelope(serde_json::json!({
            "panes": [{
                "pane_id": pane,
                "tab_id": tab,
                "workspace_id": workspace,
                "focused": true
            }]
        }))
    }

    fn backend(script: Script) -> (HerdrBackend, Arc<Script>) {
        let script = Arc::new(script);
        let mut env = BTreeMap::new();
        env.insert("HERDR_ENV".into(), "1".into());
        env.insert("HERDR_SESSION".into(), "onlyne-test".into());
        (HerdrBackend::with_env(script.clone(), env), script)
    }

    fn create_workspace() -> String {
        envelope(serde_json::json!({
            "type": "workspace_created",
            "workspace": {"workspace_id": "wF"},
            "tab": {"tab_id": "wF:t0"},
            "root_pane": {"pane_id": "wF:p0"},
        }))
    }

    fn create_tab() -> String {
        envelope(serde_json::json!({
            "type": "tab_created",
            "tab": {
                "tab_id": "wF:t1",
                "label": "planner",
                "number": 1,
                "pane_count": 1,
                "workspace_id": "wF"
            },
            "root_pane": {"pane_id": "wF:p1"},
        }))
    }

    fn split_pane() -> String {
        envelope(serde_json::json!({
            "type": "pane_info",
            "pane": {
                "pane_id": "wF:p2",
                "tab_id": "wF:t1",
                "workspace_id": "wF",
                "agent_status": "unknown",
                "focused": false,
                "revision": 1
            }
        }))
    }

    fn agent_started() -> String {
        envelope(serde_json::json!({
            "type": "agent_started",
            "agent": {
                "name": "onlyne-planner-abcd1234",
                "agent": "pi",
                "agent_status": "idle",
                "interactive_ready": true,
                "pane_id": "wF:p2"
            },
            "argv": ["pi"]
        }))
    }

    #[test]
    fn agent_name_follows_herdr_charset_and_length() {
        assert_eq!(
            agent_name("planner", "ABCD1234-ffff-4000-8000-000000000001"),
            "onlyne-planner-abcd1234"
        );
        assert!(agent_name("planner", "abcd1234ffff").len() <= 32);
        assert!(is_agent_name(&agent_name("planner", "abcd1234ffff")));
    }

    #[test]
    fn spawn_creates_workspace_tab_and_splits() {
        let script = Script::default()
            .reply(
                "workspace list",
                0,
                envelope(serde_json::json!({"workspaces": []})),
            )
            .reply("workspace create", 0, create_workspace())
            .reply("tab list", 0, envelope(serde_json::json!({"tabs": []})))
            .reply("tab create", 0, create_tab())
            .reply("pane split", 0, split_pane())
            .reply("agent start", 0, agent_started());
        let (backend, script) = backend(script);
        let session = backend.spawn(spec(vec!["pi"])).unwrap();
        assert_eq!(session.backend, "herdr");
        assert_eq!(session.backend_ref["herdr"]["pane_id"], "wF:p2");
        assert_eq!(
            session.backend_ref["herdr"]["agent"],
            "onlyne-planner-abcd1234"
        );
        assert_eq!(
            session.backend_ref["herdr"]["workspace_label"],
            "onlyne:lab"
        );
        let calls = script.calls();
        assert!(calls.iter().any(|call| call.contains("workspace list")));
        assert!(calls.iter().any(|call| {
            call.contains("workspace create")
                && call.contains("--label")
                && call.contains("onlyne:lab")
                && call.contains("--cwd")
                && call.contains("/tmp/ws")
                && call.contains("--no-focus")
        }));
        assert!(calls.iter().any(|call| {
            call.contains("tab create")
                && call.contains("--workspace")
                && call.contains("wF")
                && call.contains("--label")
                && call.contains("planner")
                && call.contains("--no-focus")
        }));
        let split = calls
            .iter()
            .find(|call| call.contains("pane split"))
            .unwrap();
        assert!(split.contains("--pane wF:p1"));
        assert!(split.contains("--direction right"));
        assert!(split.contains("--ratio 0.5"));
        assert!(split.contains("--cwd /tmp/ws"));
        assert!(split.contains("--env ONLYNE_CLUSTER=lab"));
        assert!(split.contains("--env ONLYNE_ROLE=planner"));
        assert!(split.contains("--no-focus"));
        assert!(calls.iter().any(|call| {
            call.contains("agent start onlyne-planner-abcd1234")
                && call.contains("--kind pi")
                && call.contains("--pane wF:p2")
                && call.contains("--timeout 25000")
        }));
    }

    #[test]
    fn spawn_reuses_workspace_and_tab() {
        let script = Script::default()
            .reply(
                "workspace list",
                0,
                envelope(serde_json::json!({
                    "workspaces": [{
                        "workspace_id": "wF",
                        "label": "onlyne:lab",
                        "active_tab_id": "wF:t1",
                        "pane_count": 2,
                        "focused": true
                    }]
                })),
            )
            .reply(
                "tab list",
                0,
                envelope(serde_json::json!({
                    "tabs": [{
                        "tab_id": "wF:t1",
                        "label": "planner",
                        "pane_count": 2,
                        "workspace_id": "wF"
                    }]
                })),
            )
            .reply(
                "pane list",
                0,
                envelope(serde_json::json!({
                    "panes": [{
                        "pane_id": "wF:p1",
                        "tab_id": "wF:t1",
                        "workspace_id": "wF",
                        "focused": true
                    }]
                })),
            )
            .reply("pane split", 0, split_pane())
            .reply("agent start", 0, agent_started());
        let (backend, script) = backend(script);
        backend.spawn(spec(vec!["pi"])).unwrap();
        let calls = script.calls().join("\n");
        assert!(!calls.contains("workspace create"));
        assert!(!calls.contains("tab create"));
        assert!(calls.contains("pane split"));
    }

    #[test]
    fn spawn_falls_back_to_pane_run_for_unknown_kind() {
        let script = Script::default()
            .reply(
                "workspace list",
                0,
                envelope(serde_json::json!({"workspaces": []})),
            )
            .reply("workspace create", 0, create_workspace())
            .reply("tab list", 0, envelope(serde_json::json!({"tabs": []})))
            .reply("tab create", 0, create_tab())
            .reply("pane split", 0, split_pane())
            .reply("pane run", 0, String::new());
        let (backend, script) = backend(script);
        backend.spawn(spec(vec!["echo", "hello world"])).unwrap();
        let calls = script.calls();
        assert!(calls.iter().all(|call| !call.contains("agent start")));
        assert!(calls.iter().any(|call| {
            call.contains("pane run wF:p2") && call.contains("'echo' 'hello world'")
        }));
    }

    #[test]
    fn spawn_falls_back_to_pane_run_when_agent_start_fails() {
        let script = Script::default()
            .reply(
                "workspace list",
                0,
                envelope(serde_json::json!({"workspaces": []})),
            )
            .reply("workspace create", 0, create_workspace())
            .reply("tab list", 0, envelope(serde_json::json!({"tabs": []})))
            .reply("tab create", 0, create_tab())
            .reply("pane split", 0, split_pane())
            .reply(
                "agent start",
                1,
                r#"{"id":"cli:agent:start","error":{"message":"not ready"}}"#,
            )
            .reply("pane run", 0, String::new());
        let (backend, script) = backend(script);
        backend.spawn(spec(vec!["pi"])).unwrap();
        let joined = script.calls().join("\n");
        assert!(joined.contains("agent start"));
        assert!(joined.contains("pane run wF:p2"));
    }

    #[test]
    fn probe_maps_pane_get_and_process_info() {
        let script = Script::default()
            .reply(
                "pane get",
                0,
                envelope(serde_json::json!({
                    "type": "pane_info",
                    "pane": {
                        "pane_id": "wF:p2",
                        "agent_status": "idle",
                        "revision": 4,
                        "focused": false
                    }
                })),
            )
            .reply(
                "process-info",
                0,
                envelope(serde_json::json!({
                    "process_info": {"pane_id": "wF:p2", "shell_pid": 9}
                })),
            );
        let (backend, _) = backend(script);
        let probe = backend.probe(&session_ref("wF:p2")).unwrap();
        assert!(probe.alive);
        assert!(probe.attached);
        assert_eq!(probe.detail.unwrap()["agent_status"], "idle");
    }

    #[test]
    fn probe_missing_pane_is_dead() {
        let script = Script::default().reply("pane get", 1, r#"{"id":"cli:pane:get"}"#);
        let (backend, _) = backend(script);
        let probe = backend.probe(&session_ref("wF:p9")).unwrap();
        assert!(!probe.alive);
        assert!(!probe.attached);
    }

    #[test]
    fn close_sends_pane_close() {
        let script =
            Script::default().reply("pane close", 0, envelope(serde_json::json!({"type": "ok"})));
        let (backend, script) = backend(script);
        backend
            .close(&session_ref("wF:p2"), CloseReason::Completed, true)
            .unwrap();
        assert!(
            script
                .calls()
                .iter()
                .any(|call| call == "herdr pane close wF:p2")
        );
    }

    #[test]
    fn focus_walks_workspace_tab_agent_when_elsewhere() {
        let script = Script::default()
            .reply("pane list", 0, focused_pane("w1", "w1:t1", "w1:p1"))
            .reply(
                "workspace focus",
                0,
                envelope(serde_json::json!({"type": "workspace_info"})),
            )
            .reply(
                "tab focus",
                0,
                envelope(serde_json::json!({"type": "tab_info"})),
            )
            .reply(
                "agent focus",
                0,
                envelope(serde_json::json!({"type": "agent_info"})),
            )
            .reply("pane get", 0, pane_info("wF:p2", true));
        let (backend, script) = backend(script);
        backend.focus(&session_ref("wF:p2")).unwrap();
        assert_eq!(
            script.calls(),
            vec![
                "herdr pane list".to_string(),
                "herdr workspace focus wF".to_string(),
                "herdr tab focus wF:t1".to_string(),
                "herdr agent focus wF:p2".to_string(),
                "herdr pane get wF:p2".to_string(),
            ]
        );
    }

    #[test]
    fn focus_skips_workspace_and_tab_when_already_there() {
        let script = Script::default()
            .reply("pane list", 0, focused_pane("wF", "wF:t1", "wF:p1"))
            .reply(
                "agent focus",
                0,
                envelope(serde_json::json!({"type": "agent_info"})),
            )
            .reply("pane get", 0, pane_info("wF:p2", true));
        let (backend, script) = backend(script);
        backend.focus(&session_ref("wF:p2")).unwrap();
        assert_eq!(
            script.calls(),
            vec![
                "herdr pane list".to_string(),
                "herdr agent focus wF:p2".to_string(),
                "herdr pane get wF:p2".to_string(),
            ]
        );
    }

    #[test]
    fn focus_skips_every_hop_when_the_pane_already_holds_focus() {
        let script = Script::default().reply("pane list", 0, focused_pane("wF", "wF:t1", "wF:p2"));
        let (backend, script) = backend(script);
        backend.focus(&session_ref("wF:p2")).unwrap();
        assert_eq!(script.calls(), vec!["herdr pane list".to_string()]);
    }

    #[test]
    fn focus_walks_the_anchor_for_a_shell_pane() {
        let script = Script::default()
            .reply("pane list", 0, focused_pane("w1", "w1:t1", "w1:p1"))
            .reply(
                "workspace focus",
                0,
                envelope(serde_json::json!({"type": "workspace_info"})),
            )
            .reply(
                "tab focus",
                0,
                envelope(serde_json::json!({"type": "tab_info"})),
            )
            .reply("pane focus", 0, envelope(serde_json::json!({"type": "ok"})))
            .reply("pane get", 0, pane_info("wF:p2", true));
        let (backend, script) = backend(script);
        backend.focus(&shell_session_ref("wF:p2")).unwrap();
        assert_eq!(
            script.calls(),
            vec![
                "herdr pane list".to_string(),
                "herdr workspace focus wF".to_string(),
                "herdr tab focus wF:t1".to_string(),
                "herdr pane focus --pane wF:p1 --direction right".to_string(),
                "herdr pane get wF:p2".to_string(),
            ]
        );
    }

    #[test]
    fn focus_walks_the_anchor_in_the_recorded_direction() {
        // A `down` split reaches its pane by walking down from the anchor. The
        // direction comes from the recorded split, so a wrong value would move
        // focus to another session's pane.
        let script = Script::default()
            .reply("pane list", 0, focused_pane("wF", "wF:t1", "wF:p1"))
            .reply("pane focus", 0, envelope(serde_json::json!({"type": "ok"})))
            .reply("pane get", 0, pane_info("wF:p3", true));
        let (backend, script) = backend(script);
        let reference = session_ref_with("wF:p3", "", "wF:p1", "down");
        backend.focus(&reference).unwrap();
        assert_eq!(
            script.calls()[1],
            "herdr pane focus --pane wF:p1 --direction down".to_string()
        );
    }

    #[test]
    fn focus_falls_back_to_the_anchor_when_the_agent_is_gone() {
        let script = Script::default()
            .reply("pane list", 0, focused_pane("wF", "wF:t1", "wF:p1"))
            .reply_err(
                "agent focus",
                1,
                r#"{"error":{"code":"agent_not_found","message":"agent target wF:p2 not found"},"id":"cli:agent:focus"}"#,
            )
            .reply("pane focus", 0, envelope(serde_json::json!({"type": "ok"})))
            .reply("pane get", 0, pane_info("wF:p2", true));
        let (backend, script) = backend(script);
        backend.focus(&session_ref("wF:p2")).unwrap();
        assert_eq!(
            script.calls(),
            vec![
                "herdr pane list".to_string(),
                "herdr agent focus wF:p2".to_string(),
                "herdr pane focus --pane wF:p1 --direction right".to_string(),
                "herdr pane get wF:p2".to_string(),
            ]
        );
    }

    #[test]
    fn focus_reports_a_refused_agent_hop_without_navigation() {
        // Only `agent_not_found` opens the anchor path. Any other refusal is
        // the answer and reaches the caller unchanged.
        let script = Script::default()
            .reply("pane list", 0, focused_pane("wF", "wF:t1", "wF:p1"))
            .reply_err(
                "agent focus",
                1,
                r#"{"error":{"code":"pane_not_found","message":"pane wF:p2 not found"},"id":"cli:agent:focus"}"#,
            );
        let (backend, script) = backend(script);
        let error = backend.focus(&session_ref("wF:p2")).unwrap_err();
        assert!(error.to_string().contains("pane_not_found"), "{error}");
        assert!(
            !script
                .calls()
                .iter()
                .any(|call| call.contains("pane focus"))
        );
    }

    #[test]
    fn focus_reports_a_failure_when_another_pane_holds_focus() {
        let script = Script::default()
            .reply("pane list", 0, focused_pane("wF", "wF:t1", "wF:p1"))
            .reply("pane focus", 0, envelope(serde_json::json!({"type": "ok"})))
            .reply("pane get", 0, pane_info("wF:p2", false))
            .reply("pane list", 0, focused_pane("wF", "wF:t1", "wF:p9"));
        let (backend, _) = backend(script);
        let error = backend.focus(&shell_session_ref("wF:p2")).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("herdr left pane wF:p2 unfocused (focused pane wF:p9)"),
            "{error}"
        );
    }

    #[test]
    fn focus_reports_a_failure_without_a_recorded_anchor() {
        let script = Script::default().reply("pane list", 0, focused_pane("wF", "wF:t1", "wF:p1"));
        let (backend, script) = backend(script);
        let reference = session_ref_with("wF:p2", "", "", "");
        let error = backend.focus(&reference).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("has no recorded split anchor, so focus has no direction to walk"),
            "{error}"
        );
        assert_eq!(script.calls(), vec!["herdr pane list".to_string()]);
    }

    #[test]
    fn available_reads_injected_env() {
        let (backend, _) = backend(Script::default());
        assert!(backend.available().unwrap());
        let missing = HerdrBackend::with_env(Arc::new(Script::default()), BTreeMap::new());
        assert!(!missing.available().unwrap());
    }
}
