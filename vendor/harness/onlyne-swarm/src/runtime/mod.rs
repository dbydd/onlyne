use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

pub mod fake;
pub mod herdr;
pub mod orca;
pub mod zellij;

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

pub(crate) fn unsupported(backend: &str, operation: &str, detail: &str) -> anyhow::Error {
    anyhow::anyhow!("runtime backend {backend} does not support {operation}: {detail}")
}

/// Auto-probe: pick the first backend that reports available and can spawn +
/// probe. Reached through `SWARM_RUNTIME=auto`, or directly by callers such as
/// `swarm doctor` that want capability discovery.
pub fn select_backend(runner: Arc<dyn Runner>) -> Result<Box<dyn SessionBackend>> {
    let backends: [Box<dyn SessionBackend>; 3] = [
        Box::new(herdr::HerdrBackend::new(runner.clone())),
        Box::new(zellij::ZellijBackend::new(runner.clone())),
        Box::new(orca::OrcaBackend::new(runner)),
    ];
    for backend in backends {
        if backend.available()? && backend.capabilities().spawn && backend.capabilities().probe {
            return Ok(backend);
        }
    }
    Err(anyhow::anyhow!(
        "no usable session backend available (tried herdr, zellij, orca)"
    ))
}

/// Construct a backend by name (`orca` | `zellij` | `herdr` | `fake`).
pub fn backend_by_name(name: &str, runner: Arc<dyn Runner>) -> Result<Box<dyn SessionBackend>> {
    match name {
        "orca" => Ok(Box::new(orca::OrcaBackend::new(runner))),
        "zellij" => Ok(Box::new(zellij::ZellijBackend::new(runner))),
        "herdr" => Ok(Box::new(herdr::HerdrBackend::new(runner))),
        "fake" => Ok(Box::new(fake::FakeBackend::new())),
        other => Err(anyhow::anyhow!("unknown session backend: {other}")),
    }
}

/// Resolve a backend name to a concrete backend. `auto` probes capability.
/// Empty or unknown names fall back to `"orca"` (the historical default) with
/// the reason logged.
pub fn backend_for(requested: &str, runner: Arc<dyn Runner>) -> Result<Box<dyn SessionBackend>> {
    let name = requested.trim();
    if name.eq_ignore_ascii_case("auto") {
        return select_backend(runner);
    }
    let name = if name.is_empty() { "orca" } else { name };
    match backend_by_name(name, runner.clone()) {
        Ok(b) => Ok(b),
        Err(e) => {
            tracing::warn!("SWARM_RUNTIME={requested} unusable ({e}); falling back to orca");
            backend_by_name("orca", runner)
        }
    }
}

/// Scheduler default backend: deterministic, driven by `SWARM_RUNTIME`
/// (`auto` | `orca` | `zellij` | `herdr` | `fake`), defaulting to `orca`.
/// Orca is the historical single backend and its liveness contract treats a
/// failed probe as unknown-alive; an env-var default keeps `ORCA_CLI_COMMAND`
/// test fixtures and operator setups reproducible on machines where zellij or
/// herdr happen to also be installed. Capability discovery is available through
/// the explicit `auto` value, so an installed backend only joins selection when
/// the operator asks for it.
pub fn default_backend() -> Result<Box<dyn SessionBackend>> {
    backend_for(
        &std::env::var("SWARM_RUNTIME").unwrap_or_default(),
        Arc::new(ProcessRunner),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[derive(Default)]
    struct ProbeRunner(Mutex<Vec<(String, Vec<String>)>>);
    impl Runner for ProbeRunner {
        fn run(
            &self,
            program: &str,
            args: &[String],
            _: Option<&Path>,
            _: &BTreeMap<String, String>,
        ) -> Result<CommandOutput> {
            self.0
                .lock()
                .unwrap()
                .push((program.to_owned(), args.to_vec()));
            Ok(CommandOutput {
                status: 1,
                stdout: vec![],
                stderr: b"missing".to_vec(),
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
    fn selection_has_fixed_priority_and_fails_explicitly() {
        let runner = Arc::new(ProbeRunner::default());
        let error = match select_backend(runner.clone()) {
            Ok(_) => panic!("selection unexpectedly succeeded"),
            Err(error) => error,
        };
        assert!(error.to_string().contains("no usable session backend"));
        assert_eq!(runner.0.lock().unwrap()[0].0, "herdr");
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
                self.calls.lock().unwrap().push(program.to_owned());
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
        let backend = backend_for("AUTO", runner.clone()).unwrap();
        assert_eq!(backend.name(), "zellij");
        assert_eq!(
            runner.calls.lock().unwrap().as_slice(),
            &["herdr".to_string(), "zellij".to_string()]
        );
    }

    #[test]
    fn scheduler_default_uses_orca_unless_auto_is_requested() {
        let runner = Arc::new(ProbeRunner::default());
        assert_eq!(backend_for("", runner.clone()).unwrap().name(), "orca");
        assert_eq!(backend_for("orca", runner.clone()).unwrap().name(), "orca");
        assert_eq!(backend_for("fake", runner.clone()).unwrap().name(), "fake");
        assert_eq!(backend_for("nope", runner.clone()).unwrap().name(), "orca");
        // Empty and named modes resolve deterministically without probing.
        assert!(runner.0.lock().unwrap().is_empty());
    }
}
