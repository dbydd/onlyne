use super::*;
use std::collections::BTreeMap;
use std::path::Path;
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
/// shell must enter the client-owned workspace before starting the agent, since a
/// generated workspace directory is a workspace-local instance and is not
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

/// Result of checking one Orca folder record against the filesystem.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FolderRecordState {
    Live,
    Pruned,
    LeftAlone,
}

/// Prune one stale Orca folder record.
///
/// Orca keeps folder-kind nodes as Orca-side metadata with no directory
/// backlink. A node whose workspace path no longer exists is a ghost
/// pointing at nothing and gets removed. Anything else stays untouched.
fn prune_folder_record(runner: &dyn Runner, command: &str, path: &Path) -> FolderRecordState {
    if path.exists() {
        return FolderRecordState::Live;
    }
    let Some(_node_id) = folder_record_id(runner, command, path) else {
        return FolderRecordState::Pruned;
    };
    match folder_record_setup_id(runner, command, path) {
        Some(setup_id) => {
            let output = runner.run(
                command,
                &[
                    "project".into(),
                    "setup-delete".into(),
                    "--setup".into(),
                    setup_id,
                    "--json".into(),
                ],
                None,
                &BTreeMap::new(),
            );
            match output {
                Ok(output) => {
                    let accepted = serde_json::from_slice::<Value>(&output.stdout)
                        .ok()
                        .and_then(|value| value.get("ok")?.as_bool())
                        == Some(true);
                    if accepted {
                        FolderRecordState::Pruned
                    } else {
                        FolderRecordState::LeftAlone
                    }
                }
                Err(_) => FolderRecordState::LeftAlone,
            }
        }
        None => FolderRecordState::LeftAlone,
    }
}

/// Look up the Orca folder node id for a workspace path.
fn folder_record_id(runner: &dyn Runner, command: &str, path: &Path) -> Option<String> {
    let canon = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let output = runner
        .run(
            command,
            &[
                "worktree".into(),
                "show".into(),
                "--worktree".into(),
                format!("path:{}", canon.display()),
                "--json".into(),
            ],
            None,
            &BTreeMap::new(),
        )
        .ok()?;
    if output.status != 0 {
        return None;
    }
    let value: Value = serde_json::from_slice(&output.stdout).ok()?;
    if value.get("ok")?.as_bool() != Some(true) {
        return None;
    }
    value
        .pointer("/result/worktree/id")
        .and_then(Value::as_str)
        .map(str::to_owned)
}

/// Look up the Orca project setup id whose path matches a workspace path.
fn folder_record_setup_id(runner: &dyn Runner, command: &str, path: &Path) -> Option<String> {
    let canon = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let output = runner
        .run(
            command,
            &["project".into(), "setups".into(), "--json".into()],
            None,
            &BTreeMap::new(),
        )
        .ok()?;
    if output.status != 0 {
        return None;
    }
    let value: Value = serde_json::from_slice(&output.stdout).ok()?;
    let setups = value.pointer("/result/setups")?.as_array()?;
    for setup in setups {
        let candidate = setup.get("path")?.as_str()?;
        let canon_candidate =
            std::fs::canonicalize(candidate).unwrap_or_else(|_| Path::new(candidate).to_path_buf());
        if canon_candidate == canon {
            return setup.get("id")?.as_str().map(str::to_owned);
        }
    }
    None
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
        let _ = prune_folder_record(self.runner.as_ref(), &self.command, &spec.cwd);
        let title = spec
            .rename
            .clone()
            .unwrap_or_else(|| format!("onlyne:{}", spec.task_id));
        let shell = spawn_command(&spec)?;
        let mut args = vec![
            "terminal".into(),
            "create".into(),
            "--title".into(),
            title,
            "--command".into(),
            shell,
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
        env.insert("ONLYNE_TASK".into(), "task one".into());
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
            "cd '/tmp/work space' && env 'ONLYNE_TASK=task one' 'QUOTED=a'\\''b' 'pi' '--model' 'gpt 5'"
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

    #[test]
    fn missing_workspace_prunes_the_folder_record() {
        use std::sync::Mutex;
        struct ScriptRunner {
            calls: Mutex<Vec<Vec<String>>>,
        }
        impl Runner for ScriptRunner {
            fn run(
                &self,
                _program: &str,
                args: &[String],
                _cwd: Option<&Path>,
                _env: &BTreeMap<String, String>,
            ) -> Result<CommandOutput> {
                self.calls.lock().unwrap().push(args.to_vec());
                let text = args
                    .iter()
                    .map(String::as_str)
                    .collect::<Vec<_>>()
                    .join(" ");
                let body = if text.contains("setup-delete") {
                    serde_json::json!({"ok": true}).to_string()
                } else if text.contains("setups") {
                    serde_json::json!({
                        "ok": true,
                        "result": {
                            "setups": [
                                {"id": "setup-ghost", "path": "/tmp/onlyne-ghost-missing"}
                            ]
                        }
                    })
                    .to_string()
                } else {
                    serde_json::json!({
                        "ok": true,
                        "result": {"worktree": {"id": "node-ghost"}}
                    })
                    .to_string()
                };
                Ok(CommandOutput {
                    status: 0,
                    stdout: body.into_bytes(),
                    stderr: Vec::new(),
                })
            }
        }
        let runner = ScriptRunner {
            calls: Mutex::new(Vec::new()),
        };
        let missing = Path::new("/tmp/onlyne-ghost-missing");
        assert!(!missing.exists());
        let state = prune_folder_record(&runner, "orca", missing);
        assert_eq!(state, FolderRecordState::Pruned);
        let calls = runner.calls.lock().unwrap();
        assert!(
            calls
                .iter()
                .any(|args| args.contains(&"setup-delete".to_string()))
        );
    }

    #[test]
    fn existing_workspace_leaves_the_folder_record_alone() {
        use std::sync::Mutex;
        struct SilentRunner {
            calls: Mutex<usize>,
        }
        impl Runner for SilentRunner {
            fn run(
                &self,
                _program: &str,
                _args: &[String],
                _cwd: Option<&Path>,
                _env: &BTreeMap<String, String>,
            ) -> Result<CommandOutput> {
                *self.calls.lock().unwrap() += 1;
                Ok(CommandOutput {
                    status: 0,
                    stdout: b"{}".to_vec(),
                    stderr: Vec::new(),
                })
            }
        }
        let dir = tempfile::tempdir().unwrap();
        let runner = SilentRunner {
            calls: Mutex::new(0),
        };
        let state = prune_folder_record(&runner, "orca", dir.path());
        assert_eq!(state, FolderRecordState::Live);
        assert_eq!(*runner.calls.lock().unwrap(), 0);
    }
}
