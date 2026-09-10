use super::*;
use std::collections::BTreeMap;
use std::sync::Arc;

pub struct OrcaBackend {
    runner: Arc<dyn Runner>,
    command: String,
}
impl OrcaBackend {
    pub fn new(runner: Arc<dyn Runner>) -> Self {
        Self {
            runner,
            command: std::env::var("ORCA_CLI_COMMAND").unwrap_or_else(|_| "orca".into()),
        }
    }
    fn json(&self, args: Vec<String>) -> Result<Value> {
        let output = run_checked(
            self.runner.as_ref(),
            &self.command,
            &args,
            None,
            &BTreeMap::new(),
        )?;
        serde_json::from_slice(&output.stdout).map_err(Into::into)
    }
    fn handle(v: &Value) -> Option<String> {
        ["/result/terminal/handle", "/terminal/handle", "/handle"]
            .iter()
            .find_map(|p| {
                v.pointer(p)
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
                    .map(str::to_owned)
            })
            .or_else(|| {
                v.get("terminal")
                    .and_then(|x| x.as_str().map(str::to_owned))
            })
    }
    fn ref_handle(session: &SessionRef) -> Result<&str> {
        session
            .backend_ref
            .get("handle")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow::anyhow!("orca session ref missing string handle"))
    }
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

/// Build the command executed inside the Orca terminal.
///
/// Orca keeps the visible terminal under the operator's worktree. The spawned
/// shell must enter the scheduler-owned workspace before starting Pi, since a
/// generated `.ws/<role>` directory is a workspace-local instance and is not
/// necessarily an Orca-registered worktree. Environment entries travel through
/// `env` so the terminal process receives the same spawn contract as zellij and
/// other backends.
fn spawn_command(spec: &SpawnSpec) -> Result<String> {
    if spec.command.is_empty() {
        anyhow::bail!("orca spawn requires a command");
    }
    let mut command = format!("cd {} &&", shell_quote(&spec.cwd.to_string_lossy()));
    if !spec.env.is_empty() {
        command.push_str(" env");
        for (key, value) in &spec.env {
            command.push(' ');
            command.push_str(&shell_quote(&format!("{key}={value}")));
        }
    }
    for arg in &spec.command {
        command.push(' ');
        command.push_str(&shell_quote(arg));
    }
    Ok(command)
}
impl SessionBackend for OrcaBackend {
    fn name(&self) -> &'static str {
        "orca"
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
        Ok(self
            .runner
            .run(
                &self.command,
                &["terminal".into(), "list".into(), "--json".into()],
                None,
                &BTreeMap::new(),
            )
            .map(|o| o.status == 0)
            .unwrap_or(false))
    }
    fn spawn(&self, spec: SpawnSpec) -> Result<SessionRef> {
        let title = spec
            .rename
            .clone()
            .unwrap_or_else(|| format!("swarm:{}", spec.task_id));
        let command = spawn_command(&spec)?;
        let mut args = vec![
            "terminal".into(),
            "create".into(),
            "--title".into(),
            title,
            "--command".into(),
            command,
        ];
        if spec.focus.unwrap_or(false) {
            args.push("--focus".into());
        }
        args.push("--json".into());
        let value = self.json(args)?;
        let handle = Self::handle(&value)
            .ok_or_else(|| anyhow::anyhow!("orca terminal create returned no handle: {value}"))?;
        Ok(SessionRef {
            task_id: spec.task_id,
            backend: self.name().into(),
            backend_ref: serde_json::json!({"handle": handle}),
            generation: 1,
        })
    }
    fn attach(&self, session: &SessionRef) -> Result<SessionRef> {
        let _ = self.json(vec![
            "terminal".into(),
            "show".into(),
            "--terminal".into(),
            Self::ref_handle(session)?.into(),
            "--json".into(),
        ])?;
        Ok(session.clone())
    }
    fn probe(&self, session: &SessionRef) -> Result<ResourceProbe> {
        let value = self.json(vec![
            "terminal".into(),
            "show".into(),
            "--terminal".into(),
            Self::ref_handle(session)?.into(),
            "--json".into(),
        ])?;
        let terminal = value.pointer("/result/terminal");
        let status = terminal
            .and_then(|v| v.get("status"))
            .and_then(Value::as_str);
        Ok(ResourceProbe {
            alive: !matches!(status, Some("exited" | "closed" | "dead")),
            attached: terminal
                .and_then(|v| v.get("connected"))
                .and_then(Value::as_bool)
                .unwrap_or(true),
            detail: Some(value),
        })
    }
    fn close(&self, session: &SessionRef, _reason: CloseReason, _force: bool) -> Result<()> {
        self.json(vec![
            "terminal".into(),
            "close".into(),
            "--terminal".into(),
            Self::ref_handle(session)?.into(),
            "--json".into(),
        ])
        .map(|_| ())
    }
    fn rename(&self, session: &SessionRef, title: &str) -> Result<()> {
        self.json(vec![
            "terminal".into(),
            "rename".into(),
            "--terminal".into(),
            Self::ref_handle(session)?.into(),
            "--title".into(),
            title.into(),
            "--json".into(),
        ])
        .map(|_| ())
    }
    fn focus(&self, session: &SessionRef) -> Result<()> {
        self.json(vec![
            "terminal".into(),
            "switch".into(),
            "--terminal".into(),
            Self::ref_handle(session)?.into(),
            "--json".into(),
        ])
        .map(|_| ())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spawn_command_enters_workspace_exports_env_and_quotes_args() {
        let mut env = BTreeMap::new();
        env.insert("ONLYNE_SWARM_TASK".into(), "task one".into());
        env.insert("QUOTED".into(), "a'b".into());
        let command = spawn_command(&SpawnSpec {
            cwd: "/tmp/work space".into(),
            task_id: "task-1".into(),
            command: vec!["pi".into(), "--model".into(), "gpt 5".into()],
            env,
            focus: None,
            rename: None,
        })
        .unwrap();
        assert_eq!(
            command,
            "cd '/tmp/work space' && env 'ONLYNE_SWARM_TASK=task one' 'QUOTED=a'\\''b' 'pi' '--model' 'gpt 5'"
        );
    }

    #[test]
    fn spawn_command_rejects_an_empty_command() {
        let error = spawn_command(&SpawnSpec {
            cwd: "/tmp/work".into(),
            task_id: "task-1".into(),
            command: vec![],
            env: BTreeMap::new(),
            focus: None,
            rename: None,
        })
        .unwrap_err();
        assert!(error.to_string().contains("requires a command"));
    }
}
