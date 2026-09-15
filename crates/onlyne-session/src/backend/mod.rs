use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

pub mod exec;
pub mod fake;
pub mod herdr;
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

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SpawnSpec {
    pub cwd: PathBuf,
    pub task_id: String,
    pub command: Vec<String>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    #[serde(default)]
    pub focus: Option<bool>,
    #[serde(default)]
    pub placement: Option<PanePlacement>,
    #[serde(default)]
    pub rename: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SplitDirection {
    Right,
    Down,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct PanePlacement {
    pub direction: SplitDirection,
    pub ratio: f64,
}

impl SplitDirection {
    pub fn as_herdr(self) -> &'static str {
        match self {
            Self::Right => "right",
            Self::Down => "down",
        }
    }
}

impl PanePlacement {
    /// A split that brings the pane count to a power of two goes right.
    /// Every other split goes down. Ratio is always 0.5.
    pub fn from_pane_count(pane_count: usize) -> Self {
        let direction = if (pane_count + 1).is_power_of_two() {
            SplitDirection::Right
        } else {
            SplitDirection::Down
        };
        Self {
            direction,
            ratio: 0.5,
        }
    }
}

/// Stderr line and [`NoSupportedHost`] display when no host is selected.
pub const NO_SUPPORTED_HOST: &str =
    "onlyne: no supported host detected; run inside herdr, orca, or zellij, or set ONLYNE_BACKEND";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackendName {
    Herdr,
    Orca,
    Zellij,
    Exec,
    Fake,
}

impl BackendName {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Herdr => "herdr",
            Self::Orca => "orca",
            Self::Zellij => "zellij",
            Self::Exec => "exec",
            Self::Fake => "fake",
        }
    }

    pub fn parse(name: &str) -> Option<Self> {
        match name.trim().to_ascii_lowercase().as_str() {
            "herdr" => Some(Self::Herdr),
            "orca" => Some(Self::Orca),
            "zellij" => Some(Self::Zellij),
            // `headless` is the operator-facing alias; projections keep `exec`.
            "exec" | "headless" => Some(Self::Exec),
            "fake" => Some(Self::Fake),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelectionSource {
    Explicit,
    Env,
    None,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostDetection {
    pub backend: Option<BackendName>,
    pub source: SelectionSource,
    pub explicit: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NoSupportedHost;

impl std::fmt::Display for NoSupportedHost {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(NO_SUPPORTED_HOST)
    }
}

impl std::error::Error for NoSupportedHost {}

pub fn process_env() -> BTreeMap<String, String> {
    std::env::vars().collect()
}

fn env_nonempty(env: &BTreeMap<String, String>, key: &str) -> bool {
    env.get(key).is_some_and(|value| !value.is_empty())
}

pub(crate) fn herdr_host_present(env: &BTreeMap<String, String>) -> bool {
    env.get("HERDR_ENV").is_some_and(|value| value == "1")
        && (env_nonempty(env, "HERDR_SOCKET_PATH")
            || env_nonempty(env, "HERDR_SESSION")
            || env_nonempty(env, "HERDR_WORKSPACE_ID"))
}

fn orca_host_present(env: &BTreeMap<String, String>) -> bool {
    env_nonempty(env, "ORCA_PANE_KEY")
        || env_nonempty(env, "ORCA_TERMINAL_HANDLE")
        || env_nonempty(env, "ORCA_WORKTREE_ID")
}

fn zellij_host_present(env: &BTreeMap<String, String>) -> bool {
    env.contains_key("ZELLIJ")
}

/// Map process environment to a backend choice.
pub fn detect_host(env: &BTreeMap<String, String>) -> HostDetection {
    let explicit = env
        .get("ONLYNE_BACKEND")
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());
    if let Some(name) = &explicit {
        if !name.eq_ignore_ascii_case("auto") {
            return HostDetection {
                backend: BackendName::parse(name),
                source: SelectionSource::Explicit,
                explicit: Some(name.clone()),
            };
        }
    }
    let backend = if herdr_host_present(env) {
        Some(BackendName::Herdr)
    } else if orca_host_present(env) {
        Some(BackendName::Orca)
    } else if zellij_host_present(env) {
        Some(BackendName::Zellij)
    } else {
        None
    };
    HostDetection {
        source: if backend.is_some() {
            SelectionSource::Env
        } else {
            SelectionSource::None
        },
        backend,
        explicit,
    }
}

pub fn doctor_report(env: &BTreeMap<String, String>) -> Value {
    let detected = detect_host(env);
    let host = detected.backend.map(|name| name.as_str());
    let backend_selection = match detected.source {
        SelectionSource::Explicit => "explicit",
        SelectionSource::Env => "env",
        SelectionSource::None => "none",
    };
    let binary = match detected.backend {
        Some(BackendName::Herdr) => Some(
            env.get("HERDR_BIN_PATH")
                .filter(|value| !value.is_empty())
                .cloned()
                .unwrap_or_else(|| "herdr".into()),
        ),
        Some(BackendName::Orca) => Some(
            env.get("ORCA_CLI_COMMAND")
                .filter(|value| !value.is_empty())
                .cloned()
                .unwrap_or_else(|| "orca".into()),
        ),
        Some(BackendName::Zellij) => Some(
            env.get("ZELLIJ_COMMAND")
                .filter(|value| !value.is_empty())
                .cloned()
                .unwrap_or_else(|| "zellij".into()),
        ),
        Some(BackendName::Exec) | Some(BackendName::Fake) => None,
        None => None,
    };
    let mut report = serde_json::json!({
        "host": host,
        "binary": binary,
        "session": env.get("HERDR_SESSION").filter(|value| !value.is_empty()),
        "workspace_id": env.get("HERDR_WORKSPACE_ID").filter(|value| !value.is_empty()),
        "tab_id": env.get("HERDR_TAB_ID").filter(|value| !value.is_empty()),
        "pane_id": env.get("HERDR_PANE_ID").filter(|value| !value.is_empty()),
        "backend_selection": backend_selection,
        "explicit": detected.explicit,
    });
    if detected.backend.is_none() {
        report["refusal"] = Value::String(NO_SUPPORTED_HOST.into());
    }
    report
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
///
/// Herdr writes its error document to stderr with an empty stdout, Orca writes
/// `{"ok":false,…}` to stdout. Both shapes are decoded so
/// [`CommandFailure::code`] carries the machine-readable code either way.
fn command_failure(command: &str, output: &CommandOutput, body: Option<&Value>) -> CommandFailure {
    let stderr_body = || -> Option<Value> {
        let text = String::from_utf8_lossy(&output.stderr);
        let text = text.trim();
        if text.is_empty() {
            return None;
        }
        serde_json::from_str::<Value>(text).ok()
    };
    let fallback = stderr_body();
    let error = body
        .and_then(|body| body.get("error"))
        .or_else(|| fallback.as_ref().and_then(|value| value.get("error")));
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

/// Build a backend from an environment map. `ONLYNE_BACKEND` wins when it
/// names `herdr`, `orca`, `zellij`, `exec` (alias `headless`), or `fake`. An
/// empty or `auto` value probes herdr, then orca, then zellij. `exec` and
/// `fake` are never discovered. No match returns [`NoSupportedHost`].
pub fn select_backend_from_env(
    env: &BTreeMap<String, String>,
    runner: Arc<dyn Runner>,
    policy: WorktreePolicy,
) -> Result<Box<dyn SessionBackend>> {
    let detected = detect_host(env);
    match detected.backend {
        Some(name) => backend_by_name(name.as_str(), runner, policy),
        None if detected
            .explicit
            .as_deref()
            .is_some_and(|name| !name.eq_ignore_ascii_case("auto")) =>
        {
            Err(anyhow::anyhow!(
                "unknown session backend: {}",
                detected.explicit.unwrap_or_default()
            ))
        }
        None => Err(NoSupportedHost.into()),
    }
}

/// Probe the process environment. Live `HERDR_*` / `ORCA_*` / `ZELLIJ` values
/// on the developer machine affect this path; tests use
/// [`select_backend_from_env`].
pub fn select_backend(
    runner: Arc<dyn Runner>,
    policy: WorktreePolicy,
) -> Result<Box<dyn SessionBackend>> {
    let mut env = process_env();
    env.remove("ONLYNE_BACKEND");
    select_backend_from_env(&env, runner, policy)
}

pub fn backend_by_name(
    name: &str,
    runner: Arc<dyn Runner>,
    policy: WorktreePolicy,
) -> Result<Box<dyn SessionBackend>> {
    match BackendName::parse(name) {
        Some(BackendName::Herdr) => Ok(Box::new(herdr::HerdrBackend::new(runner))),
        Some(BackendName::Orca) => Ok(Box::new(orca::OrcaBackend::with_policy(runner, policy))),
        Some(BackendName::Zellij) => Ok(Box::new(zellij::ZellijBackend::new(runner))),
        Some(BackendName::Fake) => Ok(Box::new(fake::FakeBackend::new())),
        Some(BackendName::Exec) => Ok(Box::new(exec::ExecBackend::new())),
        None => Err(anyhow::anyhow!("unknown session backend: {name}")),
    }
}

/// Resolve a backend name to a concrete backend. `auto` and empty probe the
/// supplied runner's process environment through [`select_backend`]. Named
/// values stay exact: unknown names error with the existing spelling.
pub fn backend_for(
    requested: &str,
    runner: Arc<dyn Runner>,
    policy: WorktreePolicy,
) -> Result<Box<dyn SessionBackend>> {
    backend_for_env(requested, &process_env(), runner, policy)
}

pub fn backend_for_env(
    requested: &str,
    env: &BTreeMap<String, String>,
    runner: Arc<dyn Runner>,
    policy: WorktreePolicy,
) -> Result<Box<dyn SessionBackend>> {
    let name = requested.trim();
    if name.eq_ignore_ascii_case("auto") || name.is_empty() {
        let mut probe = env.clone();
        probe.remove("ONLYNE_BACKEND");
        return select_backend_from_env(&probe, runner, policy);
    }
    backend_by_name(name, runner, policy)
}

/// Client default backend: driven by `ONLYNE_BACKEND`
/// (`herdr` | `orca` | `zellij` | `exec`/`headless` | `fake` | `auto`). An
/// empty value probes herdr, then orca, then zellij. `exec` and `fake` stay
/// opt-in. `headless` selects [`BackendName::Exec`]; [`BackendName::as_str`]
/// still answers `exec`.
///
/// `worktree` is the workspace config's `[orca] worktree` policy; only the
/// Orca backend reads it.
pub fn default_backend(worktree: WorktreePolicy) -> Result<Box<dyn SessionBackend>> {
    let env = process_env();
    backend_for_env(
        env.get("ONLYNE_BACKEND").map(String::as_str).unwrap_or(""),
        &env,
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

    fn env(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
            .collect()
    }

    #[test]
    fn detect_host_table_covers_explicit_env_and_none() {
        let cases = [
            (
                env(&[("ONLYNE_BACKEND", "herdr")]),
                Some(BackendName::Herdr),
                SelectionSource::Explicit,
            ),
            (
                env(&[("ONLYNE_BACKEND", "fake")]),
                Some(BackendName::Fake),
                SelectionSource::Explicit,
            ),
            (
                env(&[
                    ("HERDR_ENV", "1"),
                    ("HERDR_SESSION", "onlyne-test"),
                    ("ORCA_PANE_KEY", "tab:leaf"),
                ]),
                Some(BackendName::Herdr),
                SelectionSource::Env,
            ),
            (
                env(&[("ORCA_WORKTREE_ID", "wt-1")]),
                Some(BackendName::Orca),
                SelectionSource::Env,
            ),
            (
                env(&[("ZELLIJ", "0")]),
                Some(BackendName::Zellij),
                SelectionSource::Env,
            ),
            (env(&[]), None, SelectionSource::None),
            (env(&[("HERDR_ENV", "1")]), None, SelectionSource::None),
            (
                env(&[("ONLYNE_BACKEND", "auto"), ("ZELLIJ", "1")]),
                Some(BackendName::Zellij),
                SelectionSource::Env,
            ),
        ];
        for (input, backend, source) in cases {
            let detected = detect_host(&input);
            assert_eq!(detected.backend, backend, "{input:?}");
            assert_eq!(detected.source, source, "{input:?}");
        }
    }

    #[test]
    fn empty_env_refuses_with_no_supported_host() {
        let runner = Arc::new(ProbeRunner::default());
        let error = select_backend_from_env(&BTreeMap::new(), runner, WorktreePolicy::Host)
            .err()
            .expect("empty env must refuse");
        assert!(error.downcast_ref::<NoSupportedHost>().is_some());
        assert_eq!(error.to_string(), NO_SUPPORTED_HOST);
    }

    #[test]
    fn named_backends_stay_exact_and_unknown_names_error() {
        let runner = Arc::new(ProbeRunner::default());
        assert_eq!(
            backend_by_name("herdr", runner.clone(), WorktreePolicy::Host)
                .unwrap()
                .name(),
            "herdr"
        );
        assert_eq!(
            backend_for_env(
                "zellij",
                &BTreeMap::new(),
                runner.clone(),
                WorktreePolicy::Host
            )
            .unwrap()
            .name(),
            "zellij"
        );
        assert_eq!(
            backend_for_env(
                "fake",
                &BTreeMap::new(),
                runner.clone(),
                WorktreePolicy::Host
            )
            .unwrap()
            .name(),
            "fake"
        );
        assert_eq!(
            backend_for_env(
                "exec",
                &BTreeMap::new(),
                runner.clone(),
                WorktreePolicy::Host
            )
            .unwrap()
            .name(),
            "exec"
        );
        assert_eq!(runner.calls().len(), 0);
        let error = backend_for_env("nope", &BTreeMap::new(), runner, WorktreePolicy::Host)
            .err()
            .expect("unknown name must error");
        assert_eq!(error.to_string(), "unknown session backend: nope");
    }

    #[test]
    fn headless_is_the_exec_alias() {
        assert_eq!(BackendName::parse("headless"), Some(BackendName::Exec));
        assert_eq!(BackendName::parse("HEADLESS"), Some(BackendName::Exec));
        assert_eq!(BackendName::parse("Headless"), Some(BackendName::Exec));
        assert_eq!(BackendName::parse("exec"), Some(BackendName::Exec));
        assert_eq!(BackendName::parse("EXEC"), Some(BackendName::Exec));
        assert_eq!(BackendName::Exec.as_str(), "exec");
        assert_eq!(BackendName::parse("nope"), None);

        let detected = detect_host(&env(&[("ONLYNE_BACKEND", "headless")]));
        assert_eq!(detected.backend, Some(BackendName::Exec));
        assert_eq!(detected.source, SelectionSource::Explicit);
        assert_eq!(detected.backend.unwrap().as_str(), "exec");

        let runner = Arc::new(ProbeRunner::default());
        assert_eq!(
            backend_by_name("headless", runner.clone(), WorktreePolicy::Host)
                .unwrap()
                .name(),
            "exec"
        );
        assert_eq!(
            backend_by_name("HEADLESS", runner.clone(), WorktreePolicy::Host)
                .unwrap()
                .name(),
            "exec"
        );
        assert_eq!(
            backend_for_env(
                "headless",
                &BTreeMap::new(),
                runner.clone(),
                WorktreePolicy::Host
            )
            .unwrap()
            .name(),
            "exec"
        );
        let error = backend_for_env("nope", &BTreeMap::new(), runner, WorktreePolicy::Host)
            .err()
            .expect("unknown name must error");
        assert_eq!(error.to_string(), "unknown session backend: nope");
    }

    #[test]
    fn env_probe_picks_herdr_before_orca() {
        let runner = Arc::new(ProbeRunner::default());
        let backend = select_backend_from_env(
            &env(&[
                ("HERDR_ENV", "1"),
                ("HERDR_SOCKET_PATH", "/tmp/herdr.sock"),
                ("ORCA_PANE_KEY", "tab:leaf"),
                ("ZELLIJ", "0"),
            ]),
            runner,
            WorktreePolicy::Host,
        )
        .unwrap();
        assert_eq!(backend.name(), "herdr");
    }

    #[test]
    fn placement_from_pane_count_matches_required_inputs() {
        let expected = [
            (0, SplitDirection::Right),
            (1, SplitDirection::Right),
            (2, SplitDirection::Down),
            (3, SplitDirection::Right),
            (4, SplitDirection::Down),
            (5, SplitDirection::Down),
        ];
        for (count, direction) in expected {
            let placement = PanePlacement::from_pane_count(count);
            assert_eq!(placement.direction, direction, "count {count}");
            assert_eq!(placement.ratio, 0.5);
        }
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

    /// Herdr answers a refusal with a JSON document on stderr and an empty
    /// stdout, so the code the backends branch on has to come from there.
    #[test]
    fn command_failure_reads_a_code_from_the_stderr_document() {
        let output = CommandOutput {
            status: 1,
            stdout: Vec::new(),
            stderr: br#"{"error":{"code":"agent_not_found","message":"agent target wF:p2 not found"},"id":"cli:agent:focus"}"#.to_vec(),
        };
        let failure = command_failure("herdr agent focus wF:p2", &output, None);
        assert_eq!(failure.code(), Some("agent_not_found"));
        assert_eq!(
            failure.to_string(),
            "runtime command failed: herdr agent focus wF:p2 (status 1, agent_not_found): agent target wF:p2 not found"
        );
    }

    /// A backend that answers in plain text keeps that text as the message, and
    /// carries no code.
    #[test]
    fn command_failure_keeps_plain_stderr_text() {
        let output = CommandOutput {
            status: 2,
            stdout: Vec::new(),
            stderr: b"unknown option: --bogus".to_vec(),
        };
        let failure = command_failure("herdr pane split", &output, None);
        assert_eq!(failure.code(), None);
        assert_eq!(
            failure.to_string(),
            "runtime command failed: herdr pane split (status 2): unknown option: --bogus"
        );
    }
}
