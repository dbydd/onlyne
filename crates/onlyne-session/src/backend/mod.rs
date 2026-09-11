use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

pub mod exec;
pub mod fake;
pub mod orca;
pub mod zellij;

pub use orca::WorktreePolicy;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Capabilities {
    pub spawn: bool,
    pub attach: bool,
    pub probe: bool,
    pub close: bool,
    pub focus: bool,
    pub rename: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpawnSpec {
    pub cwd: PathBuf,
    pub task_id: String,
    pub command: Vec<String>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    #[serde(default)]
    pub focus: Option<bool>,
    #[serde(default)]
    pub rename: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionRef {
    pub task_id: String,
    pub backend: String,
    pub backend_ref: Value,
    pub generation: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ResourceProbe {
    pub alive: bool,
    pub attached: bool,
    pub detail: Option<Value>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CloseReason {
    Completed,
    Cancelled,
    Fault,
    Shutdown,
    Replaced,
    Operator,
}

pub trait SessionBackend: Send + Sync {
    fn name(&self) -> &'static str;
    fn capabilities(&self) -> Capabilities;
    fn available(&self) -> Result<bool>;
    fn spawn(&self, spec: SpawnSpec) -> Result<SessionRef>;
    fn attach(&self, session: &SessionRef) -> Result<SessionRef>;
    fn probe(&self, session: &SessionRef) -> Result<ResourceProbe>;
    fn close(&self, session: &SessionRef, reason: CloseReason, force: bool) -> Result<()>;
    fn rename(&self, _session: &SessionRef, _title: &str) -> Result<()> {
        Err(unsupported(
            self.name(),
            "rename",
            "backend does not expose rename",
        ))
    }
    fn focus(&self, _session: &SessionRef) -> Result<()> {
        Err(unsupported(
            self.name(),
            "focus",
            "backend does not expose focus",
        ))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandOutput {
    pub status: i32,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

pub trait Runner: Send + Sync {
    fn run(
        &self,
        program: &str,
        args: &[String],
        cwd: Option<&Path>,
        env: &BTreeMap<String, String>,
    ) -> Result<CommandOutput>;
}

#[derive(Debug, Default, Clone, Copy)]
pub struct ProcessRunner;

impl Runner for ProcessRunner {
    fn run(
        &self,
        program: &str,
        args: &[String],
        cwd: Option<&Path>,
        env: &BTreeMap<String, String>,
    ) -> Result<CommandOutput> {
        let mut command = std::process::Command::new(program);
        command.args(args);
        if let Some(cwd) = cwd {
            command.current_dir(cwd);
        }
        command.envs(env);
        let output = command.output()?;
        Ok(CommandOutput {
            status: output.status.code().unwrap_or(-1),
            stdout: output.stdout,
            stderr: output.stderr,
        })
    }
}

pub(crate) fn command_error(program: &str, output: &CommandOutput) -> anyhow::Error {
    anyhow::anyhow!(
        "runtime command failed: {program} (status {}): {}",
        output.status,
        String::from_utf8_lossy(&output.stderr).trim()
    )
}

pub(crate) fn run_checked(
    runner: &dyn Runner,
    program: &str,
    args: &[String],
    cwd: Option<&Path>,
    env: &BTreeMap<String, String>,
) -> Result<CommandOutput> {
    let output = runner.run(program, args, cwd, env)?;
    if output.status != 0 {
        return Err(command_error(program, &output));
    }
    Ok(output)
}

/// A backend command failure decoded from the CLI's JSON error body.
///
/// Orca exits 1 with an empty stderr and `{"ok":false,"error":{"code":…}}` on
/// stdout, so an exit-code-only message names neither the cause nor the
/// command. [`run_json`] keeps both, and `code` is the machine-readable value
/// backends branch on (`terminal_handle_stale`, `selector_not_found`, …).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandFailure {
    command: String,
    status: i32,
    code: Option<String>,
    message: Option<String>,
}

impl CommandFailure {
    /// Build a failure for a command the backend describes itself.
    pub(crate) fn new(
        command: impl Into<String>,
        status: i32,
        code: Option<String>,
        message: Option<String>,
    ) -> Self {
        Self {
            command: command.into(),
            status,
            code,
            message,
        }
    }

    /// The CLI's machine-readable error code, when the body carried one.
    pub fn code(&self) -> Option<&str> {
        self.code.as_deref()
    }
}

impl std::fmt::Display for CommandFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "runtime command failed: {} (status {}",
            self.command, self.status
        )?;
        if let Some(code) = &self.code {
            write!(f, ", {code}")?;
        }
        write!(f, ")")?;
        if let Some(message) = &self.message {
            write!(f, ": {message}")?;
        }
        Ok(())
    }
}

impl std::error::Error for CommandFailure {}

/// Run one backend command whose protocol is JSON on stdout, and return its
/// `result` object — or the whole document when it has no `result`.
///
/// Failure detail comes from the body first and the stderr second, so a
/// backend that reports errors inside the payload still produces a message
/// that names the cause. Callers branch on the code through
/// [`CommandFailure::code`] after a `downcast_ref`.
pub(crate) fn run_json(
    runner: &dyn Runner,
    program: &str,
    args: &[String],
    cwd: Option<&Path>,
    env: &BTreeMap<String, String>,
) -> Result<Value> {
    let output = runner.run(program, args, cwd, env)?;
    let body = serde_json::from_slice::<Value>(&output.stdout).ok();
    let command = format!("{program} {}", args.join(" "));
    if output.status == 0 {
        let Some(value) = body else {
            return Err(command_failure(&command, &output, None).into());
        };
        if value.get("ok").and_then(Value::as_bool) == Some(false) {
            return Err(command_failure(&command, &output, Some(&value)).into());
        }
        return Ok(match value.get("result") {
            Some(result) => result.clone(),
            None => value,
        });
    }
    Err(command_failure(&command, &output, body.as_ref()).into())
}

/// The structured failure for one refused (or body-less) CLI answer.
fn command_failure(command: &str, output: &CommandOutput, body: Option<&Value>) -> CommandFailure {
    let error = body.and_then(|body| body.get("error"));
    let text = |key: &str| {
        error
            .and_then(|error| error.get(key))
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .map(str::to_owned)
    };
    let message = text("message").or_else(|| {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stderr = stderr.trim();
        if stderr.is_empty() {
            body.map(Value::to_string)
        } else {
            Some(stderr.to_string())
        }
    });
    CommandFailure::new(command, output.status, text("code"), message)
}

/// The JSON error code of a backend CLI failure, when it carried one.
pub(crate) fn failure_code(error: &anyhow::Error) -> Option<&str> {
    error
        .downcast_ref::<CommandFailure>()
        .and_then(CommandFailure::code)
}

pub(crate) fn unsupported(backend: &str, operation: &str, detail: &str) -> anyhow::Error {
    anyhow::anyhow!("runtime backend {backend} does not support {operation}: {detail}")
}

/// Probe order: zellij, then orca, then fake as the last resort. `fake` is
/// always available, so `auto` resolves on any machine; explicit names still
/// select one backend only. Reached through `ONLYNE_BACKEND=auto`, or directly
/// by callers that want capability discovery.
pub fn select_backend(
    runner: Arc<dyn Runner>,
    policy: WorktreePolicy,
) -> Result<Box<dyn SessionBackend>> {
    let backends: [Box<dyn SessionBackend>; 3] = [
        Box::new(zellij::ZellijBackend::new(runner.clone())),
        Box::new(orca::OrcaBackend::with_policy(runner, policy)),
        Box::new(fake::FakeBackend::new()),
    ];
    for backend in backends {
        if backend.available()? && backend.capabilities().spawn && backend.capabilities().probe {
            return Ok(backend);
        }
    }
    Err(anyhow::anyhow!(
        "no usable session backend available (tried zellij, orca, fake)"
    ))
}

pub fn backend_by_name(
    name: &str,
    runner: Arc<dyn Runner>,
    policy: WorktreePolicy,
) -> Result<Box<dyn SessionBackend>> {
    match name {
        "orca" => Ok(Box::new(orca::OrcaBackend::with_policy(runner, policy))),
        "zellij" => Ok(Box::new(zellij::ZellijBackend::new(runner))),
        "fake" => Ok(Box::new(fake::FakeBackend::new())),
        "exec" => Ok(Box::new(exec::ExecBackend::new())),
        other => Err(anyhow::anyhow!("unknown session backend: {other}")),
    }
}

/// Resolve a backend name to a concrete backend. `auto` probes capability.
/// Empty or unknown names fall back to `"zellij"` with the reason logged.
pub fn backend_for(
    requested: &str,
    runner: Arc<dyn Runner>,
    policy: WorktreePolicy,
) -> Result<Box<dyn SessionBackend>> {
    let name = requested.trim();
    if name.eq_ignore_ascii_case("auto") {
        return select_backend(runner, policy);
    }
    let name = if name.is_empty() { "zellij" } else { name };
    match backend_by_name(name, runner.clone(), policy.clone()) {
        Ok(b) => Ok(b),
        Err(e) => {
            tracing::warn!("ONLYNE_BACKEND={requested} unusable ({e}); falling back to zellij");
            backend_by_name("zellij", runner, policy)
        }
    }
}

/// Client default backend: deterministic, driven by `ONLYNE_BACKEND`
/// (`auto` | `zellij` | `orca` | `fake` | `exec`), defaulting to `zellij`.
/// Capability discovery is available through the explicit `auto` value, so an
/// installed backend only joins selection when the operator asks for it.
///
/// `exec` is never part of `auto`: it spawns the session command as a child of
/// this process with no terminal around it, which is a deliberate choice a case
/// or a headless host makes (see `backend::exec`), not a fallback to discover.
///
/// `worktree` is the workspace config's `[orca] worktree` policy; only the
/// Orca backend reads it, the other three ignore it.
pub fn default_backend(worktree: WorktreePolicy) -> Result<Box<dyn SessionBackend>> {
    backend_for(
        &std::env::var("ONLYNE_BACKEND").unwrap_or_default(),
        Arc::new(ProcessRunner),
        worktree,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use parking_lot::Mutex;
    use std::collections::VecDeque;

    /// Answers one scripted `(status, stdout)` per call; a call with no answer
    /// fails the way an absent CLI does.
    #[derive(Default)]
    struct ProbeRunner {
        calls: Mutex<Vec<(String, Vec<String>)>>,
        script: Mutex<VecDeque<(i32, String)>>,
    }

    impl ProbeRunner {
        fn reply(self, status: i32, body: &str) -> Self {
            self.script.lock().push_back((status, body.to_string()));
            self
        }

        fn calls(&self) -> Vec<(String, Vec<String>)> {
            self.calls.lock().clone()
        }
    }

    impl Runner for ProbeRunner {
        fn run(
            &self,
            program: &str,
            args: &[String],
            _: Option<&Path>,
            _: &BTreeMap<String, String>,
        ) -> Result<CommandOutput> {
            self.calls.lock().push((program.to_owned(), args.to_vec()));
            let (status, stdout) = self
                .script
                .lock()
                .pop_front()
                .unwrap_or_else(|| (1, String::new()));
            Ok(CommandOutput {
                status,
                stdout: stdout.into_bytes(),
                stderr: Vec::new(),
            })
        }
    }

    #[test]
    fn session_ref_keeps_opaque_json() {
        let value = SessionRef {
            task_id: "t".into(),
            backend: "fake".into(),
            backend_ref: serde_json::json!({"x": [1, 2]}),
            generation: 3,
        };
        let decoded: SessionRef =
            serde_json::from_value(serde_json::to_value(value.clone()).unwrap()).unwrap();
        assert_eq!(decoded, value);
    }

    #[test]
    fn selection_falls_back_to_fake_when_nothing_else_answers() {
        let runner = Arc::new(ProbeRunner::default());
        let backend = select_backend(runner.clone(), WorktreePolicy::Host).unwrap();
        assert_eq!(backend.name(), "fake");
        assert_eq!(runner.calls()[0].0, "zellij");
    }

    #[test]
    fn auto_selection_uses_the_documented_priority() {
        #[derive(Default)]
        struct AutoRunner {
            calls: Mutex<Vec<String>>,
        }
        impl Runner for AutoRunner {
            fn run(
                &self,
                program: &str,
                _: &[String],
                _: Option<&Path>,
                _: &BTreeMap<String, String>,
            ) -> Result<CommandOutput> {
                self.calls.lock().push(program.to_owned());
                let zellij = program == "zellij";
                Ok(CommandOutput {
                    status: if zellij { 0 } else { 1 },
                    stdout: if zellij {
                        b"session\n".to_vec()
                    } else {
                        vec![]
                    },
                    stderr: b"missing".to_vec(),
                })
            }
        }
        let runner = Arc::new(AutoRunner::default());
        let backend = backend_for("AUTO", runner.clone(), WorktreePolicy::Host).unwrap();
        assert_eq!(backend.name(), "zellij");
        assert_eq!(runner.calls.lock().as_slice(), &["zellij".to_string()]);
    }

    #[test]
    fn scheduler_default_uses_zellij_unless_auto_is_requested() {
        let runner = Arc::new(ProbeRunner::default());
        assert_eq!(
            backend_for("", runner.clone(), WorktreePolicy::Host)
                .unwrap()
                .name(),
            "zellij"
        );
        assert_eq!(
            backend_for("zellij", runner.clone(), WorktreePolicy::Host)
                .unwrap()
                .name(),
            "zellij"
        );
        assert_eq!(
            backend_for("fake", runner.clone(), WorktreePolicy::Host)
                .unwrap()
                .name(),
            "fake"
        );
        // `exec` is opt-in only: naming it selects it, `auto` never reaches it.
        assert_eq!(
            backend_for("exec", runner.clone(), WorktreePolicy::Host)
                .unwrap()
                .name(),
            "exec"
        );
        assert_eq!(
            backend_for("nope", runner.clone(), WorktreePolicy::Host)
                .unwrap()
                .name(),
            "zellij"
        );
        // Empty and named modes resolve deterministically without probing.
        assert!(runner.calls().is_empty());
    }

    /// Orca is the only backend that reads the policy, and the JSON error body
    /// is where its CLI puts the reason for a refusal.
    #[test]
    fn run_json_unwraps_result_and_names_a_refusal() {
        let cli = Arc::new(
            ProbeRunner::default()
                .reply(0, r#"{"ok":true,"result":{"terminal":{"handle":"term_1"}}}"#)
                .reply(
                    1,
                    r#"{"ok":false,"error":{"code":"terminal_handle_stale","message":"handle is stale"}}"#,
                )
                .reply(1, r#"{"ok":false,"error":{"code":"selector_not_found"}}"#),
        );
        let args = ["terminal".to_string(), "show".to_string()];
        let value = run_json(cli.as_ref(), "orca", &args, None, &BTreeMap::new()).unwrap();
        assert_eq!(value.pointer("/terminal/handle").unwrap(), "term_1");

        let error = run_json(cli.as_ref(), "orca", &args, None, &BTreeMap::new()).unwrap_err();
        let failure = error.downcast_ref::<CommandFailure>().unwrap();
        assert_eq!(failure.code(), Some("terminal_handle_stale"));
        assert_eq!(
            error.to_string(),
            "runtime command failed: orca terminal show (status 1, terminal_handle_stale): handle is stale"
        );

        // A body without a message still names the code it failed on.
        let error = run_json(cli.as_ref(), "orca", &args, None, &BTreeMap::new()).unwrap_err();
        assert_eq!(
            error.downcast_ref::<CommandFailure>().unwrap().code(),
            Some("selector_not_found")
        );
    }
}
