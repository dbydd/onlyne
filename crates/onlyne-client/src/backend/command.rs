use super::*;

pub(super) fn command_error(program: &str, output: &CommandOutput) -> anyhow::Error {
    anyhow::anyhow!(
        "runtime command failed: {program} (status {}): {}",
        output.status,
        String::from_utf8_lossy(&output.stderr).trim()
    )
}

pub(super) fn run_checked(
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
pub(super) fn run_json(
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
pub(super) fn command_failure(
    command: &str,
    output: &CommandOutput,
    body: Option<&Value>,
) -> CommandFailure {
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
pub(super) fn failure_code(error: &anyhow::Error) -> Option<&str> {
    error
        .downcast_ref::<CommandFailure>()
        .and_then(CommandFailure::code)
}

pub(super) fn unsupported(backend: &str, operation: &str, detail: &str) -> anyhow::Error {
    anyhow::anyhow!("runtime backend {backend} does not support {operation}: {detail}")
}
